#!/usr/bin/env bash
# Compares cold-cache scans on Linux, where the disk is the bottleneck.
# Needs sudo to drop the page cache before every run.
# Usage: bench/cold.sh [TREE] [PAIRS]
set -euo pipefail

tree=${1:-/usr}
pairs=${2:-3}
bin=./target/release/minimenta
ncdu=${NCDU:-ncdu}
drop="sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null"
cargo build --release -q

echo "-- cold: A = minimenta (default threads), B = ncdu -t 64 (its fastest cold setting)"
python3 -I bench/interleave.py --prepare "$drop" "$pairs" "$bin --summary $tree" "$ncdu -0 -t 64 -O /dev/null --no-compress $tree"
echo "-- cold: A = minimenta (default threads), B = ncdu with its defaults (1 thread)"
python3 -I bench/interleave.py --prepare "$drop" 2 "$bin --summary $tree" "$ncdu -0 -O /dev/null --no-compress $tree"
