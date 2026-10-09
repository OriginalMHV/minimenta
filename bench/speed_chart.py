"""Draws the speed chart and the speed tables of the README from Speed workflow results.

Usage:
    gh run download RUN_ID -n speed-merged -D DIR
    python3 -I bench/speed_chart.py [--check] DIR/speed-merged.json [DIR2/speed-merged.json ...]

Download the artifact of each run into its own directory, then pass every
speed-merged.json file. The script writes two outputs:

- docs/assets/speed.svg, the chart of the first scan of a cold disk.
- The tables in README.md between <!-- speed-tables:begin --> and
  <!-- speed-tables:end -->. The script changes nothing else in the README.

With --check, the script writes nothing. It exits with 1 when an output differs.

How the chart values come from the data:

1. The chart shows the first cold tree of each platform.
2. The ratio of a tool is the median over the alternating pairs of the time of
   the tool divided by the time of minimenta. The data files hold it as ratio_median.
3. The baseline is ncdu with its default settings. Windows has no ncdu, so gdu is
   the baseline there.
4. In one run, the value of a tool is the ratio of the baseline divided by the
   ratio of the tool. The value of minimenta is the ratio of the baseline.
5. With several runs, the value is the median of the values of the runs.
6. The chart shows one decimal. An exact tie rounds against minimenta: the value of
   minimenta rounds down and the value of every other tool rounds up.
7. A bar has a number of # characters in proportion to the shown value. The longest
   bar fills the bracket. All bars use one scale.

In the tables, a tie rounds down for the ratio and for the bounds of the middle
half, and in the direction that is worse for minimenta for times. With several
runs, a table value is the median of the values of the runs, and the pairs add up.
"""

import argparse
import json
import sys
from fractions import Fraction
from xml.sax.saxutils import escape

BEGIN = "<!-- speed-tables:begin -->"
END = "<!-- speed-tables:end -->"
PLATFORMS = (("macos", "macOS"), ("linux", "Linux"), ("windows", "Windows"))
BASELINES = ("ncdu", "gdu")
CHART_LABELS = {
    "ncdu": "ncdu",
    "ncdu-fast": "ncdu -t 64",
    "gdu": "gdu",
    "dua": "dua-cli",
    "dust": "dust*",
    "minimenta-no-mft": "minimenta (no admin)",
}
CELLS = 24
HALF = Fraction(1, 2)
DOWN, UP = "down", "up"

WIDTH = 760
MARGIN = 40
FONT_SIZE = 20
CHAR_WIDTH = 12
ROW_PITCH = 28
LABEL_X = MARGIN
VALUE_X = 368
BAR_X = 392
SANS = "Inter, 'Segoe UI', Helvetica, Arial, sans-serif"
STYLE = (
    ".fg{fill:#1F2328}.mu{fill:#59636E}.ac{fill:#1A7F5A}.bar{fill:#8C959F}"
    "@media (prefers-color-scheme: dark){.fg{fill:#F0F6FC}.mu{fill:#9198A1}.ac{fill:#44B78F}.bar{fill:#6E7681}}"
)


def die(message):
    sys.exit(f"speed_chart: {message}")


def median(values):
    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def scaled(value, places, ties):
    """The value as a whole number of 10^-places. An exact tie goes down or up."""
    exact = value * 10**places
    whole = exact.numerator // exact.denominator
    rest = exact - whole
    if rest > HALF or (rest == HALF and ties == UP):
        whole += 1
    return whole


def show(whole, places):
    digits = str(whole).rjust(places + 1, "0")
    return f"{digits[:-places]}.{digits[-places:]}" if places else digits


def fixed(value, places, ties):
    return show(scaled(value, places, ties), places)


class Run:
    def __init__(self, path):
        with open(path, encoding="utf-8") as f:
            self.doc = json.load(f, parse_float=Fraction)
        doc = self.doc
        if doc.get("schema") != 1 or "rows" not in doc or "platforms" not in doc:
            die(f"{path} is not a speed-merged.json file of schema 1")
        self.path = path
        self.id = str(doc["run_id"])
        self.date = str(doc["date"])[:10]
        self.groups = {}
        self.trees = {name: [] for name, _ in PLATFORMS}
        for row in doc["rows"]:
            key = (row["platform"], row["mode"], row["tree"])
            if key not in self.groups:
                self.groups[key] = {}
                self.trees.setdefault(row["platform"], []).append(key)
            self.groups[key][row["tool"]] = row
        self.platforms = doc["platforms"]


class Series:
    """The rows of one tool on one tree in every run that has them."""

    def __init__(self, rows):
        self.rows = rows
        self.first = rows[0]

    def median(self, field):
        return median([row[field] for row in self.rows])

    def total(self, field):
        return sum(row[field] for row in self.rows)


def load(paths):
    runs = [Run(path) for path in paths]
    seen = set()
    for run in runs:
        if run.id in seen:
            die(f"run {run.id} appears twice")
        seen.add(run.id)
        for platform in run.doc.get("missing_platforms") or []:
            print(f"speed_chart: run {run.id} has no data for {platform}", file=sys.stderr)
    return runs


def trees_of(runs, platform):
    keys = []
    for run in runs:
        for key in run.trees.get(platform, []):
            if key not in keys:
                keys.append(key)
    return keys


def series_of(runs, key):
    """Maps each tool of a tree to its Series, in the order of the data."""
    by_tool = {}
    for run in runs:
        for tool, row in run.groups.get(key, {}).items():
            by_tool.setdefault(tool, []).append(row)
    return {tool: Series(rows) for tool, rows in by_tool.items()}


def baseline_of(group):
    for tool in BASELINES:
        if tool in group and not group[tool]["diagnostic"]:
            return tool
    die("no baseline tool (ncdu or gdu) in a cold tree")


def speed_values(runs, key):
    """Maps each tool to its exact speed relative to the baseline, and names the baseline."""
    baseline = baseline_of(next(run.groups[key] for run in runs if key in run.groups))
    values = {}
    for run in runs:
        group = run.groups.get(key)
        if not group or baseline not in group:
            continue
        base = Fraction(group[baseline]["ratio_median"])
        values.setdefault("minimenta", []).append(base)
        for tool, row in group.items():
            if not row["diagnostic"]:
                values.setdefault(tool, []).append(base / row["ratio_median"])
    return baseline, {tool: median(each) for tool, each in values.items()}


def count_range(values, unit=""):
    low, high = min(values), max(values)
    text = f"{low:,}" if low == high else f"{low:,} to {high:,}"
    return f"{text} {unit}".strip()


def tree_path(platform, path):
    return path.replace("/", "\\") if platform == "windows" else path


class ChartRow:
    def __init__(self, label, kind, shown, cells):
        self.label, self.kind, self.shown, self.cells = label, kind, shown, cells

    @property
    def text(self):
        return f"{show(self.shown, 1)}x"


def build_chart(runs):
    """Returns the platform blocks of the chart and the footnote facts."""
    blocks = []
    for platform, name in PLATFORMS:
        cold = [key for key in trees_of(runs, platform) if key[1] == "cold"]
        if not cold:
            continue
        key = cold[0]
        series = series_of(runs, key)
        baseline, values = speed_values(runs, key)
        has_ncdu = "ncdu" in series
        no_admin = "minimenta-no-mft" in series
        hero_label = "minimenta (admin)" if no_admin else "minimenta"
        others = [(tool, value) for tool, value in values.items() if tool != "minimenta"]
        others.sort(key=lambda pair: -pair[1])
        entries = [(hero_label, "hero", scaled(values["minimenta"], 1, DOWN))]
        for tool, value in others:
            label = CHART_LABELS.get(tool, series[tool].first["label"])
            kind = "variant" if tool.startswith("minimenta") else "other"
            entries.append((label, kind, scaled(value, 1, UP)))
        note = None
        if not has_ncdu:
            note = f"Relative to {series[baseline].first['label']}. ncdu does not run on {name}."
        runner_cores = sorted({run.platforms[platform]["runner"]["cores"] for run in runs if platform in run.platforms})
        facts = {
            "name": name,
            "cores": count_range(runner_cores, "cores") if len(runner_cores) > 1 else f"{runner_cores[0]} cores",
            "path": tree_path(platform, series[baseline].first["path"]),
            "items": count_range([row["items"] for row in series[baseline].rows], "items"),
        }
        blocks.append({"platform": platform, "name": name, "key": key, "baseline": baseline, "entries": entries, "note": note, "facts": facts})
    if not blocks:
        die("no cold tree in the data")
    longest = max(whole for block in blocks for _, _, whole in block["entries"])
    for block in blocks:
        rows = []
        for label, kind, whole in block["entries"]:
            cells = min(CELLS, max(1, scaled(Fraction(whole, longest) * CELLS, 0, UP)))
            rows.append(ChartRow(label, kind, whole, cells))
        block["rows"] = rows
    return blocks


def date_range(runs):
    dates = sorted(run.date for run in runs)
    return dates[0] if dates[0] == dates[-1] else f"{dates[0]} to {dates[-1]}"


def footnotes(blocks, runs):
    count = len(runs)
    lines = [f"File cache dropped before every run. GitHub runners, {count} run{'s' if count != 1 else ''}, {date_range(runs)}."]
    facts = [f"{b['facts']['name']}: {b['facts']['cores']}, {b['facts']['path']}, {b['facts']['items']}." for b in blocks]
    if len(facts) > 1:
        lines.append(" ".join(facts[:2]))
        lines.extend(facts[2:])
    else:
        lines.extend(facts)
    lines.append("* dust prints a tree. It is not interactive.")
    return lines


def description(blocks):
    parts = []
    for block in blocks:
        values = ", ".join(f"{row.label.rstrip('*')} {row.text}" for row in block["rows"])
        parts.append(f"{block['name']}: {values}.")
    return "Speed relative to ncdu with its default settings. Longer is faster. " + " ".join(parts)


def render_svg(blocks, runs):
    notes = footnotes(blocks, runs)
    out = []
    y = 34
    out.append(f'<text x="{MARGIN}" y="{y}" font-family="{SANS}" font-size="18" font-weight="600" class="fg">First scan of a cold disk</text>')
    y += 26
    out.append(f'<text x="{MARGIN}" y="{y}" font-family="{SANS}" font-size="15" class="mu">Speed relative to ncdu with its default settings. Longer is faster.</text>')
    y += 40
    for block in blocks:
        heading = f'<tspan font-size="16" font-weight="600">{escape(block["name"])}</tspan>'
        if block["note"]:
            heading += f'<tspan dx="12" font-size="15">{escape(block["note"])}</tspan>'
        out.append(f'<text x="{MARGIN}" y="{y}" font-family="{SANS}" class="mu">{heading}</text>')
        y += 30
        for row in block["rows"]:
            text_class = "fg" if row.kind == "hero" else "mu"
            fill_class = "ac" if row.kind in ("hero", "variant") else "bar"
            filled = "#" * row.cells
            padding = " " * (CELLS - row.cells)
            out.append(f'<text x="{LABEL_X}" y="{y}" font-size="{FONT_SIZE}" class="{text_class}">{escape(row.label)}</text>')
            out.append(f'<text x="{VALUE_X}" y="{y}" font-size="{FONT_SIZE}" text-anchor="end" class="{text_class}">{row.text}</text>')
            out.append(
                f'<text x="{BAR_X}" y="{y}" font-size="{FONT_SIZE}" xml:space="preserve" class="mu">'
                f'[<tspan class="{fill_class}">{filled}</tspan>{padding}]</text>'
            )
            y += ROW_PITCH
        y += 10
    y += 8
    for line in notes:
        out.append(f'<text x="{MARGIN}" y="{y}" font-family="{SANS}" font-size="15" class="mu">{escape(line)}</text>')
        y += 22
    height = y - 22 + 14
    head = (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" viewBox="0 0 {WIDTH} {height}" '
        f"font-family=\"'JetBrains Mono', ui-monospace, SFMono-Regular, Menlo, Consolas, monospace\" "
        f'role="img" aria-labelledby="title desc">'
    )
    prefix = [
        head,
        f"<title id=\"title\">First scan of a cold disk</title><desc id=\"desc\">{escape(description(blocks))}</desc>",
        f"<style>{STYLE}</style>",
    ]
    return "\n".join(prefix + out + ["</svg>"]) + "\n"


def md_table(header, rows, align=None):
    align = align or ["---"] * len(header)
    lines = ["| " + " | ".join(header) + " |", "| " + " | ".join(align) + " |"]
    lines += ["| " + " | ".join(row) + " |" for row in rows]
    return lines


def time_text(ms, mode, ties):
    if mode == "cold":
        return f"{fixed(ms / 1000, 2, ties)} s"
    return f"{fixed(ms, 1, ties)} ms"


def verdict_text(series, ratio, q1, q3):
    label = series.first["label"]
    variant = series.first["tool"].startswith("minimenta")
    if q1 <= 1 <= q3:
        return "even"
    if ratio > 1:
        return "default scan faster" if variant else "minimenta faster"
    return f"{label} faster"


def runs_table(runs):
    rows = []
    for run in runs:
        for platform, name in PLATFORMS:
            info = run.platforms.get(platform)
            if not info:
                continue
            repo = info.get("repository", "OriginalMHV/minimenta")
            head = run.doc.get("pull_request_head") or run.doc.get("commit") or ""
            runner = info["runner"]
            admin = ("yes" if runner.get("elevated") else "no") if platform == "windows" else "n/a"
            rows.append(
                [
                    f"[{run.id}](https://github.com/{repo}/actions/runs/{run.id})",
                    run.date,
                    f"[`{head[:7]}`](https://github.com/{repo}/commit/{head})" if head else "n/a",
                    name,
                    runner["cpu"].replace("|", "/"),
                    str(runner["cores"]),
                    admin,
                ]
            )
    lines = ["**Runs**", ""]
    lines += md_table(["Run", "Date", "Measured commit", "Platform", "Runner CPU", "Cores", "Administrator"], rows, ["---", "---", "---", "---", "---", "---:", "---"])
    if any(run.doc.get("pull_request_head") for run in runs):
        lines += ["", "The measured commit of a pull request is the head of the pull request."]
    return lines


def tree_title(platform_name, key, series):
    _, mode, tree = key
    first = next(iter(series.values())).first
    where = "synthetic tree" if tree == "synthetic" else f"`{tree_path(key[0], first['path'])}`"
    what = "First scan of a cold disk" if mode == "cold" else "Scan with a warm cache"
    return f"**{what}, {platform_name}, {where}**"


def tree_table(runs, key, name, chart_keys):
    _, mode, _ = key
    series = series_of(runs, key)
    base_rows = next(iter(series.values()))
    items = count_range([row["items"] for row in base_rows.rows], "items")
    lines = [tree_title(name, key, series), ""]
    intro = [items]
    speed = None
    if mode == "cold":
        baseline, speed = speed_values(runs, key)
        intro.append(f"The speed value is relative to {series[baseline].first['label']}")
        if key in chart_keys:
            intro.append("This tree is in the chart")
    elif key[2] == "synthetic":
        intro.append("From bench/gen_tree.py")
    lines.append(". ".join(intro) + ".")
    lines.append("")
    header = ["Tool", "Version", "Tool median", "minimenta median", "Ratio", "Middle half", "Pairs (minimenta faster / tool faster)", "Result"]
    align = ["---", "---", "---:", "---:", "---:", "---", "---", "---"]
    if speed:
        header.append("Speed vs baseline")
        align.append("---:")
    rows = []
    for tool, each in series.items():
        ratio = each.median("ratio_median")
        q1, q3 = each.median("ratio_q1"), each.median("ratio_q3")
        versions = sorted({row["version"] for row in each.rows})
        label = each.first["label"]
        if each.first["diagnostic"]:
            label += " (setting check)"
        row = [
            label,
            " / ".join(versions),
            time_text(each.median("median_ms"), mode, DOWN),
            time_text(each.median("minimenta_median_ms"), mode, UP),
            fixed(ratio, 3, DOWN),
            f"{fixed(q1, 2, DOWN)} to {fixed(q3, 2, DOWN)}",
            f"{each.total('pairs')} ({each.total('minimenta_faster_pairs')} / {each.total('tool_faster_pairs')})",
            verdict_text(each, ratio, q1, q3),
        ]
        if speed:
            if each.first["diagnostic"]:
                row.append("n/a")
            else:
                row.append(f"{fixed(speed[tool], 1, UP)}x" + (" (baseline)" if tool == baseline else ""))
        rows.append(row)
    lines += md_table(header, rows, align)
    if speed:
        lines += ["", f"The speed value of minimenta is {fixed(speed['minimenta'], 1, DOWN)}x. It is the ratio of the baseline."]
    return lines


def per_run_table(runs):
    if len(runs) < 2:
        return []
    header = ["Platform", "Tree", "Tool"] + [run.id for run in runs] + ["Median"]
    rows = []
    for platform, name in PLATFORMS:
        for key in trees_of(runs, platform):
            if key[1] != "cold":
                continue
            series = series_of(runs, key)
            for tool, each in series.items():
                if each.first["diagnostic"]:
                    continue
                cells = []
                for run in runs:
                    row = run.groups.get(key, {}).get(tool)
                    cells.append(fixed(row["ratio_median"], 3, DOWN) if row else "n/a")
                rows.append([name, f"`{tree_path(platform, each.first['path'])}`", each.first["label"]] + cells + [fixed(each.median("ratio_median"), 3, DOWN)])
    lines = ["**Ratio of the cold first scans in each run**", ""]
    lines += md_table(header, rows, ["---", "---", "---"] + ["---:"] * (len(runs) + 1))
    return lines


def render_tables(runs, blocks):
    chart_keys = {block["key"] for block in blocks}
    out = runs_table(runs)
    if len(runs) > 1:
        out += ["", f"{len(runs)} runs. Each value in the tables below is the median of the values of the runs. Pairs add up."]
    for mode in ("cold", "warm"):
        for platform, name in PLATFORMS:
            for key in trees_of(runs, platform):
                if key[1] == mode:
                    out += ["", *tree_table(runs, key, name, chart_keys)]
    per_run = per_run_table(runs)
    if per_run:
        out += ["", *per_run]
    return "\n".join(out)


def splice(text, block):
    if text.count(BEGIN) != 1 or text.count(END) != 1 or text.index(BEGIN) > text.index(END):
        die(f"the README needs exactly one {BEGIN} line before one {END} line")
    head, rest = text.split(BEGIN)
    _, tail = rest.split(END)
    return f"{head}{BEGIN}\n\n{block}\n\n{END}{tail}"


def read_text(path):
    with open(path, encoding="utf-8", newline="") as f:
        return f.read()


def write_text(path, text):
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.write(text)


def summary(blocks, runs):
    print(f"speed_chart: {len(runs)} run{'s' if len(runs) != 1 else ''}: {', '.join(run.id for run in runs)}")
    for block in blocks:
        values = ", ".join(f"{row.label.rstrip('*')} {row.text}" for row in block["rows"])
        print(f"  {block['name']} ({block['facts']['path']}): {values}")


def main():
    parser = argparse.ArgumentParser(description="Draws the speed chart and the speed tables from speed-merged.json files.")
    parser.add_argument("files", nargs="+", metavar="speed-merged.json")
    parser.add_argument("--svg", default="docs/assets/speed.svg")
    parser.add_argument("--readme", default="README.md")
    parser.add_argument("--check", action="store_true", help="write nothing, exit 1 when an output differs")
    args = parser.parse_args()
    runs = load(args.files)
    blocks = build_chart(runs)
    svg = render_svg(blocks, runs)
    readme = read_text(args.readme)
    updated = splice(readme, render_tables(runs, blocks))
    summary(blocks, runs)
    if args.check:
        stale = [path for path, old, new in ((args.svg, read_text(args.svg), svg), (args.readme, readme, updated)) if old != new]
        if stale:
            sys.exit(f"speed_chart: out of date: {', '.join(stale)}")
        print("speed_chart: outputs are up to date")
        return
    write_text(args.svg, svg)
    write_text(args.readme, updated)
    print(f"speed_chart: wrote {args.svg} and the tables in {args.readme}")


if __name__ == "__main__":
    main()
