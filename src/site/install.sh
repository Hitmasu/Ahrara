#!/bin/sh
# Installs the Ahrara CLI on Linux and macOS:
#   curl -fsSL https://ahrara.dev/install.sh | sh
# Downloads the release binary listed in the manifest's direct channel, checks
# its size and SHA-256, and registers the installation for `ahrara update`.
# AHRARA_INSTALL_DIR (default ~/.ahrara/bin) and AHRARA_MANIFEST_URL override
# the defaults.
set -eu

fail() {
	printf 'Ahrara install: %s\n' "$*" >&2
	exit 1
}

fetch() {
	curl --proto =https --tlsv1.2 -fsSL -o "$2" "$1" || fail "cannot download $1"
}

sha256() {
	if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d ' ' -f 1
}

manifest_url=${AHRARA_MANIFEST_URL:-https://github.com/Hitmasu/Ahrara/releases/latest/download/stable.json}
dest=${AHRARA_INSTALL_DIR:-$HOME/.ahrara/bin}
exe=$dest/ahrara

case $(uname -s) in
Linux) os=linux ;;
Darwin) os=darwin ;;
*) fail "unsupported system $(uname -s); on Windows use install.ps1" ;;
esac
case $(uname -m) in
x86_64 | amd64) arch=x86_64 ;;
aarch64 | arm64) arch=aarch64 ;;
*) fail "unsupported architecture $(uname -m)" ;;
esac
# A shell running under Rosetta still gets the native Apple silicon binary.
if [ "$os" = darwin ] && [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
	arch=aarch64
fi
target=$os-$arch

# The CLI records which installer owns it beside the executable.
if [ -f "$exe.install.json" ]; then
	method=$(sed -n 's/.*"method" *: *"\([a-z_]*\)".*/\1/p' "$exe.install.json")
	[ "$method" = sh ] || fail "$exe is managed by ${method:-another installer}; update it there instead"
	exec "$exe" update
fi

work=$(mktemp -d) || fail 'cannot create a temporary directory'
download=
trap 'rm -rf "$work" ${download:+"$download"}' EXIT
trap 'exit 1' HUP INT TERM

fetch "$manifest_url" "$work/stable.json"
payload=$(tr -d ' \r\n' <"$work/stable.json" | sed -n 's|.*"payload":"\([A-Za-z0-9+/=]*\)".*|\1|p')
[ -n "$payload" ] || fail "unexpected manifest at $manifest_url"
printf '%s' "$payload" >"$work/payload.b64"
base64 -d "$work/payload.b64" >"$work/payload.json" 2>/dev/null ||
	openssl base64 -d -A -in "$work/payload.b64" -out "$work/payload.json" ||
	fail 'cannot decode the manifest'

# scripts/release-manifest.py writes compact JSON with sorted keys, so the
# direct channel's artifacts end where its "version" key starts. Other
# channels list the same targets, so only that slice is searched.
json=$(cat "$work/payload.json")
direct=${json#*'"direct":'}
[ "$direct" != "$json" ] || fail 'the manifest has no direct channel'
version=${direct#*'"version":"'}
version=${version%%'"'*}
artifacts=${direct%%'"version":'*}
entry=${artifacts#*"\"$target\":"}
[ "$entry" != "$artifacts" ] || fail "no Ahrara $version release for $target yet"
entry=${entry%%'}'*}
sha=${entry#*'"sha256":"'}
sha=${sha%%'"'*}
size=${entry#*'"size":'}
size=${size%%,*}
url=${entry#*'"url":"'}
url=${url%%'"'*}

mkdir -p "$dest" || fail "cannot create $dest"
download=$(mktemp "$dest/.ahrara.XXXXXX") || fail "cannot write to $dest"
printf 'Downloading Ahrara %s for %s\n' "$version" "$target"
fetch "$url" "$download"
[ "$(wc -c <"$download" | tr -d ' ')" = "$size" ] || fail 'the download size does not match the manifest'
[ "$(sha256 "$download")" = "$sha" ] || fail 'the download checksum does not match the manifest'
chmod 755 "$download"
mv -f "$download" "$exe"
download=
"$exe" update --register sh >/dev/null || fail 'cannot register the installation'
"$exe" --version

case ":${PATH:-}:" in
*":$dest:"*) ;;
*)
	printf '\nAdd Ahrara to your PATH in ~/.profile, ~/.bashrc or ~/.zshrc, then open a new terminal:\n'
	# shellcheck disable=SC2016 # $PATH is meant for the user's shell profile.
	printf '  export PATH="%s:$PATH"\n' "$dest"
	;;
esac
