"""Creates a fixed synthetic tree for throughput benchmarks: 10x10x10 leaf
directories with 50 empty files each (about 51,000 items). Empty files keep
generation fast when endpoint security inspects every write."""

import os
import sys

root = sys.argv[1]
fanout, depth, files = 10, 3, 50


def build(path, level):
    os.makedirs(path, exist_ok=True)
    if level == depth:
        for i in range(files):
            open(os.path.join(path, f"file{i:02}.dat"), "wb").close()
        return
    for i in range(fanout):
        build(os.path.join(path, f"d{i}"), level + 1)


build(root, 0)
