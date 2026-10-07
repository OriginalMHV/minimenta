#!/usr/bin/env bash
# Creates the home folder that docs/demo/demo.tape records.
# Usage: docs/demo/make-home.sh DIR
# Large files are APFS clones of one blob. The tree reports about 8 GiB of
# disk usage but needs about 1.5 GiB. demo.tape runs it in a new temporary
# folder for each recording, because the demo deletes files.
set -euo pipefail

home=${1:?usage: make-home.sh DIR}
blob="$home.blob"
rm -rf "$home"
mkdir -p "$home"
[ -f "$blob" ] || dd if=/dev/zero of="$blob" bs=1048576 count=1600 2>/dev/null

# big PATH MIB: a large file of MIB mebibytes
big() {
  mkdir -p "$(dirname "$home/$1")"
  cp -c "$blob" "$home/$1" 2>/dev/null || cp "$blob" "$home/$1"
  truncate -s "${2}m" "$home/$1"
}

# small DIR COUNT KIB: COUNT files of about KIB kibibytes each
small() {
  mkdir -p "$home/$1"
  for i in $(seq "$2"); do
    head -c $(($3 * 1024 + i * 512)) /dev/zero >"$home/$1/file$i.js"
  done
}

big "Downloads/ubuntu-24.04.3-desktop-arm64.iso" 1536
big "Downloads/Screen Recording 2026-09-12 at 14.03.11.mov" 1229
big "Downloads/Docker.dmg" 590
big "Downloads/family-photos-2025.zip" 412
big "Downloads/Postman-osx-arm64.zip" 281
big "Downloads/node-v22.20.0.pkg" 85
big "Downloads/IMG_4031.HEIC" 3
big "Downloads/tax-return-2025.pdf" 1

big "Projects/rust-cli/target/debug/deps/librust_cli-3f9a1c2e.rlib" 612
big "Projects/rust-cli/target/debug/incremental/rust_cli-2k9x/s-h1.bin" 388
big "Projects/rust-cli/target/release/rust-cli" 14
big "Projects/ml-sandbox/data/train.parquet" 704
big "Projects/ml-sandbox/.venv/lib/python3.13/site-packages/torch/lib/libtorch_cpu.dylib" 236
big "Projects/webapp/node_modules/@next/swc-darwin-arm64/next-swc.darwin-arm64.node" 118
big "Projects/webapp/node_modules/@esbuild/darwin-arm64/bin/esbuild" 10
small "Projects/webapp/node_modules/react-dom/cjs" 40 24
small "Projects/webapp/node_modules/typescript/lib" 60 48
small "Projects/webapp/src" 30 4

big ".cache/huggingface/hub/models--Qwen--Qwen2.5-0.5B/blobs/model.safetensors" 943
big ".cache/pip/http-v2/wheels.bin" 214
big ".cache/uv/archive-v0/torch.bin" 180

big "Movies/Trip to Lofoten.mov" 802
big "Documents/Thesis/thesis-final-v3.pdf" 38
big "Documents/Thesis/figures.zip" 52
small "Documents/Notes" 25 6
