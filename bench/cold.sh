#!/usr/bin/env bash
# Compares cold-cache scans, where the disk is the bottleneck. Drops the file
# cache before every run (purge on macOS, drop_caches on Linux). Needs sudo.
# Usage: bench/cold.sh [TREE] [PAIRS]
set -euo pipefail

tree=${1:-/usr}
pairs=${2:-3}
bin=./target/release/minimenta
ncdu=${NCDU:-ncdu}
if [ "$(uname)" = Darwin ]; then
  drop="sync; sudo purge"
else
  drop="sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null"
fi
cargo build --release -q

echo "-- cold: A = minimenta (default threads), B = ncdu -t 64 (its fastest cold setting)"
python3 -I bench/interleave.py --prepare "$drop" "$pairs" "$bin --summary $tree" "$ncdu -0 -t 64 -O /dev/null --no-compress $tree"
echo "-- cold: A = minimenta (default threads), B = ncdu with its defaults (1 thread)"
python3 -I bench/interleave.py --prepare "$drop" 2 "$bin --summary $tree" "$ncdu -0 -O /dev/null --no-compress $tree"
