#!/usr/bin/env bash
# Finds a good default thread count for Linux: cold scans need many threads to
# keep the disk busy, warm scans prefer about one per core. Also compares with
# ncdu at its default settings. Needs sudo to drop the page cache.
# Usage: bench/cold.sh [COLD_TREE] [PAIRS] [WARM_TREE]
set -euo pipefail

tree=${1:-/usr}
pairs=${2:-3}
warm_tree=${3:-${TMPDIR:-/tmp}/minimenta-bench-tree}
bin=./target/release/minimenta
ncdu=${NCDU:-ncdu}
cores=$(nproc)
drop="sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null"
cargo build --release -q

for t in 8 16 32; do
  echo "-- cold: A = minimenta -t $t, B = minimenta -t $cores"
  python3 -I bench/interleave.py --prepare "$drop" "$pairs" "$bin --summary -t $t $tree" "$bin --summary -t $cores $tree"
  echo "-- warm: A = minimenta -t $t, B = minimenta -t $cores"
  python3 -I bench/interleave.py 30 "$bin --summary -t $t $warm_tree" "$bin --summary -t $cores $warm_tree"
done
echo "-- cold, defaults: A = minimenta -t 16, B = ncdu (no options)"
python3 -I bench/interleave.py --prepare "$drop" 2 "$bin --summary -t 16 $tree" "$ncdu -0 -O /dev/null --no-compress $tree"
