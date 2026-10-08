#!/usr/bin/env bash
# Experiment: tunes the race between the MFT reader and the listing. Every
# command starts through `env`, so the start-up cost of env.exe hits both
# sides of a comparison equally.
# Usage: bench/windows-race.sh TREE [WARM_PAIRS] [COLD_PAIRS]
set -euo pipefail
tree=$1
warm=${2:-10}
cold=${3:-3}
bin=./target/release/minimenta.exe
py=${PY:-python}
purge="powershell -NoProfile -ExecutionPolicy Bypass -File bench/windows-purge.ps1"
race="env MINIMENTA_UNUSED=1 $bin --summary '$tree'"
background="env MINIMENTA_MFT_BACKGROUND=1 $bin --summary '$tree'"
listing="env MINIMENTA_UNUSED=1 $bin --summary --no-mft '$tree'"
cargo build --release -q
both="env MINIMENTA_DIR_INFO=both $bin --summary --no-mft '$tree'"
echo "-- warm: A = --no-mft (full info class), B = --no-mft (class with short names)"
$py -I bench/interleave.py "$warm" "$listing" "$both"
echo "-- warm: A = race, B = --no-mft"
$py -I bench/interleave.py "$warm" "$race" "$listing"
echo "-- warm: A = race with background MFT threads, B = --no-mft"
$py -I bench/interleave.py "$warm" "$background" "$listing"
[ "$cold" -gt 0 ] || exit 0
for i in 1 2; do $purge; MINIMENTA_PROFILE=1 $bin --summary "$tree" 2>&1 >/dev/null | grep '^mft:' || true; done
echo "-- cold: A = --no-mft (full info class), B = --no-mft (class with short names)"
$py -I bench/interleave.py --prepare "$purge" "$cold" "$listing" "$both"
echo "-- cold: A = race, B = --no-mft"
$py -I bench/interleave.py --prepare "$purge" "$cold" "$race" "$listing"
echo "-- cold: A = race with background MFT threads, B = race"
$py -I bench/interleave.py --prepare "$purge" "$cold" "$background" "$race"
