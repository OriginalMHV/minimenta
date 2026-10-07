#!/usr/bin/env bash
# Measures scan throughput (items per second) of minimenta and ncdu on a fixed
# synthetic tree. Usage: bench/throughput.sh [TREE_DIR]
set -euo pipefail

tree=${1:-${TMPDIR:-/tmp}/minimenta-bench-tree}
bin=./target/release/minimenta
threads=$(sysctl -n hw.ncpu 2>/dev/null || nproc)

[ -d "$tree" ] || python3 -I bench/gen_tree.py "$tree"
cargo build --release -q
items=$($bin --summary "$tree" | sed -E 's/.* ([0-9]+) items.*/\1/')

json=$(mktemp)
hyperfine -N --warmup 2 --runs 10 --export-json "$json" \
  -n minimenta "$bin --summary $tree" \
  -n "ncdu -t $threads" "ncdu -0 -t $threads -O /dev/null --no-compress $tree" \
  -n "ncdu -t 1" "ncdu -0 -O /dev/null --no-compress $tree" >/dev/null

echo "$items items in $tree"
jq -r --argjson items "$items" '.results[] |
  "\(.command | .[0:16] + " " * (16 - length))  median \(.median * 1000 | floor) ms  mean \(.mean * 1000 | floor) ms  \($items / .median | floor) items/s"' "$json"
jq -r '.results as $r | "minimenta is \(($r[1].median / $r[0].median) * 100 | floor / 100)x faster than ncdu at its best"' "$json"
rm -f "$json"
