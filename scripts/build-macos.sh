#!/usr/bin/env bash
set -euo pipefail
[[ $(uname -s) == Darwin ]] || { echo 'Run on macOS with Xcode Command Line Tools.' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
xcrun --find clang >/dev/null
export MACOSX_DEPLOYMENT_TARGET=11.0
cd "$root/cli"
mkdir -p "$root/dist"
for arch in x86_64 aarch64; do
  rustup target add "$arch-apple-darwin" --toolchain 1.98.1
  cargo build --locked --release --target "$arch-apple-darwin" --bin ahrara
  cp "target/$arch-apple-darwin/release/ahrara" "$root/dist/ahrara-darwin-$arch"
done
