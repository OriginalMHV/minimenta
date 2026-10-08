#!/usr/bin/env bash
# Experiment: cold and warm scans at several thread counts, plus ncdu.
# Drops the file cache before every cold run (purge on macOS,
# drop_caches on Linux). Needs sudo.
# Usage: bench/cold-sweep.sh TREE [PAIRS]
set -euo pipefail

tree=$1
pairs=${2:-3}
bin=./target/release/minimenta
ncdu=${NCDU:-ncdu}
cores=$(sysctl -n hw.ncpu 2>/dev/null || nproc)
if [ "$(uname)" = Darwin ]; then
  drop="sync; sudo purge"
else
  drop="sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null"
fi
cargo build --release -q
echo "$($bin --summary -t "$cores" "$tree")"

for t in 8 16 32 64; do
  echo "-- cold: A = minimenta -t $t, B = minimenta -t $cores"
  python3 -I bench/interleave.py --prepare "$drop" "$pairs" "$bin --summary -t $t $tree" "$bin --summary -t $cores $tree"
  echo "-- warm: A = minimenta -t $t, B = minimenta -t $cores"
  python3 -I bench/interleave.py 20 "$bin --summary -t $t $tree" "$bin --summary -t $cores $tree"
done
for t in 16 64; do
  echo "-- cold: A = minimenta -t $t, B = ncdu -t $t"
  python3 -I bench/interleave.py --prepare "$drop" "$pairs" "$bin --summary -t $t $tree" "$ncdu -0 -t $t -O /dev/null --no-compress $tree"
done
echo "-- cold: A = minimenta -t 16, B = ncdu with its defaults"
python3 -I bench/interleave.py --prepare "$drop" 2 "$bin --summary -t 16 $tree" "$ncdu -0 -O /dev/null --no-compress $tree"
