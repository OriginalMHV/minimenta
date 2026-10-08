"""Compares two commands by running them in alternating order and taking the
median of the per-pair time ratios. A machine that slows down for a while
then slows both commands, so the ratio stays fair on noisy shared runners.

Usage: python3 -I bench/interleave.py [--prepare "SHELL COMMAND"] PAIRS "COMMAND A" "COMMAND B"

--prepare runs before every timed command and is not timed, for example to
drop the page cache for a cold-cache comparison.
"""

import shlex
import statistics
import subprocess
import sys
import time


PREPARE = None


def run(cmd):
    if PREPARE:
        subprocess.run(PREPARE, shell=True, check=True)
    start = time.perf_counter()
    subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
    return time.perf_counter() - start


def main():
    global PREPARE
    args = sys.argv[1:]
    if args[0] == "--prepare":
        PREPARE, args = args[1], args[2:]
    pairs, a, b = int(args[0]), shlex.split(args[1]), shlex.split(args[2])
    # Warm-up runs make no sense when every run starts cold.
    for _ in range(0 if PREPARE else 3):
        run(a)
        run(b)
    times_a, times_b, ratios, diffs = [], [], [], []
    for i in range(pairs):
        # Swap the order every pair so neither command always runs first.
        ta, tb = (run(a), run(b)) if i % 2 == 0 else tuple(reversed((run(b), run(a))))
        times_a.append(ta)
        times_b.append(tb)
        ratios.append(tb / ta)
        diffs.append(tb - ta)
    ratios.sort()
    q1, q3 = ratios[len(ratios) // 4], ratios[3 * len(ratios) // 4]
    print(f"A median {statistics.median(times_a) * 1000:.1f} ms   B median {statistics.median(times_b) * 1000:.1f} ms")
    print(f"B/A median ratio {statistics.median(ratios):.2f}x  (middle half {q1:.2f}x to {q3:.2f}x, {pairs} pairs)")
    print(f"B-A median difference {statistics.median(diffs) * 1000:+.2f} ms")


main()
