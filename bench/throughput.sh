#!/usr/bin/env bash
# Measures how much faster minimenta scans than ncdu on a fixed synthetic tree.
# The commands run in alternating pairs (bench/interleave.py), so a machine
# that slows down for a while affects both tools equally.
# Usage: bench/throughput.sh [TREE_DIR] [PAIRS]
set -euo pipefail

tree=${1:-${TMPDIR:-/tmp}/minimenta-bench-tree}
pairs=${2:-60}
bin=./target/release/minimenta
ncdu=${NCDU:-ncdu}
threads=$(sysctl -n hw.ncpu 2>/dev/null || nproc)

[ -d "$tree" ] || python3 -I bench/gen_tree.py "$tree"
cargo build --release -q
items=$($bin --summary "$tree" | sed -E 's/.* ([0-9]+) items.*/\1/')
echo "$items items, $threads threads (A = minimenta, B = ncdu)"

echo "-- all threads: minimenta vs ncdu -t $threads"
python3 -I bench/interleave.py "$pairs" "$bin --summary $tree" "$ncdu -0 -t $threads -O /dev/null --no-compress $tree"
echo "-- single thread: minimenta -t 1 vs ncdu"
python3 -I bench/interleave.py "$((pairs / 2))" "$bin --summary -t 1 $tree" "$ncdu -0 -O /dev/null --no-compress $tree"
