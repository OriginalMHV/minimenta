#!/usr/bin/env bash
# Measures scan throughput (items per second) of minimenta and ncdu on a fixed
# synthetic tree. Runs several rounds and reports the median speedup, because
# shared machines vary from minute to minute.
# Usage: bench/throughput.sh [TREE_DIR] [ROUNDS]
set -euo pipefail

tree=${1:-${TMPDIR:-/tmp}/minimenta-bench-tree}
rounds=${2:-3}
bin=./target/release/minimenta
threads=$(sysctl -n hw.ncpu 2>/dev/null || nproc)
json=$(mktemp)
trap 'rm -f "$json"' EXIT

[ -d "$tree" ] || python3 -I bench/gen_tree.py "$tree"
cargo build --release -q
items=$($bin --summary "$tree" | sed -E 's/.* ([0-9]+) items.*/\1/')
echo "$items items, $threads threads, $rounds rounds of 30 runs"

# Prints "MINIMENTA_MEDIAN NCDU_MEDIAN" in seconds for one round.
round() {
  hyperfine -N --warmup 3 --runs 30 --export-json "$json" \
    "$bin --summary -t $1 $tree" \
    "ncdu -0 -t $1 -O /dev/null --no-compress $tree" >/dev/null 2>&1
  jq -r '"\(.results[0].median) \(.results[1].median)"' "$json"
}

ratios=()
for r in $(seq "$rounds"); do
  read -r mm nc < <(round "$threads")
  ratio=$(echo "$nc / $mm" | bc -l)
  ratios+=("$ratio")
  printf 'round %d: minimenta %4.0f ms (%7.0f items/s)  ncdu -t %d %4.0f ms (%7.0f items/s)  %.2fx\n' \
    "$r" "$(echo "$mm * 1000" | bc -l)" "$(echo "$items / $mm" | bc -l)" \
    "$threads" "$(echo "$nc * 1000" | bc -l)" "$(echo "$items / $nc" | bc -l)" "$ratio"
done
median=$(printf '%s\n' "${ratios[@]}" | sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}')
printf 'median speedup over ncdu -t %d: %.2fx\n' "$threads" "$median"

read -r mm nc < <(round 1)
printf 'single thread: minimenta %.0f ms  ncdu %.0f ms  %.2fx\n' \
  "$(echo "$mm * 1000" | bc -l)" "$(echo "$nc * 1000" | bc -l)" "$(echo "$nc / $mm" | bc -l)"

empty=$(mktemp -d)
hyperfine -N --warmup 3 --runs 30 --export-json "$json" \
  "$bin --summary $empty" "ncdu -0 -O /dev/null --no-compress $empty" >/dev/null 2>&1
rmdir "$empty"
jq -r '"startup (empty directory): minimenta \(.results[0].median * 10000 | floor / 10) ms  ncdu \(.results[1].median * 10000 | floor / 10) ms"' "$json"
