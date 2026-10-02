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
Linux development requires Docker Engine with Compose, Make, and OpenSSL 3. Make runs the pinned Rust
toolchain in a container. Plugin and release tests also require Node.js 20+ and
Python 3.11+; secret scanning
requires Gitleaks.

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
  --upload-file cli/tests/fixtures/message.eml
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
in [Dockerfile.builder](cli/Dockerfile.builder). Validate release binaries on their
target OS before publishing.

## Checks

Run the checks relevant to your change before submitting a pull request:

| Change | Commands |
|---|---|
| Rust | `make check` and `make test` |
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
respective `third_party/README.md` files.

## Project layout

| Path | Purpose |
|---|---|
| `cli/` | Rust client, encrypted storage, CLI, Web, MCP, and HTTP API |
| `server/` | Rust SMTP receiver and authenticated relay |
| `protocol/vectors.json` | Shared protocol test vectors |
| `docs/` | User and release guides |
| `scripts/` | Release tooling |
| `plugin/`, `skills/` | Shared agent launcher and installation skill |
| `tests/` | Release and plugin tests |
| `site/` | Static landing page; no frontend build required |
| `Formula/` | Homebrew formula |

Preview the site with `python3 -m http.server 8080 --bind 127.0.0.1 --directory site`.
Static hosting publishes the contents of `site/`.

## Pull requests

Describe the problem, resulting behavior, and checks run, including validation
limits. Update documentation when behavior changes. Do not commit generated files,
real email, credentials, or personal profiles. Preserve upstream license notices
and document vendored changes in the corresponding `third_party/README.md`.
Contributions are made under the [MIT License](LICENSE).
