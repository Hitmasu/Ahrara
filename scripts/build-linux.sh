#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
arch=${1:-x86_64}
case "$arch" in
  x86_64) platform=amd64 ;;
  aarch64) platform=arm64 ;;
  *) echo 'usage: build-linux.sh x86_64|aarch64' >&2; exit 1 ;;
esac
docker build --platform "linux/$platform" -f "$root/cli/Dockerfile.builder" -t "ahrara-builder:$platform" "$root"
mkdir -p "$root/.dev/cargo" "$root/.dev/target" "$root/dist"
docker run --rm --platform "linux/$platform" --user "$(id -u):$(id -g)" \
  -v "$root:/work" -w /work/cli -e CARGO_HOME=/work/.dev/cargo -e CARGO_TARGET_DIR=/work/.dev/target \
  "ahrara-builder:$platform" cargo build --locked --release --bin ahrara --target "$arch-unknown-linux-musl"
cp "$root/.dev/target/$arch-unknown-linux-musl/release/ahrara" "$root/dist/ahrara-linux-$arch"
