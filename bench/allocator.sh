#!/usr/bin/env bash
# Experiment: compares minimenta built with mimalloc against the system
# allocator, warm cache, in alternating pairs.
# Usage: bench/allocator.sh TREE [PAIRS] [EXTRA ARGS...]
set -euo pipefail
tree=$1
pairs=${2:-20}
shift 2 || true
extra="$*"
py=${PY:-python3}
exe=minimenta
[ "$(uname -s | cut -c1-5)" = MINGW ] && exe=minimenta.exe
cargo build --release -q
cargo build --release -q --features mimalloc --target-dir target-mimalloc
echo "-- warm: A = mimalloc, B = system allocator ($tree $extra)"
$py -I bench/interleave.py "$pairs" "./target-mimalloc/release/$exe --summary $extra '$tree'" "./target/release/$exe --summary $extra '$tree'"
