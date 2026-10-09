#!/usr/bin/env bash
# Installs the other disk usage tools for bench/compare.py into DEST/bin.
# Every download is pinned by SHA-256. The values come from the official
# release pages and were checked against the digests that GitHub publishes.
# Usage: bench/install-tools.sh linux|macos|windows DEST
set -euo pipefail

platform=${1:?usage: install-tools.sh linux|macos|windows DEST}
dest=${2:?usage: install-tools.sh linux|macos|windows DEST}
ua="minimenta-bench"

GDU_VERSION=5.38.0
DUA_VERSION=2.45.1
DUST_VERSION=1.2.6

# ncdu 2.9.2 (October 2025) has no prebuilt binary. It only fixes a hang when
# ncdu is built with Zig 0.15.2. The newest static build on dev.yorhel.nl is
# 2.9.1. The PGP signature of that file by Yoran Heling (key
# 7446 0D32 B808 10EB A9AF A2E9 6239 4C69 8C27 39FA) was checked.
NCDU_VERSION=2.9.1
NCDU_SHA256_linux=0c6c84b3f763c33baa051c8327f0e736f46f33a8cc061bb951d24517a8c5d73e

case "$platform" in
  linux)
    GDU_ASSET=gdu_linux_amd64.tgz
    GDU_SHA256=91735d86160f6f72ecd92ed277a765b23d787e0cb34b9cd55ddece7186eb38b9
    DUA_ASSET=dua-v$DUA_VERSION-x86_64-unknown-linux-musl.tar.gz
    DUA_SHA256=e9279f02bbbf22611c1602c4d5b526c9b99962d3d828016d48a0ee5b27252805
    DUST_ASSET=dust-v$DUST_VERSION-x86_64-unknown-linux-gnu.tar.gz
    DUST_SHA256=14f788707aad217667f6812cf1a9639bf5c861a2ee815d486cffc36051f5e759
    ;;
  macos)
    [ "$(uname -m)" = arm64 ] || { echo "install-tools: this script pins arm64 macOS builds" >&2; exit 1; }
    GDU_ASSET=gdu_darwin_arm64.tgz
    GDU_SHA256=cbfc4981f6d5d5bf66a1dbb1faa65bc64b15060b2782b325b888f616655415db
    DUA_ASSET=dua-v$DUA_VERSION-aarch64-apple-darwin.tar.gz
    DUA_SHA256=fdc3149dd99287f007f67ad81b2c75cfa23fed653007f1cb9a83f5a333e683e1
    DUST_ASSET=dust-v$DUST_VERSION-aarch64-apple-darwin.tar.gz
    DUST_SHA256=5c37ee0d353165c73aedd6b21b9133182221efe08f9a5e76c082d9d6d5a0935d
    ;;
  windows)
    GDU_ASSET=gdu_windows_amd64.exe.zip
    GDU_SHA256=22e6616e0bdc25d8d803934b4d3189996ebf74de8ac9b3635a4e4e079c3d7f8a
    DUA_ASSET=dua-v$DUA_VERSION-x86_64-pc-windows-msvc.zip
    DUA_SHA256=48c245f987c5359df0302202e6338e3235e4a4724db58d7d66138b096e8b2015
    DUST_ASSET=dust-v$DUST_VERSION-x86_64-pc-windows-msvc.zip
    DUST_SHA256=6a73222902d6960ce7183cd833a5d3ea99a3e3ff3cfb33720c1939a41cb7744d
    ;;
  *) echo "install-tools: unknown platform $platform" >&2; exit 1 ;;
esac

sha256_check() {
  if command -v sha256sum >/dev/null; then
    echo "$1  $2" | sha256sum -c -
  else
    echo "$1  $2" | shasum -a 256 -c -
  fi
}

# fetch URL FILE SHA256: each file goes in its own folder, so nothing downloaded is ever run from there.
fetch() {
  mkdir -p "$(dirname "$2")"
  curl -sSfL -A "$ua" -o "$2" "$1"
  sha256_check "$3" "$2"
}

# extract ARCHIVE OUTDIR: tar.gz, tgz or zip, flat.
extract() {
  mkdir -p "$2"
  case "$1" in
    *.zip) unzip -q -o -j "$1" -d "$2" ;;
    *) tar -C "$2" -xzf "$1" ;;
  esac
}

work="$dest/dl"
bin="$dest/bin"
mkdir -p "$bin"
ext=""
if [ "$platform" = windows ]; then
  ext=".exe"
fi

if [ "$platform" = linux ]; then
  fetch "https://dev.yorhel.nl/download/ncdu-$NCDU_VERSION-linux-x86_64.tar.gz" "$work/ncdu/ncdu.tar.gz" "$NCDU_SHA256_linux"
  extract "$work/ncdu/ncdu.tar.gz" "$work/ncdu/x"
  install -m 0755 "$work/ncdu/x/ncdu" "$bin/ncdu"
elif [ "$platform" = macos ]; then
  # The author publishes no macOS build. Homebrew builds ncdu from the source release.
  export HOMEBREW_NO_INSTALL_CLEANUP=1 HOMEBREW_NO_ENV_HINTS=1
  brew install ncdu
  ln -sf "$(brew --prefix ncdu)/bin/ncdu" "$bin/ncdu"
  brew info ncdu | head -3
fi

fetch "https://github.com/dundee/gdu/releases/download/v$GDU_VERSION/$GDU_ASSET" "$work/gdu/$GDU_ASSET" "$GDU_SHA256"
extract "$work/gdu/$GDU_ASSET" "$work/gdu/x"
install -m 0755 "$work/gdu/x/"gdu_* "$bin/gdu$ext"

fetch "https://github.com/Byron/dua-cli/releases/download/v$DUA_VERSION/$DUA_ASSET" "$work/dua/$DUA_ASSET" "$DUA_SHA256"
extract "$work/dua/$DUA_ASSET" "$work/dua/x"
if [ "$platform" = windows ]; then
  install -m 0755 "$work/dua/x/dua.exe" "$bin/dua.exe"
else
  install -m 0755 "$(find "$work/dua/x" -name dua -type f | head -n 1)" "$bin/dua"
fi

fetch "https://github.com/bootandy/dust/releases/download/v$DUST_VERSION/$DUST_ASSET" "$work/dust/$DUST_ASSET" "$DUST_SHA256"
extract "$work/dust/$DUST_ASSET" "$work/dust/x"
if [ "$platform" = windows ]; then
  install -m 0755 "$work/dust/x/dust.exe" "$bin/dust.exe"
else
  install -m 0755 "$(find "$work/dust/x" -name dust -type f | head -n 1)" "$bin/dust"
fi

echo "-- installed tools in $bin"
for tool in ncdu gdu dua dust; do
  [ -e "$bin/$tool$ext" ] || continue
  printf '%s: ' "$tool"
  "$bin/$tool$ext" --version 2>&1 | head -n 2 | tr '\n' ' '
  echo
done
