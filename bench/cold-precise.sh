#!/usr/bin/env bash
# Experiment: a more precise cold-cache comparison on Linux.
# Usage: bench/cold-precise.sh [TREE] [PAIRS]
set -euo pipefail

tree=${1:-/usr}
pairs=${2:-8}
bin=./target/release/minimenta
ncdu=${NCDU:-ncdu}
drop="sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null"
cargo build --release -q

echo "-- cold: A = minimenta -t 16, B = ncdu -t 64"
python3 -I bench/interleave.py --prepare "$drop" "$pairs" "$bin --summary -t 16 $tree" "$ncdu -0 -t 64 -O /dev/null --no-compress $tree"
echo "-- cold: A = minimenta -t 64, B = ncdu -t 64"
python3 -I bench/interleave.py --prepare "$drop" "$pairs" "$bin --summary -t 64 $tree" "$ncdu -0 -t 64 -O /dev/null --no-compress $tree"
echo "-- cold: A = minimenta -t 64, B = minimenta -t 16"
python3 -I bench/interleave.py --prepare "$drop" "$pairs" "$bin --summary -t 64 $tree" "$bin --summary -t 16 $tree"
