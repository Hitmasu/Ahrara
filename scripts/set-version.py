#!/usr/bin/env python3
"""Set one version in every manifest, lockfile and documented snippet.

The release workflow runs it with the pushed tag's version, so releasing never
needs a version commit first. The current version comes from src/cli/Cargo.toml,
which every other manifest matches (make client-test checks it). Each place below
must match exactly, so a file whose layout changed fails instead of being skipped.
"""
import argparse
import json
from pathlib import Path
import re
import tomllib

ROOT = Path(__file__).resolve().parent.parent
# (file, text around the version as {v}, occurrences)
PLACES = [
    *[(path, '"version": "{v}"', 1) for path in (
        "package.json", "plugin.json", "gemini-extension.json", ".claude-plugin/plugin.json",
        ".codex-plugin/plugin.json", ".devin-plugin/plugin.json", ".qoder-plugin/plugin.json",
        ".github/plugin/plugin.json", "src/cli/npm/package.json", "src/sdks/javascript/package.json")],
    ("plugin.yaml", "\nversion: {v}\n", 1),
    ("openapi.yaml", "\n  version: {v}\n", 1),
    *[(f"src/{crate}/Cargo.toml", '\nversion = "{v}"\n', 1) for crate in ("cli", "core", "native", "server", "web")],
    # Path dependencies on the core and web crates.
    ("src/cli/Cargo.toml", 'version = "{v}" }', 2),
    ("src/native/Cargo.toml", 'version = "{v}" }', 1),
    ("src/web/Cargo.toml", 'version = "{v}" }', 1),
    ("src/sdks/python/pyproject.toml", '\nversion = "{v}"\n', 1),
    ("src/sdks/dotnet/src/Ahrara/Ahrara.csproj", "<Version>{v}</Version>", 1),
    ("src/sdks/java/pom.xml", "<artifactId>ahrara</artifactId>\n  <version>{v}</version>", 1),
    ("src/sdks/k6/go.mod", "github.com/Hitmasu/Ahrara/src/sdks/go v{v}", 1),
    *[(f"src/site/{page}index.html", '<span class="terminal-version">v{v}</span>', 1) for page in ("", "pt-br/", "es/")],
    *[(f"src/site/{page}index.html", "&lt;version&gt;{v}&lt;/version&gt;", 1)
      for page in ("", "pt-br/", "es/", "docs/", "docs/pt-br/", "docs/es/")],
]
CRATES = ("ahrara", "ahrara-core", "ahrara-native", "ahrara-server", "ahrara-web")
LOCKS = [f"src/{crate}/Cargo.lock" for crate in ("cli", "core", "native", "server", "web")]
NPM_LOCK = "src/sdks/javascript/package-lock.json"
FILES = sorted({path for path, _, _ in PLACES} | set(LOCKS) | {NPM_LOCK})


def set_version(root, new):
    if not re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", new):
        raise ValueError(f"{new!r} is not a stable version such as 1.2.3")
    old = tomllib.loads((root / "src/cli/Cargo.toml").read_text())["package"]["version"]
    texts = {path: (root / path).read_text() for path in FILES}

    for path, around, count in PLACES:
        before, after = around.replace("{v}", old), around.replace("{v}", new)
        if texts[path].count(before) != count:
            raise ValueError(f"{path}: expected {count} of {before!r}")
        texts[path] = texts[path].replace(before, after)
    # Only this repository's crates; a dependency may share the version.
    crates = "|".join(re.escape(crate) for crate in CRATES)
    for path in LOCKS:
        texts[path], found = re.subn(rf'(\nname = "(?:{crates})"\nversion = )"{re.escape(old)}"\n',
                                     rf'\1"{new}"\n', texts[path])
        if not found:
            raise ValueError(f"{path}: no crate of this repository at {old}")
    # The package's own entries only, rewritten as npm formats the file.
    lock = json.loads(texts[NPM_LOCK])
    if json.dumps(lock, indent=2, ensure_ascii=False) + "\n" != texts[NPM_LOCK]:
        raise ValueError(f"{NPM_LOCK}: not formatted as npm writes it")
    if lock["version"] != old or lock["packages"][""]["version"] != old:
        raise ValueError(f"{NPM_LOCK}: package version is not {old}")
    lock["version"] = lock["packages"][""]["version"] = new
    texts[NPM_LOCK] = json.dumps(lock, indent=2, ensure_ascii=False) + "\n"

    for path, text in texts.items():
        (root / path).write_text(text)
    return old


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("version", help="stable version without v, e.g. 1.2.3")
    parser.add_argument("--root", type=Path, default=ROOT, help=argparse.SUPPRESS)
    args = parser.parse_args()
    old = set_version(args.root, args.version)
    print(f"Version {old} -> {args.version}")
