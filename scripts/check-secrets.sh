#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
command -v gitleaks >/dev/null || { echo 'Install Gitleaks to run this source validation.' >&2; exit 1; }
mkdir -p "$root/.dev"
scan=$(mktemp -d "$root/.dev/source-scan.XXXXXX")
trap 'rm -rf "$scan"' EXIT
cd "$root"
# Scan versionable source only. Never copy development credentials or caches.
git ls-files --cached --others --exclude-standard -z | tar --null -T - -cf - | tar -xf - -C "$scan"
gitleaks dir "$scan" --config "$root/.gitleaks.toml" --redact --no-banner --report-path "$root/.dev/gitleaks.json"
