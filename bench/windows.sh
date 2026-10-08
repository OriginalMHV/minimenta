#!/usr/bin/env bash
# Compares minimenta with gdu and dua-cli on Windows (Git Bash), and minimenta
# reading the NTFS master file table with minimenta listing directories.
# Both other tools scan in parallel by default. Paths use forward slashes,
# which Windows accepts and which survive the shell quoting in interleave.py.
# Usage: GDU=... DUA=... [COLD_PAIRS=N] bench/windows.sh TREE [PAIRS]
set -euo pipefail

tree=$1
pairs=${2:-30}
bin=./target/release/minimenta.exe
py=${PY:-python}
mm="$bin --summary '$tree'"
listing="$bin --summary --no-mft '$tree'"
gdu="$GDU -n -p -c '$tree'"
dua="$DUA '$tree'"

cargo build --release -q
echo "$($bin --summary "$tree")"
for i in 1 2 3; do MINIMENTA_PROFILE=1 $bin --summary "$tree" 2>&1 >/dev/null | grep '^mft:' || true; done
echo "-- warm: A = minimenta, B = minimenta --no-mft"
$py -I bench/interleave.py "$pairs" "$mm" "$listing"
echo "-- warm: A = minimenta, B = gdu (defaults)"
$py -I bench/interleave.py "$pairs" "$mm" "$gdu"
echo "-- warm: A = minimenta, B = dua-cli (defaults)"
$py -I bench/interleave.py "$pairs" "$mm" "$dua"

# COLD_PAIRS > 0: also compare with the file cache cleared before every run.
cold_pairs=${COLD_PAIRS:-0}
if [ "$cold_pairs" -gt 0 ]; then
  purge="powershell -NoProfile -ExecutionPolicy Bypass -File bench/windows-purge.ps1"
  echo "-- cold: A = minimenta, B = minimenta --no-mft"
  $py -I bench/interleave.py --prepare "$purge" "$cold_pairs" "$mm" "$listing"
  echo "-- cold: A = minimenta, B = gdu (defaults)"
  $py -I bench/interleave.py --prepare "$purge" "$cold_pairs" "$mm" "$gdu"
  echo "-- cold: A = minimenta, B = dua-cli (defaults)"
  $py -I bench/interleave.py --prepare "$purge" "$cold_pairs" "$mm" "$dua"
fi
