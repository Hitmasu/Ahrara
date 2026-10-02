#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$root/.dev/go-cache" "$root/.dev/go-mod"
exec docker run --rm --network host --user "$(id -u):$(id -g)" \
  -v "$root:/work" -w /work/server \
  -e GOCACHE=/work/.dev/go-cache -e GOPATH=/work/.dev/go-mod \
  golang:1.27.1-bookworm@sha256:69a7b9788769bec032d238959b61854e9ae87f57be9029ec04e9885fabf99195 "$@"
