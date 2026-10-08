#!/usr/bin/env bash
# Experiments for cold-cache scans on Linux. Needs sudo to drop the page cache.
# Usage: bench/cold.sh [TREE] [PAIRS] [WARM_TREE]
set -euo pipefail

tree=${1:-/usr}
pairs=${2:-3}
warm_tree=${3:-${TMPDIR:-/tmp}/minimenta-bench-tree}
bin=./target/release/minimenta
ncdu=${NCDU:-ncdu}
drop="sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null"
cargo build --release -q

echo "-- cold, 64 threads: A = minimenta inode order, B = minimenta hash order"
python3 -I bench/interleave.py --prepare "$drop" "$pairs" \
  "env MINIMENTA_INODE_ORDER=1 $bin --summary -t 64 $tree" "$bin --summary -t 64 $tree"
echo "-- cold, 64 threads: A = minimenta inode order, B = ncdu -t 64"
python3 -I bench/interleave.py --prepare "$drop" "$pairs" \
  "env MINIMENTA_INODE_ORDER=1 $bin --summary -t 64 $tree" "$ncdu -0 -t 64 -O /dev/null --no-compress $tree"
echo "-- warm, $warm_tree: A = minimenta -t 64, B = minimenta -t $(nproc)"
python3 -I bench/interleave.py 30 "$bin --summary -t 64 $warm_tree" "$bin --summary -t $(nproc) $warm_tree"
echo "-- warm, $warm_tree: A = minimenta inode order, B = minimenta hash order"
python3 -I bench/interleave.py 30 "env MINIMENTA_INODE_ORDER=1 $bin --summary $warm_tree" "$bin --summary $warm_tree"
