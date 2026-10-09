"""Merges the JSON documents of bench/compare.py, one per platform, into one.

Usage: python -I bench/merge_speed.py OUT FILE...

The merged document keeps every platform document unchanged under
"platforms". It also has "rows", one flat row for each tool on each tree,
which is the form a chart needs. Each row names the run ID and the commit of
its platform, so every number traces to a run.
"""

import datetime
import json
import os
import sys

NOTES = {
    "ratio": "Time of the tool divided by the time of minimenta, for each pair of runs that alternate in order. "
    "ratio_median is the median of these ratios. A value above 1 means that minimenta is faster. "
    "A value below 1 means that the tool is faster.",
    "middle_half": "ratio_q1 to ratio_q3 is the middle half of the sorted ratios. With 3 pairs, it is the full range.",
    "verdict": "even when the middle half includes 1.0. Otherwise the tool that is faster in the median.",
    "diagnostic": "Rows with diagnostic true check a setting of a tool. They are not part of the comparison.",
}

ROW_FIELDS = (
    "tool", "name", "label", "version", "settings", "threads", "pairs", "median_ms", "minimenta_median_ms",
    "ratio_median", "ratio_q1", "ratio_q3", "verdict", "diagnostic",
)


def rows(doc):
    for tree in doc["trees"]:
        for result in tree["results"]:
            row = {
                "platform": doc["platform"],
                "run_id": doc.get("run_id"),
                "commit": doc.get("commit"),
                "tree": tree["id"],
                "path": tree["path"],
                "mode": tree["mode"],
                "items": tree["items"],
            }
            row.update({field: result[field] for field in ROW_FIELDS})
            row["total_bytes"] = tree["totals"].get(result["tool"] if result["tool"] in tree["totals"] else result["name"], {}).get("bytes")
            yield row


def markdown(merged):
    lines = ["### Speed on all platforms", ""]
    lines.append(f"Commit `{(merged['commit'] or '')[:12]}`, run {merged['run_id']}. Ratio is tool time divided by minimenta time. Above 1 means minimenta is faster.")
    keys = []
    for row in merged["rows"]:
        key = (row["platform"], row["mode"], row["tree"])
        if key not in keys:
            keys.append(key)
    for platform, mode, tree in keys:
        picked = [r for r in merged["rows"] if (r["platform"], r["mode"], r["tree"]) == (platform, mode, tree) and not r["diagnostic"]]
        lines += ["", f"**{platform}, {mode}, {tree}** `{picked[0]['path']}`, {picked[0]['items']} items", ""]
        lines.append("| Tool | Version | Median ms | Ratio | Middle half | Pairs | Result |")
        lines.append("| --- | --- | ---: | ---: | --- | ---: | --- |")
        for r in picked:
            lines.append(
                f"| {r['label']} | {r['version']} | {r['median_ms']} | {r['ratio_median']:.2f} | "
                f"{r['ratio_q1']:.2f} to {r['ratio_q3']:.2f} | {r['pairs']} | {r['verdict']} |"
            )
    return "\n".join(lines) + "\n"


def main():
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    out, files = sys.argv[1], sys.argv[2:]
    platforms = {}
    for path in files:
        with open(path, encoding="utf-8") as f:
            doc = json.load(f)
        if doc["platform"] in platforms:
            sys.exit(f"merge_speed: two documents for {doc['platform']}")
        platforms[doc["platform"]] = doc
    missing = [p for p in ("linux", "macos", "windows") if p not in platforms]
    docs = list(platforms.values())
    if not docs:
        sys.exit("merge_speed: no input")
    merged = {
        "schema": 1,
        "date": max(d.get("finished") or d["date"] for d in docs),
        "commit": docs[0].get("commit"),
        "run_id": docs[0].get("run_id"),
        "pull_request_head": docs[0].get("pull_request_head"),
        "ref": docs[0].get("ref"),
        "missing_platforms": missing,
        "notes": NOTES,
        "platforms": {name: platforms[name] for name in sorted(platforms)},
        "rows": [row for name in sorted(platforms) for row in rows(platforms[name])],
    }
    for d in docs:
        if d.get("commit") != merged["commit"] or d.get("run_id") != merged["run_id"]:
            merged.setdefault("warnings", []).append(f"{d['platform']} comes from another commit or run")
    with open(out, "w", encoding="utf-8") as f:
        json.dump(merged, f, indent=1)
        f.write("\n")
    print("===== MINIMENTA SPEED MERGED BEGIN =====")
    print(json.dumps(merged, indent=1))
    print("===== MINIMENTA SPEED MERGED END =====")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write(markdown(merged))
    if missing:
        sys.exit(f"merge_speed: missing platforms: {', '.join(missing)}")
    _ = datetime


if __name__ == "__main__":
    main()
