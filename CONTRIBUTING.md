# Contributing to Ahrara

Use English for code, comments, documentation, and user-facing messages. Keep
changes focused and follow the surrounding implementation.

Search [issues](https://github.com/Hitmasu/Ahrara/issues) and
[pull requests](https://github.com/Hitmasu/Ahrara/pulls) before starting. Discuss
substantial changes in an issue; small fixes can go straight to a pull request.
Bug reports should include the version, OS, reproduction steps, and expected and
actual behavior. Use synthetic email and sanitized logs. Report vulnerabilities
through [SECURITY.md](SECURITY.md).

## Build and run

Fork and clone the repository, then follow the [local setup](README.md#-install).
Linux development requires Docker Engine with Compose and Make. Make runs the
pinned Rust toolchain, certificate generation, and all tests in containers.
The SDK matrix also downloads container images for each language and browser;
no language SDK or browser installation on the host is needed. Run secret
scanning with the Gitleaks container.

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
`AHRARA_WSS_PORT=18443 docker compose up -d --wait server`. Update `server_url` in
any existing profile. Set `AHRARA_UID` and `AHRARA_GID` for Compose if the generated
files belong to a user other than `1000:1000`.

With the listener running, copy its address and send a sample message:

```sh
curl --url smtp://localhost:2525 --ssl-reqd --cacert .dev/certs/ca.pem \
  --mail-from sender@example.test --mail-rcpt '<address>@ahrara.dev' \
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

## Checks

Run the checks relevant to your change before submitting a pull request:

| Change | Commands |
|---|---|
| Rust | `make check` and `make test` |
| Embedded SDKs | `make sdk-test` (isolated relay, Docker-only language/framework tests; individual `sdk-*-test` targets are in the Makefile) |
| REST contract | `make openapi` and `make openapi-check`; include `openapi.yaml` |
| Delivery or reconnect | `make e2e` |
| MCP, API, Web, or forwarding | `make mcp-e2e`, `make api-e2e`, `make web-e2e`, or `make forwarding-e2e` |
| Updater or release tooling | `make update-test` |
| Agent plugin | `make test` (includes native stdio, launcher, and installer checks) |
| Any contribution | Review the diff and scan for secrets with Gitleaks |

Integration tests need `make dev-certs` and the development relay. Stop the
interactive client before running them. For bug fixes, reproduce the failure with
a regression test when practical. Check documentation links and commands; preview
site changes in a browser. Vendored dependency checks are documented in their
respective `src/third_party/README.md` files.

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
| Go / k6 | `src/sdks/go/native/<goos>-<goarch>/`, staged by `make sdk-go-package` |

Packages for the language bindings must include their native assets; the Rust
SDK is consumed as source. Linux x64 is the currently validated SDK platform.
Validate each additional target natively before advertising it. Generated
packages, binaries, profiles, and logs belong in ignored directories.

## Project layout

| Path | Purpose |
|---|---|
| `src/core/` | Shared Rust transport, identity, encrypted storage, and embedded inbox |
| `src/native/` | Shared core exposed through an owned-string C ABI |
| `src/sdks/` | Language bindings and framework integration tests |
| `src/cli/` | CLI, Web, MCP, and HTTP API built on the shared core |
| `src/server/` | Rust SMTP receiver and authenticated relay |
| `src/third_party/` | Shared vendored mail parser |
| `protocol/vectors.json` | Shared protocol test vectors |
| `docs/` | User and release guides |
| `scripts/` | Release tooling |
| `src/plugin/`, `skills/` | Shared agent launcher and installation skill |
| `tests/` | Release and plugin tests |
| `src/site/` | Static landing page; no frontend build required |
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
Static hosting publishes the contents of `src/site/`.

## Pull requests

Describe the problem, resulting behavior, and checks run, including validation
limits. Update documentation when behavior changes. Do not commit generated files,
real email, credentials, or personal profiles. Preserve upstream license notices
and document vendored changes in the corresponding `src/third_party/README.md`.
Contributions are made under the [MIT License](LICENSE).
