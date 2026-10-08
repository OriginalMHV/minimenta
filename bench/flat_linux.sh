#!/usr/bin/env bash
# Compares scans of one directory with 200,000 empty files, warm and with a
# dropped page cache. Needs sudo for the cold runs.
# Usage: bench/flat_linux.sh [DIR] [PAIRS]
set -euo pipefail

dir=${1:-${TMPDIR:-/tmp}/minimenta-flat}
pairs=${2:-10}
bin=./target/release/minimenta
ncdu=${NCDU:-ncdu}
threads=$(nproc)
drop="sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null"

if [ ! -d "$dir" ]; then
  python3 -I - "$dir" <<'PY'
import os
import sys

os.makedirs(sys.argv[1])
for i in range(200_000):
    os.close(os.open(f"{sys.argv[1]}/file-{i:06}", os.O_CREAT | os.O_WRONLY))
PY
fi
cargo build --release -q

echo "-- one directory with 200000 files, warm: A = minimenta, B = ncdu -t $threads"
python3 -I bench/interleave.py "$pairs" "$bin --summary $dir" "$ncdu -0 -t $threads -O /dev/null --no-compress $dir"
echo "-- same, cold: A = minimenta, B = ncdu -t 64"
python3 -I bench/interleave.py --prepare "$drop" 3 "$bin --summary $dir" "$ncdu -0 -t 64 -O /dev/null --no-compress $dir"
