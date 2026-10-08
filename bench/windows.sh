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
