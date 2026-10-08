# Contributing to Ahrara

Use English for code, comments, documentation, and user-facing messages, except
for the localized site pages: the Brazilian Portuguese and Spanish documentation
in `src/site/docs/` and landing pages in `src/site/`. Keep changes focused and
follow the surrounding implementation.

Search [issues](https://github.com/Hitmasu/Ahrara/issues) and
[pull requests](https://github.com/Hitmasu/Ahrara/pulls) before starting. Discuss
substantial changes in an issue; small fixes can go straight to a pull request.
Bug reports should include the version, OS, reproduction steps, and expected and
actual behavior. Use synthetic email and sanitized logs. Report vulnerabilities
through [SECURITY.md](SECURITY.md).

## Build and run

Fork and clone the repository, then [run it locally](#run-locally).
Linux development requires Docker Engine with Compose and Make. Make runs the
pinned Rust toolchain, certificate generation, and all tests in containers.
The SDK matrix also downloads container images for each language and browser;
no language SDK or browser installation on the host is needed. Run secret
scanning with the Gitleaks container.

### Run locally

Generate the development certificates and profile, start the relay, then build
and run the Linux development client against that profile:

```sh
make dev-certs
docker compose up -d --build --wait server
make cli
dist/ahrara --config .dev/profile/config.toml
```

Pass the same `--config` to every other command for this profile.

Build outputs go to `dist/`; certificates, profiles, and caches go to `.dev/`.
Both directories are ignored by Git. The default development endpoints are:

| Service | Endpoint |
|---|---|
| SMTP with required STARTTLS | `127.0.0.1:2525` |
| Relay WebSocket | `wss://localhost:8443` |
| Client HTTP service | `http://127.0.0.1:8787` |

`make dev-certs` preserves existing certificates and profiles. New profiles include
`server_public_key`; older local profiles need the public pin from
`.dev/certs/transport-public.txt`. Trust the development CA in clients that connect
to these HTTPS endpoints. Do not use development certificates for public services.

If port 8443 is busy, use `make dev-certs AHRARA_WSS_PORT=18443` and
`AHRARA_WSS_PORT=18443 docker compose up -d --build --wait server`. Update
`server_url` in any existing profile. Set `AHRARA_UID` and `AHRARA_GID` for Compose
if the generated files belong to a user other than `1000:1000`.

With the listener running, copy its address and send a sample message:

```sh
curl --url smtp://localhost:2525 --ssl-reqd --cacert .dev/certs/ca.pem \
  --mail-from sender@example.test --mail-rcpt '<address>' \
  --upload-file src/cli/tests/fixtures/message.eml
```

Stop the client with Ctrl+C and the relay with `docker compose down`.

### Platform builds

All six release binaries are cross-compiled in a Linux x64 Docker container.
GitHub Actions uses `ubuntu-latest` for every build. Run:

```sh
make release PLATFORM=linux ARCH=x86_64
```

Use `PLATFORM=linux`, `darwin`, or `windows` and `ARCH=x86_64` or `aarch64`.
Artifacts are written to `dist/ahrara-<platform>-<arch>` (`.exe` for Windows).
`make cli` builds the Linux development client. Rust is pinned in
[rust-toolchain.toml](rust-toolchain.toml); the release compiler and SDK are pinned
in [Dockerfile.builder](src/cli/Dockerfile.builder). Validate release binaries on their
target OS before publishing.

`make web` builds the browser app into `dist/web` for static hosting. It embeds
the relay URL from `AHRARA_WEB_RELAY_URL` (default `wss://relay.ahrara.dev`) and requires
that relay's `AHRARA_SERVER_PUBLIC_KEY`. The relay must list the app's origin in
`AHRARA_WEB_ORIGINS`. The host must apply `dist/web/_headers` and answer paths
without a file with `index.html`. The pages and routes in `src/web` also serve the
CLI's Web page; `src/web/www` holds the browser's shell, Service Worker and
connection worker. Their text lives in `src/web/src/i18n.rs`, where every string
needs its English, Brazilian Portuguese and Spanish version.

## Checks

Run the checks relevant to your change before submitting a pull request:

| Change | Commands |
|---|---|
| Rust | `make check` and `make test` |
| Embedded SDKs | `make sdk-test` (isolated relay, Docker-only language/framework tests; individual `sdk-*-test` targets are in the Makefile) |
| REST contract | `make openapi` and `make openapi-check`; include `openapi.yaml` |
| Delivery or reconnect | `make e2e` |
| MCP, API, Web, or forwarding | `make mcp-e2e`, `make api-e2e`, `make web-e2e`, or `make forwarding-e2e` |
| Web pages or the browser app | `make web-app-e2e` (Chromium and Firefox in Docker against an isolated relay) |
| Updater or release tooling | `make update-test` |
| Install scripts or release workflow | `make client-lint client-test` (ShellCheck, actionlint and `tests/test_install.py`) |
| SDK release packages | `make sdk-native-release` for each platform, then `make licenses sdk-packages` |
| Agent plugin | `make test` (includes native stdio, launcher, and installer checks) |
| Any contribution | Review the diff and scan for secrets with Gitleaks |

Integration tests need `make dev-certs` and the development relay. Stop the
interactive client before running them. For bug fixes, reproduce the failure with
a regression test when practical. Check documentation links and commands; preview
site changes in a browser. Vendored dependency checks are documented in
[src/third_party/README.md](src/third_party/README.md).

### SDK development

See the [SDK guide](docs/sdks.md) for public behavior and language examples.
Run the complete integration matrix from the repository root:

```sh
make sdk-test
```

This builds the native library and an isolated relay, runs the language and
framework tests in Docker, then stops that relay. Existing services are preserved.
It also tests consuming the generated packages with their bundled native assets.

For a running development relay, use `make sdk-rust-e2e`, `make sdk-dotnet-test`,
`make sdk-java-test`, `make sdk-javascript-test`, `make sdk-python-test`,
`make sdk-go-test`, or `make sdk-k6-test`. The [Makefile](Makefile) owns setup,
formatting, type checks, packaging, and test invocation for each language.
Use its `AHRARA_TEST_SERVER_URL`, `AHRARA_TEST_SMTP_ADDRESS`,
`AHRARA_TEST_SERVER_PUBLIC_KEY`, and `AHRARA_TEST_TLS_CA` settings for a custom
relay where supported; paths must resolve inside the containers.

Rust uses `ahrara-core` directly. Other bindings call the versioned
[C interface](src/native/include/ahrara.h). Native packages must match the OS,
architecture, and C runtime. `AHRARA_NATIVE_LIBRARY` is an absolute-path
development override pointing to a compiled library, not a CLI executable.
Release builds embed the verified relay public key using
`AHRARA_SERVER_PUBLIC_KEY`.

| Package | Native asset location |
|---|---|
| .NET | `src/sdks/dotnet/runtimes/<rid>/native/` |
| JavaScript | `src/sdks/javascript/native/<platform>-<arch>/` |
| Python | `src/sdks/python/ahrara/native/` |
| Java | `src/sdks/java/src/main/resources/native/<platform>-<arch>/` |
| Go / k6 | None: downloaded from the GitHub Release on first use |

`make sdk-native-release PLATFORM=<platform> ARCH=<arch>` builds the release
library as `dist/ahrara-native-<platform>-<arch>.<so|dylib|dll>`. Linux builds
target glibc 2.17. `make sdk-stage` copies all six into the layouts above, and
`make sdk-packages` builds the npm package, one Python wheel per platform and the
NuGet package in `dist/packages/`. Both need `make licenses`, whose third-party notices
ship in every package. The Go SDK bundles nothing: the release workflow tags a
commit pinning each library's SHA-256, and the SDK downloads and verifies its
platform's library on first use. Its tests and source checkouts set
`AHRARA_NATIVE_LIBRARY`. The Rust SDK is consumed as source. `make sdk-test` runs
on Linux x64, and the release workflow loads every library on its own platform
before publishing. Generated packages, binaries, profiles, and logs belong in ignored
directories.

## Project layout

| Path | Purpose |
|---|---|
| `src/core/` | Shared Rust transport, identity, encrypted storage, and embedded inbox |
| `src/native/` | Shared core exposed through an owned-string C ABI |
| `src/sdks/` | Language bindings and framework integration tests |
| `src/cli/` | CLI, Web, MCP, and HTTP API built on the shared core |
| `src/web/` | Web UI pages, routes and assets shared by the CLI and the browser app |
| `src/server/` | Rust SMTP receiver and authenticated relay |
| `src/third_party/` | Shared vendored mail parser |
| `docs/` | User and release guides |
| `scripts/` | Release tooling |
| `src/plugin/`, `skills/` | Shared agent launcher and installation skill |
| `tests/` | Release, plugin and site tests, and the protocol vectors shared by the relay and client |
| `src/site/` | Static landing page, documentation and CLI installers; no frontend build required |
| `Formula/` | Homebrew formula |

Keep implementation in `src/`, with each component's manifests, assets and tests
alongside its source. Root-level files own repository configuration, documentation,
release tooling and cross-component tests. Agent discovery files remain in their
required locations; OpenCode delegates to `src/plugin/opencode.mjs`.

Use each language's formatter, and separate functions, type implementations and
distinct processing steps with a blank line. Keep related declarations and
operations together. Expand long macros and attributes without changing string
contents or introducing helpers solely to wrap lines.

Preview the site with `python3 -m http.server 8080 --bind 127.0.0.1 --directory src/site`.
Static hosting publishes the contents of `src/site/`, including the CLI installers
`install.sh` and `install.ps1`. The host must apply `src/site/_headers`, which serves
them as plain text and keeps `releases/stable.json` cacheable for at most five minutes.

The landing page and the documentation each exist in English, Brazilian Portuguese
and Spanish. The pages of each part share one stylesheet and one script, and both
parts load `src/site/listbox.js`, the icon dropdown of the installers and agent picker:

| Part | English | Brazilian Portuguese | Spanish | Shared |
|---|---|---|---|---|
| Landing page | `src/site/index.html` | `src/site/pt-br/index.html` | `src/site/es/index.html` | `src/site/styles.css`, `src/site/script.js` |
| Documentation | `src/site/docs/index.html` | `src/site/docs/pt-br/index.html` | `src/site/docs/es/index.html` | `src/site/docs/styles.css`, `src/site/docs/script.js` |

Each page is complete static HTML and stays readable without JavaScript. Localized
pages mirror the English structure and element IDs, so the language menu (plain
links) keeps the reader's place and deep links work in every language; they reach
shared files and assets with `../`. The documentation is a single page per
language: the URL hash selects which `<article class="page">` is shown. The docs
script adds navigation, search, synchronized example tabs, the theme toggle and
copy buttons.

The scripts contain no user-visible text. Each page declares its interface strings
in a `<script type="application/json" id="ui-strings">` block: translate the
values and keep the keys and placeholders such as `{page}` and `{count}`. Translate
the rest of the HTML too, including `<title>`, meta descriptions, `aria-label`,
`title`, `alt`, `data-label` and `data-title` attributes, and comments in code
examples; keep commands, code and identifiers unchanged. Update all three versions
when changing content, and keep examples aligned with `docs/` and the SDK guides.
No build, backend, or external asset service is required.

Each documentation language has its own `architecture.mmd`, the editable Mermaid
source for its checked-in `architecture.svg`. After changing a diagram, render it
with Mermaid CLI 12.0.0 (`mmdc -i architecture.mmd -o architecture.svg -b transparent`)
and preview the result. The site serves the SVG directly, including without JavaScript; Mermaid
is only needed when editing the diagram, not for hosting or browsing the docs.

For the documentation browser smoke check, open the local site with Playwright CLI
and run the existing script from the repository root:

```sh
playwright-cli open http://127.0.0.1:8080/docs/
playwright-cli run-code --filename=tests/site.smoke.js
```

This checks the landing page's installers, language menu and links into the docs;
every docs page, synchronized tabs, the agent picker, the clipboard, search, the theme toggle, mobile navigation and the language menu. For
each language present, it compares element IDs and interface keys with English,
checks the `lang` attribute and shared files, and reads the page without
JavaScript. The server must serve `src/site/` at its root. Browser outputs belong
in ignored development directories.

## Pull requests

Describe the problem, resulting behavior, and checks run, including validation
limits. Update documentation when behavior changes. Do not commit generated files,
real email, credentials, or personal profiles. Preserve upstream license notices
and document vendored changes in `src/third_party/README.md`. Changes to the
libcfd-derived Tunnel connector in `src/server/src/tunnel/` keep their file
notices and are summarized in `src/server/NOTICE`.
Contributions are made under the [MIT License](LICENSE).
