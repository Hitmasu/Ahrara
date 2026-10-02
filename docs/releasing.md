# Publishing a client release

The [Client release workflow](../.github/workflows/client-release.yml) builds
Linux, macOS, and Windows binaries for x64 and ARM64, publishes a GitHub Release,
and produces a signed update manifest and Homebrew formula. All build jobs run on
`ubuntu-latest` using `make release`; the relay is deployed separately.

## Prerequisites

- Set the version in `cli/Cargo.toml`, `cli/Cargo.lock`, `plugin.yaml`, and the
  plugin JSON manifests. The workflow checks that manifest versions agree.
- Configure repository variable `AHRARA_UPDATE_PUBLIC_KEY` with the Ed25519
  public key as 64 hexadecimal characters and secret `AHRARA_RELEASE_SIGNING_KEY`
  with its private PEM. Use a dedicated release key, never a user Identity.
- Set repository variable `AHRARA_SERVER_PUBLIC_KEY` to the production relay's
  verified `ahrara_server_v1_...` public pin. Release binaries embed it and connect
  to `wss://ahrara.dev` automatically. The Tunnel must route `ahrara.dev` to the
  relay, whose WebSocket endpoint is `/`.
- Commit `plugin/release-key.json` with `ed25519` set to the same public value as
  `AHRARA_UPDATE_PUBLIC_KEY` before distributing the plugin. Its checked-in null
  value deliberately prevents downloads until a release key is provisioned.
- Run the relevant [checks](../CONTRIBUTING.md#checks), including `make update-test`,
  and validate the binaries on their native platforms.

Only the public key is embedded in binaries. Builds without it cannot check or
apply updates. The manifest endpoint is `https://ahrara.dev/releases/stable.json`.

## Publish

1. Dispatch the workflow with the package version. Set `initial_release=true`
   only for the first publication; later runs verify the previous signed manifest.
2. Review the release and its six binaries. Existing releases are not overwritten.
3. Deploy the `ahrara-static-site` artifact after the binaries are available.
   Serve `/releases/stable.json` with a cache lifetime of at most one hour.
4. Review and commit the generated `Formula/ahrara.rb` from the
   `ahrara-homebrew-formula` artifact or release asset. Review the public key in
   `ahrara-plugin-key` and commit it as `plugin/release-key.json`. The workflow
   does not deploy the site or commit these files.
5. Test Homebrew on the supported macOS/Linux platforms:

   ```sh
   brew tap hitmasu/ahrara https://github.com/Hitmasu/Ahrara.git
   brew install hitmasu/ahrara/ahrara
   brew test hitmasu/ahrara/ahrara
   ```

6. Mark a channel/platform as available only when installable. The workflow's
   `available` input accepts space-separated pairs such as `brew=darwin-aarch64`.

The plugin downloads `stable.json` from the latest GitHub Release, independently
of static-site deployment. Test first installation and a subsequent launch with
the repository plugin before announcing it.

The checked-in bootstrap formula stays disabled until generated release URLs and
checksums replace it. Other package channels need their own implementation and
publication before being advertised.

## Refresh the manifest

Manifests expire after 30 days by default, with a 90-day maximum. Before expiry,
download the existing release's original six binaries into `dist/` and its signed
manifest. With Python 3.11+ and OpenSSL 3, run:

```sh
python3 scripts/release-manifest.py \
  --version <existing-version> --assets dist \
  --key /secure/release-signing.pem \
  --public-key '<64-character-public-key-hex>' \
  --previous /path/to/previous-stable.json \
  --output site/releases/stable.json \
  --homebrew-output Formula/ahrara.rb \
  --plugin-key-output plugin/release-key.json
```

Replace placeholders with the matching release values, then republish the static
manifest and replace the `stable.json` asset on the corresponding latest GitHub
Release so plugin installations also receive the refresh. The tool verifies the previous signature and preserves availability;
use `--available channel=platform` to add a published channel. Changed binary
bytes require a new version.

## Installation ownership and keys

Packages place an `ahrara.install.json` receipt beside the real executable
(`ahrara.exe.install.json` on Windows), for example:

```json
{"schema_version":1,"method":"brew"}
```

The supported receipt methods are `brew`, `npm`, `pip`, `sh`, `powershell`,
`manual`, and `plugin`. Plugin installations update through
`node plugin/ahrara.mjs --update`. Direct installations register through
`ahrara update --register`.
The updater does not replace package-owned or unregistered binaries.

Keep private signing keys out of source, artifacts, and logs. Verification-key
rotation requires a migration release signed by the current key; a compromised
key requires an out-of-band reinstall. The contract is checked in
[release-manifest.py](../scripts/release-manifest.py) and the
[Rust updater](../cli/src/update.rs).
