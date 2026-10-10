"""Compares minimenta with ncdu, gdu, dua-cli and dust, and writes JSON.

Every tool runs in alternating pairs with minimenta, as in bench/interleave.py.
The ratio of a pair is tool time divided by minimenta time. A ratio above 1
means that minimenta is faster. A ratio below 1 means that the tool is faster.
A machine that slows down for a while slows both commands of a pair, so the
median ratio stays fair on a shared runner.

A cold section clears the file cache before every run. A warm section reads a
tree that the file cache already holds. Before the timed pairs, the script
runs every tool once to measure its single-run time. It uses these times to
estimate how many pairs fit in the time budget of the section. The rounds then
stop at the maximum number of pairs, or when the next round does not fit in the
budget. A section always runs the minimum number of pairs.

Before it times anything, the script also runs every tool once on each tree,
and compares the totals. A tool that reports a far lower total than
minimenta may skip work, so the script marks that total.

With --job N/M, the document is job N of M jobs that run the same comparison
on different runners. bench/merge_speed.py pools the pairs of these jobs.

Usage: python -I bench/compare.py --out FILE --tools-dir DIR \\
           [--cold ID=PATH]... [--warm ID=PATH]... [--minimenta EXE] [--job N/M]
"""

import argparse
import datetime
import json
import os
import platform
import re
import shlex
import statistics
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
PLATFORMS = {"linux": "linux", "darwin": "macos", "win32": "windows"}
PLAT = PLATFORMS.get(sys.platform, sys.platform)
UNITS = {"B": 1, "KiB": 1024, "MiB": 1024**2, "GiB": 1024**3, "TiB": 1024**4, "PiB": 1024**5}
UNIT = "|".join(UNITS)
SCHEMA = 1
TOLERANCE = 0.05


def log(*parts):
    print(*parts, flush=True)


class Tool:
    """One command line. The tree path goes last."""

    def __init__(self, tool_id, name, label, exe, args, check, diagnostic=False, threads="default", key=None):
        self.id = tool_id
        self.name = name
        self.label = label
        self.exe = exe
        self.args = args
        self.check = check
        self.key = key or tool_id
        self.diagnostic = diagnostic
        self.threads = threads
        self.version = None
        self.version_line = None

    def argv(self, path):
        return [self.exe, *self.args, path]

    @property
    def settings(self):
        shown = [os.path.basename(self.exe).removesuffix(".exe"), *self.args, "PATH"]
        return " ".join(shlex.quote(part) for part in shown)


def capture(argv, timeout=1800):
    done = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout)
    return done.returncode, done.stdout.decode("utf-8", "replace"), done.stderr.decode("utf-8", "replace")


def to_bytes(value, unit):
    return round(float(value) * UNITS[unit])


def first_line(text):
    return next((line.strip() for line in text.splitlines() if line.strip()), "")


# Each check runs a tool once and returns the total it reports, or raises.
# It runs outside the timed pairs. The timed commands print listings or
# nothing, so a check uses the option of the tool that prints a total.


def check_minimenta(tool, path):
    code, out, err = capture(tool.argv(path))
    found = re.search(
        rf"([\d.]+)\s+({UNIT})\s+disk,\s*([\d.]+)\s+({UNIT})\s+apparent,\s*(\d+) items,\s*(\d+) errors", out
    )
    if not found:
        raise ValueError(f"cannot read the minimenta summary: {out!r} {err!r}")
    disk = to_bytes(found[1], found[2])
    return {
        "bytes": disk,
        "disk_bytes": disk,
        "apparent_bytes": to_bytes(found[3], found[4]),
        "items": int(found[5]),
        "errors": int(found[6]),
        "precision": "one decimal of the printed unit",
    }


def check_du(_tool, path):
    if PLAT == "windows":
        return None
    _, out, _ = capture(["du", "-sk", path])
    disk = int(out.split()[0]) * 1024
    if PLAT == "linux":
        _, out, _ = capture(["du", "-sb", path])
        apparent = int(out.split()[0])
    else:
        _, out, _ = capture(["du", "-skA", path])
        apparent = int(out.split()[0]) * 1024
    return {"bytes": disk, "disk_bytes": disk, "apparent_bytes": apparent, "precision": "exact (1 KiB blocks on macOS apparent size)"}


def ncdu_sum(node, dev, seen):
    head = node[0]
    dev = head.get("dev", dev)
    apparent, disk = head.get("asize", 0), head.get("dsize", 0)
    for child in node[1:]:
        if isinstance(child, list):
            a, d = ncdu_sum(child, dev, seen)
            apparent += a
            disk += d
            continue
        if child.get("hlnkc"):
            key = (child.get("dev", dev), child.get("ino"))
            if key in seen:
                continue
            seen.add(key)
        apparent += child.get("asize", 0)
        disk += child.get("dsize", 0)
    return apparent, disk


def check_ncdu(tool, path):
    with tempfile.TemporaryDirectory() as folder:
        export = os.path.join(folder, "ncdu.json")
        code, _, err = capture([tool.exe, "-0", "-o", export, path])
        with open(export, encoding="utf-8") as f:
            data = json.load(f)
    apparent, disk = ncdu_sum(data[3], 0, set())
    return {"bytes": disk, "apparent_bytes": apparent, "precision": "exact, sum of the JSON export"}


def check_gdu(tool, path):
    _, out, err = capture([tool.exe, "-n", "-p", "-c", "-s", path])
    found = re.search(rf"^\s*([\d.]+)\s+({UNIT})\s", out, re.M)
    if not found:
        raise ValueError(f"cannot read the gdu total: {out!r} {err!r}")
    return {"bytes": to_bytes(found[1], found[2]), "precision": "one decimal of the printed unit"}


def check_dua(tool, path):
    _, out, err = capture([tool.exe, "-f", "bytes", path])
    found = re.search(r"(\d+)\s*b\s+total", out)
    if not found:
        raise ValueError(f"cannot read the dua total: {out!r} {err!r}")
    return {"bytes": int(found[1]), "precision": "exact"}


def check_dust(tool, path):
    _, out, err = capture([tool.exe, "-d", "0", "-P", "-b", "-c", "-o", "b", path])
    found = re.search(r"(\d+)B\s", out)
    if not found:
        raise ValueError(f"cannot read the dust total: {out!r} {err!r}")
    return {"bytes": int(found[1]), "precision": "exact"}


def exe_path(tools_dir, name):
    return os.path.join(tools_dir, "bin", name + (".exe" if PLAT == "windows" else ""))


def build_tools(args, mode, cores):
    mm = args.minimenta
    tools = []
    if PLAT == "windows":
        tools.append(Tool("minimenta-no-mft", "minimenta", "minimenta --no-mft", mm, ["--summary", "--no-mft"], "minimenta"))
    else:
        tools.append(Tool("ncdu", "ncdu", "ncdu (defaults)", exe_path(args.tools_dir, "ncdu"), ["-0", "-o", os.devnull], "ncdu", threads="1", key="ncdu"))
        fast = "64" if mode == "cold" else str(cores)
        tools.append(Tool("ncdu-fast", "ncdu", f"ncdu -t {fast}", exe_path(args.tools_dir, "ncdu"), ["-0", "-t", fast, "-O", os.devnull], "ncdu", threads=fast, key="ncdu"))
        if mode == "warm":
            # These two rows show whether the export format changes the time of ncdu.
            tools.append(Tool("ncdu-binary-export", "ncdu", "ncdu (defaults, binary export)", exe_path(args.tools_dir, "ncdu"), ["-0", "-O", os.devnull], "ncdu", True, "1", "ncdu"))
            tools.append(Tool("ncdu-fast-json-export", "ncdu", f"ncdu -t {fast} (JSON export)", exe_path(args.tools_dir, "ncdu"), ["-0", "-t", fast, "-o", os.devnull], "ncdu", True, fast, "ncdu"))
    tools.append(Tool("gdu", "gdu", "gdu", exe_path(args.tools_dir, "gdu"), ["-n", "-p", "-c"], "gdu"))
    tools.append(Tool("dua", "dua", "dua-cli", exe_path(args.tools_dir, "dua"), [], "dua"))
    tools.append(Tool("dust", "dust", "dust", exe_path(args.tools_dir, "dust"), ["-d", "0", "-P", "-b", "-c"], "dust"))
    return tools


CHECKS = {
    "minimenta": check_minimenta,
    "ncdu": check_ncdu,
    "gdu": check_gdu,
    "dua": check_dua,
    "dust": check_dust,
}


def read_version(tool):
    code, out, err = capture([tool.exe, "--version"], timeout=60)
    text = out + err
    found = re.search(r"\d+\.\d+(?:\.\d+)?", text)
    tool.version = found[0] if found else None
    tool.version_line = first_line(text)


def purge_windows():
    """Empties all working sets and the standby list, as bench/windows-purge.ps1
    does, but inside this process. The PowerShell script compiles C# on every
    call, which takes about 7 s on a runner. Needs an administrator."""
    import ctypes
    from ctypes import wintypes

    advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    ntdll = ctypes.WinDLL("ntdll")

    class Luid(ctypes.Structure):
        _fields_ = [("low", wintypes.DWORD), ("high", wintypes.LONG)]

    class LuidAndAttributes(ctypes.Structure):
        _fields_ = [("luid", Luid), ("attributes", wintypes.DWORD)]

    class TokenPrivileges(ctypes.Structure):
        _fields_ = [("count", wintypes.DWORD), ("privileges", LuidAndAttributes * 1)]

    kernel32.GetCurrentProcess.restype = wintypes.HANDLE
    advapi32.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(wintypes.HANDLE)]
    advapi32.LookupPrivilegeValueW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR, ctypes.POINTER(Luid)]
    advapi32.AdjustTokenPrivileges.argtypes = [
        wintypes.HANDLE, wintypes.BOOL, ctypes.POINTER(TokenPrivileges), wintypes.DWORD, ctypes.c_void_p, ctypes.c_void_p,
    ]
    ntdll.NtSetSystemInformation.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_ulong]
    ntdll.NtSetSystemInformation.restype = ctypes.c_ulong

    token = wintypes.HANDLE()
    if not advapi32.OpenProcessToken(kernel32.GetCurrentProcess(), 0x0028, ctypes.byref(token)):
        raise ctypes.WinError(ctypes.get_last_error())
    luid = Luid()
    if not advapi32.LookupPrivilegeValueW(None, "SeProfileSingleProcessPrivilege", ctypes.byref(luid)):
        raise ctypes.WinError(ctypes.get_last_error())
    state = TokenPrivileges(1, (LuidAndAttributes * 1)(LuidAndAttributes(luid, 2)))
    ctypes.set_last_error(0)
    if not advapi32.AdjustTokenPrivileges(token, False, ctypes.byref(state), 0, None, None) or ctypes.get_last_error():
        raise ctypes.WinError(ctypes.get_last_error())
    system_memory_list_information = 80
    for command in (2, 4):  # 2 empties the working sets, 4 purges the standby list
        value = ctypes.c_int(command)
        status = ntdll.NtSetSystemInformation(system_memory_list_information, ctypes.byref(value), 4)
        if status:
            raise OSError(f"NtSetSystemInformation({command}) failed: 0x{status:X}")


def purge_command():
    override = os.environ.get("COMPARE_PURGE_COMMAND")
    if override:
        return {"shell": True, "cmd": override, "text": override}
    if PLAT == "linux":
        return {"shell": True, "cmd": "sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null", "text": "sync; echo 3 > /proc/sys/vm/drop_caches"}
    if PLAT == "macos":
        return {"shell": True, "cmd": "sync; sudo purge", "text": "sync; sudo purge"}
    return {"shell": False, "cmd": None, "text": "empty all working sets, then purge the standby list (as bench/windows-purge.ps1)"}


class Runner:
    def __init__(self, timeout):
        self.purge = purge_command()
        self.timeout = timeout

    def drop_cache(self):
        start = time.perf_counter()
        if self.purge["cmd"] is None:
            try:
                purge_windows()
            except Exception as e:
                log(f"   in-process purge failed ({e}), using bench/windows-purge.ps1")
                self.purge = {
                    "shell": False,
                    "text": "bench/windows-purge.ps1 (empty working sets, purge standby list)",
                    "cmd": ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", os.path.join(HERE, "windows-purge.ps1")],
                }
                subprocess.run(self.purge["cmd"], check=True, stdout=subprocess.DEVNULL)
        else:
            subprocess.run(self.purge["cmd"], shell=self.purge["shell"], check=True, stdout=subprocess.DEVNULL)
        return time.perf_counter() - start

    def timed(self, argv, cold, keep_stderr=False):
        """Runs a command and returns (seconds, exit code, stderr text).

        Popen.wait(timeout) polls with sleeps of up to 50 ms on POSIX, which
        rounds every time up to a multiple of 50 ms. So the wait has no timeout
        and a timer kills a command that runs too long."""
        if cold:
            self.drop_cache()
        err = subprocess.PIPE if keep_stderr else subprocess.DEVNULL
        expired = []
        start = time.perf_counter()
        proc = subprocess.Popen(argv, stdout=subprocess.DEVNULL, stderr=err)
        killer = threading.Timer(self.timeout, lambda: (expired.append(True), proc.kill()))
        killer.start()
        try:
            _, stderr = proc.communicate()
            elapsed = time.perf_counter() - start
        finally:
            killer.cancel()
        if expired:
            raise subprocess.TimeoutExpired(argv, self.timeout)
        return elapsed, proc.returncode, (stderr or b"").decode("utf-8", "replace")


def quartiles(values):
    ordered = sorted(values)
    n = len(ordered)
    return ordered[n // 4], ordered[3 * n // 4]


def verdict(median, q1, q3):
    if q1 <= 1.0 <= q3:
        return "even"
    return "minimenta" if median > 1.0 else "tool"


def summarize(tool, tm, tt, codes, estimated):
    ratios = [t / m for t, m in zip(tt, tm)]
    q1, q3 = quartiles(ratios)
    median = statistics.median(ratios)
    return {
        "tool": tool.id,
        "name": tool.name,
        "label": tool.label,
        "diagnostic": tool.diagnostic,
        "version": tool.version,
        "settings": tool.settings,
        "threads": tool.threads,
        "pairs": len(ratios),
        "estimated_pairs": estimated,
        "minimenta_faster_pairs": sum(r > 1.0 for r in ratios),
        "tool_faster_pairs": sum(r < 1.0 for r in ratios),
        "median_ms": round(statistics.median(tt) * 1000, 1),
        "minimenta_median_ms": round(statistics.median(tm) * 1000, 1),
        "ratio_median": round(median, 3),
        "ratio_q1": round(q1, 3),
        "ratio_q3": round(q3, 3),
        "verdict": verdict(median, q1, q3),
        "times_ms": [round(t * 1000, 1) for t in tt],
        "minimenta_times_ms": [round(t * 1000, 1) for t in tm],
        "ratios": [round(r, 3) for r in ratios],
        "exit_codes": sorted(set(codes)),
    }


def judge_totals(totals):
    """Compares each total with the minimenta total. A tool whose total is lower
    than the disk and the apparent total of minimenta by more than 5 percent
    (and 64 KiB) may skip work, so ok is false. A higher total only sets
    higher, for example when a tool also counts the size of directories."""
    mm = totals.get("minimenta")
    if not mm:
        return
    for key, entry in totals.items():
        if key == "minimenta":
            entry.update(measure="disk", deviation_pct=0.0, ok=True, higher=False)
            continue
        options = {"disk": mm["disk_bytes"], "apparent": mm["apparent_bytes"]}
        measure = min(options, key=lambda name: abs(entry["bytes"] - options[name]))
        reference = options[measure]
        difference = entry["bytes"] - reference
        slack = max(TOLERANCE * reference, 64 * 1024)
        entry["measure"] = measure
        entry["deviation_pct"] = round(difference / reference * 100, 2) if reference else None
        entry["ok"] = difference >= -slack
        entry["higher"] = difference > slack


def run_checks(tools, mm, path):
    wanted = {"minimenta": mm}
    for tool in tools:
        wanted.setdefault(tool.key, tool)
    totals, errors = {}, {}
    for key, tool in wanted.items():
        try:
            totals[key] = CHECKS[tool.check](tool, path)
        except Exception as e:
            errors[key] = f"{type(e).__name__}: {e}"
            log(f"   check failed for {key}: {errors[key]}")
    if "minimenta" not in totals:
        raise RuntimeError(f"minimenta cannot scan {path}: {errors['minimenta']}")
    try:
        du = check_du(None, path)
        if du:
            totals["du"] = du
    except Exception as e:
        log(f"   du failed: {type(e).__name__}: {e}")
    judge_totals(totals)
    return totals, errors


def run_section(args, runner, mode, tree_id, path, cores):
    cold = mode == "cold"
    mm = Tool("minimenta", "minimenta", "minimenta", args.minimenta, ["--summary"], "minimenta")
    read_version(mm)
    mm.threads = str(max(cores, 16))
    tools = build_tools(args, mode, cores)
    if args.allow_missing:
        tools = [t for t in tools if t.name == "minimenta" or os.path.exists(t.exe)]
    for tool in tools:
        if tool.name != "minimenta":
            read_version(tool)
        else:
            tool.version = mm.version
            tool.threads = mm.threads
    log(f"== {mode} {tree_id}: {path}")
    section_started = time.perf_counter()
    section = {"id": tree_id, "path": path, "mode": mode, "budget_s": args.cold_budget if cold else args.warm_budget}

    log("-- totals (not timed)")
    totals, errors = run_checks(tools, mm, path)
    for key, entry in totals.items():
        shown = "n/a" if entry["deviation_pct"] is None else f"{entry['deviation_pct']:+.2f}%"
        log(f"   {key}: {entry['bytes']} bytes ({entry['measure']}, {shown} against minimenta)" + ("" if entry["ok"] else "  <-- LOWER, CHECK") + ("  (higher)" if entry["higher"] else ""))
    section["items"] = totals.get("minimenta", {}).get("items")
    section["errors"] = totals.get("minimenta", {}).get("errors")
    section["totals"] = totals
    section["total_errors"] = errors
    section["totals_ok"] = all(entry["ok"] for entry in totals.values()) and not errors

    # The budget covers the timed part. The totals above run once and also read the tree for the first time.
    started = time.perf_counter()
    log("-- single runs (for the plan)")
    purge_s = runner.drop_cache() if cold else 0.0
    singles = {}
    for tool in [mm, *tools]:
        elapsed, code, err = runner.timed(tool.argv(path), cold, keep_stderr=True)
        singles[tool.id] = elapsed
        log(f"   {tool.id}: {elapsed:.3f} s (exit {code})")
        if code != 0:
            log(f"   note: {tool.id} exited with code {code}: {first_line(err)}")

    mm_single = singles["minimenta"]
    warmup = 0 if cold else 2
    round_cost = sum((2 * purge_s if cold else 0) + mm_single + singles[t.id] for t in tools)
    spent = time.perf_counter() - started
    left = max(section["budget_s"] - spent, 0)
    limit = args.max_pairs if cold else args.max_warm_pairs
    estimated = int(left / round_cost) - warmup if round_cost else limit
    estimated = max(args.min_pairs, min(limit, estimated))
    log(f"-- plan: about {estimated} pairs per tool, one round costs about {round_cost:.1f} s, {left:.0f} s left in the section budget")
    log(f"   The rounds stop at {limit} pairs, or when the next round does not fit in the budget (after at least {args.min_pairs}).")
    section["plan"] = {
        "single_run_s": {k: round(v, 3) for k, v in singles.items()},
        "purge_s": round(purge_s, 3),
        "round_cost_s": round(round_cost, 1),
        "estimated_pairs": estimated,
        "max_pairs": limit,
    }

    for _ in range(warmup):
        for tool in tools:
            runner.timed(mm.argv(path), cold)
            runner.timed(tool.argv(path), cold)

    times = {t.id: ([], [], []) for t in tools}
    active = list(tools)
    loop_started = time.perf_counter()
    loop_budget = max(section["budget_s"] - (loop_started - started), 0)
    for r in range(limit):
        for i, tool in enumerate(list(active)):
            tm_list, tt_list, codes = times[tool.id]
            try:
                if (r + i) % 2 == 0:
                    tm = runner.timed(mm.argv(path), cold)[0]
                    tt, code, _ = runner.timed(tool.argv(path), cold)
                else:
                    tt, code, _ = runner.timed(tool.argv(path), cold)
                    tm = runner.timed(mm.argv(path), cold)[0]
            except subprocess.TimeoutExpired:
                log(f"   {tool.id} timed out and leaves the section")
                active.remove(tool)
                continue
            tm_list.append(tm)
            tt_list.append(tt)
            codes.append(code)
            log(f"   round {r + 1} {tool.id}: minimenta {tm:.3f} s, tool {tt:.3f} s, ratio {tt / tm:.2f}")
        done = r + 1
        spent_in_loop = time.perf_counter() - loop_started
        if done >= args.min_pairs and spent_in_loop + spent_in_loop / done > loop_budget:
            log(f"-- time budget reached after {done} pairs")
            break

    section["results"] = [
        summarize(tool, *times[tool.id][:2], times[tool.id][2], estimated) for tool in tools if times[tool.id][0]
    ]
    section["elapsed_s"] = round(time.perf_counter() - section_started, 1)
    section["purge"] = runner.purge["text"] if cold else None
    return section


def memory_gib():
    try:
        if PLAT == "linux":
            with open("/proc/meminfo") as f:
                return round(int(f.readline().split()[1]) / 1024**2, 1)
        if PLAT == "macos":
            return round(int(capture(["sysctl", "-n", "hw.memsize"])[1]) / 1024**3, 1)
        import ctypes

        class Status(ctypes.Structure):
            _fields_ = [("length", ctypes.c_ulong), ("load", ctypes.c_ulong), ("total", ctypes.c_ulonglong)] + [
                (f"f{i}", ctypes.c_ulonglong) for i in range(6)
            ]

        status = Status()
        status.length = ctypes.sizeof(Status)
        ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(status))
        return round(status.total / 1024**3, 1)
    except Exception:
        return None


def cpu_name():
    try:
        if PLAT == "linux":
            with open("/proc/cpuinfo") as f:
                return next(line.split(":", 1)[1].strip() for line in f if line.startswith("model name"))
        if PLAT == "macos":
            return capture(["sysctl", "-n", "machdep.cpu.brand_string"])[1].strip()
    except Exception:
        pass
    return platform.processor() or None


def pull_request_head():
    path = os.environ.get("GITHUB_EVENT_PATH")
    try:
        with open(path, encoding="utf-8") as f:
            event = json.load(f)
        return event["pull_request"]["head"]["sha"], event["pull_request"]["number"]
    except Exception:
        return None, None


def environment(args, cores):
    head, number = pull_request_head()
    env = os.environ
    doc = {
        "schema": SCHEMA,
        "platform": PLAT,
        "date": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "commit": env.get("GITHUB_SHA"),
        "pull_request_head": head,
        "pull_request": number,
        "ref": env.get("GITHUB_REF"),
        "event": env.get("GITHUB_EVENT_NAME"),
        "repository": env.get("GITHUB_REPOSITORY"),
        "run_id": env.get("GITHUB_RUN_ID"),
        "run_attempt": env.get("GITHUB_RUN_ATTEMPT"),
        "job": env.get("GITHUB_JOB"),
        "runner": {
            "name": env.get("RUNNER_NAME"),
            "image": env.get("ImageOS"),
            "image_version": env.get("ImageVersion"),
            "os": platform.platform(),
            "arch": platform.machine(),
            "cores": cores,
            "memory_gib": memory_gib(),
            "cpu": cpu_name(),
            "elevated": elevated(),
        },
    }
    if args.job:
        doc["job_index"], doc["job_count"] = args.job
    return doc


def job_spec(text):
    try:
        index, count = (int(part) for part in text.split("/"))
    except ValueError:
        raise argparse.ArgumentTypeError(f"{text!r} is not N/M") from None
    if not 1 <= index <= count:
        raise argparse.ArgumentTypeError(f"{text!r} needs 1 <= N <= M")
    return index, count


def elevated():
    try:
        if PLAT == "windows":
            import ctypes

            return bool(ctypes.windll.shell32.IsUserAnAdmin())
        return os.geteuid() == 0
    except Exception:
        return None


def markdown(doc):
    job = f", job {doc['job_index']} of {doc['job_count']}" if doc.get("job_count") else ""
    lines = [f"### Speed on {doc['platform']}{job}", ""]
    lines.append(f"Run {doc.get('run_id')}, commit `{(doc.get('commit') or '')[:12]}`, {doc['runner']['cores']} cores, {doc['date']}.")
    for tree in doc["trees"]:
        lines += ["", f"**{tree['mode']} {tree['id']}** `{tree['path']}`, {tree['items']} items", ""]
        lines.append("| Tool | Version | Median ms | minimenta median ms | Ratio (tool / minimenta) | Middle half | Pairs | Result |")
        lines.append("| --- | --- | ---: | ---: | ---: | --- | ---: | --- |")
        for r in tree["results"]:
            name = r["label"] + (" (diagnostic)" if r["diagnostic"] else "")
            lines.append(
                f"| {name} | {r['version']} | {r['median_ms']} | {r['minimenta_median_ms']} | {r['ratio_median']:.2f} | "
                f"{r['ratio_q1']:.2f} to {r['ratio_q3']:.2f} | {r['pairs']} | {r['verdict']} |"
            )
        lower = [k for k, v in tree["totals"].items() if not v["ok"]]
        higher = [k for k, v in tree["totals"].items() if v.get("higher")]
        lines.append("")
        lines.append(
            "Totals in bytes: "
            + ", ".join(f"{k} {v['bytes']}" for k, v in tree["totals"].items())
            + (f". Lower than minimenta, check: {', '.join(lower)}." if lower else ".")
            + (f" Higher than minimenta: {', '.join(higher)}." if higher else "")
        )
    return "\n".join(lines) + "\n"


def emit(doc, out):
    with open(out, "w", encoding="utf-8") as f:
        json.dump(doc, f, indent=1)
        f.write("\n")


def main():
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", required=True)
    parser.add_argument("--tools-dir", required=True)
    parser.add_argument("--minimenta", default=os.path.join("target", "release", "minimenta" + (".exe" if PLAT == "windows" else "")))
    parser.add_argument("--cold", action="append", default=[], metavar="ID=PATH")
    parser.add_argument("--warm", action="append", default=[], metavar="ID=PATH")
    parser.add_argument("--min-pairs", type=int, default=3)
    parser.add_argument("--max-pairs", type=int, default=15, help="most pairs in a cold section")
    parser.add_argument("--max-warm-pairs", type=int, default=60, help="most pairs in a warm section")
    parser.add_argument("--cold-budget", type=int, default=480, help="seconds per cold tree")
    parser.add_argument("--warm-budget", type=int, default=240, help="seconds per warm tree")
    parser.add_argument("--timeout", type=int, default=900, help="seconds for one run")
    parser.add_argument("--allow-missing", action="store_true", help="skip tools that are not installed (local tests)")
    parser.add_argument("--job", type=job_spec, help="N/M: this is job N of M jobs with the same comparison")
    args = parser.parse_args()

    cores = os.cpu_count() or 1
    runner = Runner(args.timeout)
    doc = environment(args, cores)
    doc["trees"] = []
    doc["tools"] = {}
    try:
        for mode, specs in (("cold", args.cold), ("warm", args.warm)):
            for spec in specs:
                tree_id, path = spec.split("=", 1)
                section = run_section(args, runner, mode, tree_id, path, cores)
                doc["trees"].append(section)
                for r in section["results"]:
                    doc["tools"].setdefault(r["tool"], {"name": r["name"], "version": r["version"], "settings": r["settings"], "threads": r["threads"]})
                emit(doc, args.out)
    finally:
        doc["finished"] = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
        emit(doc, args.out)
        log(f"===== MINIMENTA SPEED JSON BEGIN {PLAT} =====")
        log(json.dumps(doc, indent=1))
        log(f"===== MINIMENTA SPEED JSON END {PLAT} =====")
        summary = os.environ.get("GITHUB_STEP_SUMMARY")
        if summary and doc["trees"]:
            with open(summary, "a", encoding="utf-8") as f:
                f.write(markdown(doc))


if __name__ == "__main__":
    main()
