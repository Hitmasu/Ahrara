# Server dependencies

## libcfd 0.2.0

`libcfd/` vendors the [published crate](https://crates.io/crates/libcfd/0.2.0)
from [apersomany/libcfd](https://github.com/apersomany/libcfd), retaining its
[Apache-2.0 license](libcfd/LICENSE). It is a community connector, compiled only
with Ahrara's optional `tunnel` feature.

- Archive SHA-256: `1ce50b0e625cf0ea8a9d482889487d247663b786b2db22e3ee3516b557ce0e67`.
- Packaged revision: `427093dbf2ee719313286c13ad5347d46e300c93`
  (the upstream archive marks this revision dirty).
- `libcfd-rpc 0.2.0` is pinned by the server's `Cargo.lock`.

### Local changes

- Direct WebSocket stream handoff and smaller HTTP copy buffers.
- Reaping completed tasks, prompt cancellation, correct chunk offsets, and
  bidirectional cleanup on proxy errors.
- Multiple registration indices, edge rotation across DNS groups, bounded retry
  backoff, and concurrent registration/shutdown handling.
- Correct HTTP/2 response headers and low-latency TCP connections.
- Correct DNS SRV labels and bounded DNS compression/name parsing.
- Structured connection diagnostics; Ahrara discards arbitrary strings and
  credential-bearing fields.
- Embedded public CA renamed to `.crt` so Git's private PEM ignore rule retains it.

### Validation

Server integration is covered by `make check` and `make test`. Run the vendored
library's offline tests for the features used by Ahrara with:

```sh
make cargo ARGS='test --locked --manifest-path /work/src/server/third_party/libcfd/Cargo.toml --no-default-features --features named-tunnel,h2-edge,axum-origin --all-targets'
```

These tests use synthetic credentials and local TLS/HTTP2 fixtures. They do not
validate public Cloudflare behavior, QUIC, or geographic failover. Preserve
upstream licenses and the regression tests when updating this fork.
