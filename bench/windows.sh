#!/usr/bin/env bash
# Compares minimenta with gdu and dua-cli on Windows (Git Bash), warm cache.
# Both tools scan in parallel by default. Paths use forward slashes, which
# Windows accepts and which survive the shell quoting in interleave.py.
# Usage: GDU=... DUA=... bench/windows.sh TREE [PAIRS]
set -euo pipefail

tree=$1
pairs=${2:-30}
bin=./target/release/minimenta.exe
py=${PY:-python}

cargo build --release -q
echo "$($bin --summary "$tree")"
echo "-- warm: A = minimenta, B = gdu (defaults)"
$py -I bench/interleave.py "$pairs" "$bin --summary '$tree'" "$GDU -n -p -c '$tree'"
echo "-- warm: A = minimenta, B = dua-cli (defaults)"
$py -I bench/interleave.py "$pairs" "$bin --summary '$tree'" "$DUA '$tree'"

# COLD_PAIRS > 0: also compare with the file cache cleared before every run.
cold_pairs=${COLD_PAIRS:-0}
if [ "$cold_pairs" -gt 0 ]; then
  purge="powershell -NoProfile -ExecutionPolicy Bypass -File bench/windows-purge.ps1"
  echo "-- cold check: one warm and one cold run of minimenta"
  $bin --summary "$tree" | sed 's/^/warm: /'
  $purge && $bin --summary "$tree" | sed 's/^/cold: /'
  echo "-- cold: A = minimenta, B = gdu (defaults)"
  $py -I bench/interleave.py --prepare "$purge" "$cold_pairs" "$bin --summary '$tree'" "$GDU -n -p -c '$tree'"
  echo "-- cold: A = minimenta, B = dua-cli (defaults)"
  $py -I bench/interleave.py --prepare "$purge" "$cold_pairs" "$bin --summary '$tree'" "$DUA '$tree'"
fi
