#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$root/.dev/cargo" "$root/.dev/target"
case $(uname -m) in x86_64) arch=amd64 ;; aarch64|arm64) arch=arm64 ;; *) echo 'Unsupported Docker host architecture' >&2; exit 1 ;; esac
if ! docker image inspect "ahrara-builder:$arch" >/dev/null 2>&1; then
  docker build -f "$root/cli/Dockerfile.builder" -t "ahrara-builder:$arch" "$root" >&2
fi
exec docker run --rm --network host --user "$(id -u):$(id -g)" \
  -v "$root:/work" -w /work/cli \
  -e CARGO_HOME=/work/.dev/cargo -e CARGO_TARGET_DIR=/work/.dev/target \
  -e AHRARA_TEST_BINARY \
  "ahrara-builder:$arch" cargo "$@"
