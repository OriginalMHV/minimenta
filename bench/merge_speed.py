"""Merges the JSON documents of bench/compare.py, one per platform or job, into one.

Usage: python -I bench/merge_speed.py OUT FILE...

The merged document keeps the document of each platform under "platforms"
(unchanged, or pooled as described below). It also has "rows", one flat row
for each tool on each tree, which is the form a chart needs. Each row names the
run ID and the commit of its platform, so every number traces to a run.

A platform can have several documents from bench/compare.py --job N/M, one per
job. The script pools them into one platform document of the same form: the
pairs of all jobs form one sample, and the ratio, middle half and verdict come
from all pairs together. Each pooled row also lists the median ratio of each
job and an interval for the median of the job medians, which holds it with a
probability of at least 95 percent (see median_interval in bench/compare.py).
The documents of the jobs stay unchanged under "job_documents". A job that is
missing, or that misses a tree or a tool, fails the merge after the merged
document is written.
"""

import datetime
import json
import os
import statistics
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from compare import median_interval, quartiles, verdict  # noqa: E402

NOTES = {
    "ratio": "Time of the tool divided by the time of minimenta, for each pair of runs that alternate in order. "
    "ratio_median is the median of these ratios. A value above 1 means that minimenta is faster. "
    "A value below 1 means that the tool is faster.",
    "middle_half": "ratio_q1 to ratio_q3 is the middle half of the sorted ratios. With 3 pairs, it is the full range.",
    "verdict": "even when the middle half includes 1.0. Otherwise the tool that is faster in the median.",
    "diagnostic": "Rows with diagnostic true check a setting of a tool. They are not part of the comparison.",
    "jobs": "jobs is the number of jobs whose pairs a row pools. The ratio of a pooled row is the median of the pairs of all its jobs.",
}

ROW_FIELDS = (
    "tool", "name", "label", "version", "settings", "threads", "pairs", "minimenta_faster_pairs", "tool_faster_pairs", "median_ms", "minimenta_median_ms",
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
            row["jobs"] = result.get("jobs", 1)
            row["job_medians"] = [entry["ratio_median"] for entry in result.get("job_ratios", [])]
            row["total_bytes"] = tree["totals"].get(result["tool"] if result["tool"] in tree["totals"] else result["name"], {}).get("bytes")
            yield row


def pool_result(index_results, warnings, where):
    """Pools the results of one tool on one tree from several jobs."""
    results = [result for _, result in index_results]
    first = results[0]
    for field in ("name", "label", "diagnostic", "settings", "threads"):
        if any(result[field] != first[field] for result in results):
            warnings.append(f"{where} {first['tool']}: the jobs differ in {field}")
    ratios = [ratio for result in results for ratio in result["ratios"]]
    q1, q3 = quartiles(ratios)
    median = statistics.median(ratios)
    versions = sorted({result["version"] for result in results if result["version"] is not None})
    if len(versions) < len({result["version"] for result in results}):
        warnings.append(f"{where} {first['tool']}: some jobs found no version")
    job_medians = [result["ratio_median"] for result in results]
    interval = median_interval(job_medians)
    return {
        "tool": first["tool"],
        "name": first["name"],
        "label": first["label"],
        "diagnostic": first["diagnostic"],
        "version": " / ".join(versions) if versions else None,
        "settings": first["settings"],
        "threads": first["threads"],
        "pairs": len(ratios),
        "estimated_pairs": sum(result["estimated_pairs"] for result in results),
        "minimenta_faster_pairs": sum(result["minimenta_faster_pairs"] for result in results),
        "tool_faster_pairs": sum(result["tool_faster_pairs"] for result in results),
        "median_ms": round(statistics.median(t for result in results for t in result["times_ms"]), 1),
        "minimenta_median_ms": round(statistics.median(t for result in results for t in result["minimenta_times_ms"]), 1),
        "ratio_median": round(median, 3),
        "ratio_q1": round(q1, 3),
        "ratio_q3": round(q3, 3),
        "verdict": verdict(median, q1, q3),
        "times_ms": [t for result in results for t in result["times_ms"]],
        "minimenta_times_ms": [t for result in results for t in result["minimenta_times_ms"]],
        "ratios": ratios,
        "exit_codes": sorted({code for result in results for code in result["exit_codes"]}),
        "jobs": len(results),
        "job_ratios": [{"job": index, "pairs": result["pairs"], "ratio_median": result["ratio_median"]} for index, result in index_results],
        "jobs_minimenta_faster": sum(m > 1.0 for m in job_medians),
        "jobs_tool_faster": sum(m < 1.0 for m in job_medians),
        "job_median_interval": list(interval) if interval else None,
    }


def pool_tree(parts, warnings):
    """Pools one tree of several jobs. parts holds (job index, tree) pairs."""
    first = parts[0][1]
    where = f"{first['mode']} {first['id']}"
    if any(tree["path"] != first["path"] for _, tree in parts):
        sys.exit(f"merge_speed: the jobs scanned different paths for {where}")
    for field in ("items", "errors"):
        if any(tree[field] != first[field] for _, tree in parts):
            warnings.append(f"{where}: the jobs differ in {field}. The tree shows the value of job {parts[0][0]}.")
    by_tool = {}
    for index, tree in parts:
        for result in tree["results"]:
            by_tool.setdefault(result["tool"], []).append((index, result))
    return {
        "id": first["id"],
        "path": first["path"],
        "mode": first["mode"],
        "budget_s": first["budget_s"],
        "items": first["items"],
        "errors": first["errors"],
        "totals": first["totals"],
        "totals_ok": all(tree["totals_ok"] for _, tree in parts),
        "jobs": [
            {"job": index, "items": tree["items"], "errors": tree["errors"], "totals_ok": tree["totals_ok"], "elapsed_s": tree["elapsed_s"]}
            for index, tree in parts
        ],
        "results": [pool_result(results, warnings, where) for results in by_tool.values()],
    }


def pool(docs, warnings):
    """Pools the documents of the jobs of one platform into one document."""
    docs = sorted(docs, key=lambda d: d.get("job_index") or 0)
    platform = docs[0]["platform"]
    count = docs[0].get("job_count")
    indexes = [d.get("job_index") for d in docs]
    if not count or any(d.get("job_count") != count for d in docs) or len(set(indexes)) != len(indexes):
        sys.exit(f"merge_speed: two documents for {platform} that are not distinct jobs of one comparison")
    pooled = {key: value for key, value in docs[0].items() if key not in ("job_index", "trees", "date", "finished", "runner", "run_attempt")}
    pooled["date"] = min(d["date"] for d in docs)
    pooled["finished"] = max(d.get("finished") or d["date"] for d in docs)
    pooled["missing_jobs"] = [index for index in range(1, count + 1) if index not in indexes]
    pooled["jobs"] = [
        {"job": d["job_index"], "run_attempt": d.get("run_attempt"), "date": d["date"], "finished": d.get("finished"), "runner": d["runner"]}
        for d in docs
    ]
    keys, parts = [], {}
    for d in docs:
        for tree in d["trees"]:
            key = (tree["mode"], tree["id"])
            if key not in parts:
                keys.append(key)
            parts.setdefault(key, []).append((d["job_index"], tree))
    pooled["trees"] = [pool_tree(parts[key], warnings) for key in keys]
    incomplete = [f"job {d['job_index']} has no trees" for d in docs if not d["trees"]]
    for key in keys:
        present = [index for index, _ in parts[key]]
        incomplete += [f"job {index} misses the {key[0]} tree {key[1]}" for index in indexes if index not in present]
    for tree in pooled["trees"]:
        for result in tree["results"]:
            present = [entry["job"] for entry in result["job_ratios"]]
            for job in tree["jobs"]:
                if job["job"] not in present:
                    incomplete.append(f"job {job['job']} has no pairs of {result['tool']} in the {tree['mode']} tree {tree['id']}")
    pooled["incomplete_jobs"] = incomplete
    pooled["tools"] = {}
    for tree in pooled["trees"]:
        for result in tree["results"]:
            pooled["tools"].setdefault(result["tool"], {"name": result["name"], "version": result["version"], "settings": result["settings"], "threads": result["threads"]})
    return pooled


def jobs_markdown(platform, doc):
    lines = ["", f"**{platform}: ratio of each job**", ""]
    header = ["Tree", "Tool"] + [f"Job {job['job']}" for job in doc["jobs"]] + ["Pooled", "Faster jobs", "Interval of the job median"]
    lines.append("| " + " | ".join(header) + " |")
    lines.append("| --- | --- |" + " ---: |" * (len(doc["jobs"]) + 1) + " --- | --- |")
    for tree in doc["trees"]:
        for r in tree["results"]:
            if r["diagnostic"]:
                continue
            by_job = {entry["job"]: entry for entry in r["job_ratios"]}
            cells = [f"{by_job[job['job']]['ratio_median']:.3f}" if job["job"] in by_job else "n/a" for job in doc["jobs"]]
            interval = r["job_median_interval"]
            cells += [
                f"{r['ratio_median']:.3f}",
                f"{r['jobs_minimenta_faster']} / {r['jobs_tool_faster']}",
                f"{interval[0]:.3f} to {interval[1]:.3f}" if interval else "n/a",
            ]
            lines.append(f"| {tree['mode']} {tree['id']} | {r['label']} | " + " | ".join(cells) + " |")
    lines.append("")
    lines.append("CPUs: " + ", ".join(f"job {job['job']} {job['runner'].get('cpu')}" for job in doc["jobs"]) + ".")
    return lines


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
    for platform, doc in merged["platforms"].items():
        if doc.get("jobs"):
            lines += jobs_markdown(platform, doc)
    return "\n".join(lines) + "\n"


def main():
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    out, files = sys.argv[1], sys.argv[2:]
    by_platform = {}
    for path in files:
        with open(path, encoding="utf-8") as f:
            doc = json.load(f)
        by_platform.setdefault(doc["platform"], []).append(doc)
    if not by_platform:
        sys.exit("merge_speed: no input")
    warnings, platforms, jobs = [], {}, {}
    for name, each in by_platform.items():
        if len(each) == 1 and not each[0].get("job_count"):
            platforms[name] = each[0]
        else:
            platforms[name] = pool(each, warnings)
            jobs[name] = sorted(each, key=lambda d: d["job_index"])
    missing = [p for p in ("linux", "macos", "windows") if p not in platforms]
    missing_jobs = {name: doc["missing_jobs"] for name, doc in platforms.items() if doc.get("missing_jobs")}
    incomplete = {name: doc["incomplete_jobs"] for name, doc in platforms.items() if doc.get("incomplete_jobs")}
    docs = [doc for each in by_platform.values() for doc in each]
    merged = {
        "schema": 1,
        "date": max(d.get("finished") or d["date"] for d in docs),
        "commit": docs[0].get("commit"),
        "run_id": docs[0].get("run_id"),
        "pull_request_head": docs[0].get("pull_request_head"),
        "ref": docs[0].get("ref"),
        "missing_platforms": missing,
        "missing_jobs": missing_jobs,
        "incomplete_jobs": incomplete,
        "notes": NOTES,
        "platforms": {name: platforms[name] for name in sorted(platforms)},
        "job_documents": {name: jobs[name] for name in sorted(jobs)},
        "rows": [row for name in sorted(platforms) for row in rows(platforms[name])],
    }
    for d in docs:
        if d.get("commit") != merged["commit"] or d.get("run_id") != merged["run_id"]:
            warnings.append(f"{d['platform']} comes from another commit or run")
    if warnings:
        merged["warnings"] = warnings
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
    if missing_jobs:
        sys.exit("merge_speed: missing jobs: " + ", ".join(f"{name} {jobs}" for name, jobs in missing_jobs.items()))
    if incomplete:
        sys.exit("merge_speed: incomplete jobs: " + "; ".join(f"{name}: {', '.join(items)}" for name, items in incomplete.items()))
    _ = datetime


if __name__ == "__main__":
    main()
