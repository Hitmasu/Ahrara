# Publishing a release

One `vX.Y.Z` tag publishes the CLI and every SDK through the
[Client release workflow](../.github/workflows/client-release.yml). The relay is
deployed separately.

| Job | What it does |
|---|---|
| `build` | Builds the CLI (`make release`) and the SDKs' native core (`make sdk-native-release`) for Linux, macOS and Windows on x64 and ARM64. The Linux x64 leg also runs `make client-lint client-test`, writes the third-party licenses (`make licenses`) and builds the browser app (`make web`). |
| `sdk-packages` | Builds the npm package, one Python wheel per platform and the NuGet package, each with its native libraries (`make sdk-packages`). |
| `smoke` | On a runner of each platform, runs the CLI and loads the native core from its Python wheel. Nothing is published unless all six pass. |
| `publish` | Creates the GitHub Release with the CLI binaries, the native libraries, the third-party licenses, the signed `stable.json` and the Homebrew formula. |
| `installers` | Installs the new release with `install.sh` and `install.ps1` on each platform. |
| `sdk-publish` | Publishes to NuGet, PyPI and npm. |
| `maven` | Publishes `dev.ahrara:ahrara` to Maven Central. |
| `go` | Tags `src/sdks/go/vX.Y.Z` and `src/sdks/k6/vX.Y.Z` on a commit, outside `main`, that pins the SHA-256 of each native library in the release. The Go SDK downloads its library from there. |

## Prerequisites

- The repository must be public. The installers, the plugin, Homebrew and the Go
  SDK download from it, npm provenance links to it, and the workflow's ARM runners
  are free only for public repositories.
- Set the same version in:
  - `src/cli/Cargo.toml` and `src/cli/Cargo.lock`, and `src/native/Cargo.toml` and
    `src/native/Cargo.lock`
  - `plugin.yaml`, `plugin.json`, `package.json`, `gemini-extension.json`,
    `.claude-plugin/plugin.json`, `.codex-plugin/plugin.json`,
    `.github/plugin/plugin.json`, `.devin-plugin/plugin.json` and
    `.qoder-plugin/plugin.json`
  - `src/sdks/javascript/package.json` and its `package-lock.json`,
    `src/sdks/python/pyproject.toml`, `src/sdks/dotnet/src/Ahrara/Ahrara.csproj`,
    `src/sdks/java/pom.xml`, and the Go SDK version required by
    `src/sdks/k6/go.mod`

  `make client-test`, the workflow and `scripts/release-manifest.py` reject a
  version that differs from `src/cli/Cargo.toml`.
- Set two repository variables:
  - `AHRARA_UPDATE_PUBLIC_KEY`: the Ed25519 public key of a dedicated release key,
    as 64 hexadecimal characters. Never use a user Identity.
  - `AHRARA_SERVER_PUBLIC_KEY`: the production relay's verified
    `ahrara_server_v1_...` pin. Release binaries and native libraries embed it and
    connect to `wss://relay.ahrara.dev`. The Tunnel must route `relay.ahrara.dev`
    to the relay, whose WebSocket endpoint is `/`, while `ahrara.dev` serves the
    static site.
- Store the secrets below in the `release` environment, not in the repository. The
  workflow runs from the pushed tag, so anyone who can push a tag could otherwise
  change it to read them. Require reviewers for the environment, limit its
  deployments to `v*` tags, and protect those tags with a ruleset. Leave the
  `src/sdks/*` tags that the workflow creates outside the ruleset.

  | Secret | Value |
  |---|---|
  | `AHRARA_RELEASE_SIGNING_KEY` | The release key's private PEM |
  | `NUGET_USER` | The nuget.org profile name (not the email) that owns `Ahrara` |
  | `NPM_TOKEN` | Only for the first release: npm granular access token that can publish to `@ahrara` |
  | `MAVEN_CENTRAL_USERNAME`, `MAVEN_CENTRAL_PASSWORD` | Central Portal user token |
  | `MAVEN_GPG_PRIVATE_KEY`, `MAVEN_GPG_PASSPHRASE` | ASCII-armored signing key and its passphrase. Publish the public key to a keyserver. |

- Create the registry owners: the npm organization `ahrara`, PyPI and nuget.org
  accounts, and the Central Portal namespace `dev.ahrara`, verified with a DNS TXT
  record on `ahrara.dev`. Each name is claimed by its first upload.
- NuGet, PyPI and npm publish through trusted publishing, with no stored token.
  Each trusts the repository owner and name, the workflow file `client-release.yml`
  and the environment `release`:
  - On nuget.org, add a Trusted Publishing policy under your username, scoped to
    push new packages and versions matching `Ahrara`. A policy for a private
    repository stays active for 7 days until its first publish.
  - On pypi.org, add a pending publisher for the project `ahrara` under Publishing
    in your account. It becomes the project's trusted publisher on the first upload,
    but does not reserve the name until then.
  - npm only configures existing packages, so the first release publishes
    `@ahrara/sdk` with `NPM_TOKEN`. Then add a GitHub Actions trusted publisher in
    the package settings on npmjs.com and allow direct publishing: configurations
    stage packages by default, and a staged version waits for approval on
    npmjs.com. Finally, delete `NPM_TOKEN` and set the package to require
    two-factor authentication and disallow tokens. npm tries trusted publishing
    before the token, so the order of these steps is safe.
- Commit `src/plugin/release-key.json` with `ed25519` set to the same value as
  `AHRARA_UPDATE_PUBLIC_KEY` before tagging, because plugins install from the
  default branch. Its checked-in null value deliberately prevents downloads until a
  release key is provisioned.
- Run the relevant [checks](../CONTRIBUTING.md#checks), including `make update-test`
  and `make sdk-test`.

The browser app is built by the same workflow as artifact `ahrara-web-app`, with
the relay key from `AHRARA_SERVER_PUBLIC_KEY`. Before publishing it, set
`AHRARA_WEB_ORIGINS=https://app.ahrara.dev` on the relay and deploy the relay.
Publish the artifact's contents at the root of `https://app.ahrara.dev` on a host
that applies its `_headers` file, such as Cloudflare Pages, and serves
`index.html` for paths without a file.

Only the public key is embedded in binaries. Builds without it cannot check or
apply updates. The manifest endpoint is `https://ahrara.dev/releases/stable.json`.

## Publish

1. Push a tag `vX.Y.Z` matching the package version (`git tag v0.1.0 && git push
   origin v0.1.0`) and approve the `release` environment when the workflow asks.
   Every version tag publishes a release. A tag push treats the
   run as the first publication only when the repository has no release yet; later
   runs verify the previous signed manifest. You can still dispatch the workflow
   manually with the version, `initial_release` and `available` inputs.
2. Review the release: six CLI binaries, six `ahrara-native-*` libraries,
   `ahrara-third-party-licenses.md`, `stable.json` and `ahrara.rb`. Existing
   releases are not overwritten, and registries never accept a version twice. A
   failed `sdk-publish`, `maven`, `go` or `installers` job can be re-run. Each
   skips what is already published.
3. Deploy the `ahrara-static-site` artifact to the host of `https://ahrara.dev`,
   such as Cloudflare Pages (`npx wrangler pages deploy <artifact-directory>
   --project-name <project>`). It contains `install.sh`, `install.ps1` and the
   generated `releases/stable.json`, so deploy the artifact rather than the
   repository. The host must apply its `_headers` file.
4. Review and commit the generated `Formula/ahrara.rb` from the
   `ahrara-homebrew-formula` artifact or release asset, and check that
   `ahrara-plugin-key` matches `src/plugin/release-key.json`. The workflow does not
   deploy the site or commit these files.
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
checksums replace it.

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
  --output src/site/releases/stable.json \
  --homebrew-output Formula/ahrara.rb \
  --plugin-key-output src/plugin/release-key.json
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
`node src/plugin/ahrara.mjs --update`. Direct installations register through
`ahrara update --register`, which `install.sh` and `install.ps1` run with `sh` and
`powershell`.
The updater does not replace package-owned or unregistered binaries.

Keep private signing keys out of source, artifacts, and logs. Verification-key
rotation requires a migration release signed by the current key; a compromised
key requires an out-of-band reinstall. The contract is checked in
[release-manifest.py](../scripts/release-manifest.py) and the
[Rust updater](../src/cli/src/update.rs).
