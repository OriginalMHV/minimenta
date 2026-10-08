#!/usr/bin/env bash
# Compares minimenta and ncdu with a cold page cache, at several thread counts.
# With a cold cache the disk is the bottleneck, so more threads than cores can
# help both tools. Linux only, needs sudo to drop the page cache.
# Usage: bench/cold.sh [TREE] [PAIRS]
set -euo pipefail

tree=${1:-/usr}
pairs=${2:-3}
bin=./target/release/minimenta
ncdu=${NCDU:-ncdu}
drop="sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null"

cargo build --release -q
for t in 4 16 64; do
  echo "-- cold cache, $t threads: A = minimenta, B = ncdu (both -t $t)"
  python3 -I bench/interleave.py --prepare "$drop" "$pairs" \
    "$bin --summary -t $t $tree" "$ncdu -0 -t $t -O /dev/null --no-compress $tree"
done
