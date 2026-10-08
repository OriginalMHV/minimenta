#!/usr/bin/env bash
# Experiment: tunes the race between the MFT reader and the listing.
# Usage: bench/windows-race.sh TREE [WARM_PAIRS] [COLD_PAIRS]
set -euo pipefail
tree=$1
warm=${2:-10}
cold=${3:-3}
bin=./target/release/minimenta.exe
py=${PY:-python}
purge="powershell -NoProfile -ExecutionPolicy Bypass -File bench/windows-purge.ps1"
listing="$bin --summary --no-mft '$tree'"
cargo build --release -q
for variant in "" "MINIMENTA_MFT_DELAY_MS=250" "MINIMENTA_MFT_THREADS=2" \
    "MINIMENTA_MFT_DELAY_MS=250 MINIMENTA_MFT_THREADS=2" \
    "MINIMENTA_MFT_DELAY_MS=250 MINIMENTA_MFT_THREADS=2 MINIMENTA_MFT_NOFLUSH=1"; do
  echo "-- warm: A = race [${variant:-defaults}], B = --no-mft"
  $py -I bench/interleave.py "$warm" "env $variant $bin --summary '$tree'" "$listing"
done
[ "$cold" -gt 0 ] || exit 0
best="MINIMENTA_MFT_DELAY_MS=250 MINIMENTA_MFT_THREADS=2"
echo "-- cold: A = race [$best], B = --no-mft"
$py -I bench/interleave.py --prepare "$purge" "$cold" "env $best $bin --summary '$tree'" "$listing"
echo "-- cold: A = race [$best], B = race [defaults]"
$py -I bench/interleave.py --prepare "$purge" "$cold" "env $best $bin --summary '$tree'" "$bin --summary '$tree'"
