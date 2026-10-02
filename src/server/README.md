# Ahrara relay

The Rust relay receives SMTP over STARTTLS and streams messages to authenticated
clients over WSS. It keeps no inbox or offline queue. SMTP success requires the
client to acknowledge durable local storage. Messages are limited to 10 MiB.

## Build and run

From the repository root:

```sh
make dev-certs
docker compose build server
docker compose up -d --wait server
```

This exposes SMTP on `127.0.0.1:2525` and WSS on `127.0.0.1:8443`. It is a local
development setup; public delivery also requires DNS, MX records, SMTP ingress,
and valid certificates. See [Contributing](../../CONTRIBUTING.md) for client setup.

## Configuration

| Environment variable | Default | Purpose |
|---|---|---|
| `AHRARA_SMTP_BIND` | `:2525` | SMTP listener |
| `AHRARA_HTTPS_BIND` | `:8443` | HTTPS/WSS listener |
| `AHRARA_TLS_CERT` | `/certs/server.pem` | Server certificate |
| `AHRARA_TLS_KEY` | `/certs/server-key.pem` | TLS private key |
| `AHRARA_TLS_CA` | `/certs/ca.pem` | CA for the local HTTPS health check |
| `AHRARA_TRANSPORT_KEY_FILE` | `/certs/transport.key` | Noise private key |
| `AHRARA_UNIX_SOCKET` | Unset | Optional HTTP/WebSocket Unix socket |
| `AHRARA_TRUSTED_EMAIL_ROUTING_CIDRS` | Empty | Trusted SMTP forwarding sources |

The `healthcheck` command requests local HTTPS `/healthz` with CA validation;
its certificate must cover `localhost`. `make dev-certs` creates the Noise key
and adds the public pin to new client profiles. For other deployments:

```sh
ahrara-server transport-keygen /secure/transport.key
ahrara-server transport-public-key /secure/transport.key
```

Both commands print only the public pin. Distribute it independently to clients
as `server_public_key`. Keep the private key read-only to the relay; rotating it
requires updating client pins. Existing development profiles are preserved, so
older ones may need the pin from `.dev/certs/transport-public.txt`.

Direct SMTP routes by `RCPT TO`. Trusted forwarding instead uses one unambiguous
`X-Forwarded-To` destination. Enable its CIDR allowlist only after restricting
SMTP ingress to the provider and verifying its final forwarding header. Simulated
tests do not validate a provider's live headers.

### SMTP limits

All values accept integers from 1 to 1024. The per-source cap cannot exceed the
global session cap.

| Variable | Default |
|---|---:|
| `AHRARA_SMTP_MAX_SESSIONS` | 256 |
| `AHRARA_SMTP_MAX_SESSIONS_PER_SOURCE` | min(16, global cap) |
| `AHRARA_SMTP_OFFER_TIMEOUT_SECONDS` | 5 |
| `AHRARA_SMTP_ACK_TIMEOUT_SECONDS` | 30 |
| `AHRARA_SMTP_TRANSACTION_TIMEOUT_SECONDS` | 300 |
| `AHRARA_SMTP_COMMAND_TIMEOUT_SECONDS` | 120 |

IPv4-mapped addresses share the IPv4 quota; IPv6 sources share a `/64` quota.
Trusted forwarding sources bypass the per-source cap, but not the global cap.
Three missing delivery ACKs in five minutes trigger a 60-second Identity cooldown.
A successful ACK clears failures; client persistence NACKs do not count as missing
ACKs. Cooldown tracking is bounded to 4,096 routes and evicts the oldest at capacity.

## Cloudflare Tunnel with WSS and Unix

The optional Compose `tunnel` profile connects a named Tunnel to the relay:

```text
Client → WSS → Cloudflare → encrypted Tunnel → cloudflared → Unix socket → relay
```

1. Configure `ahrara.dev` as the Tunnel's public hostname with origin service
   `unix:/run/ahrara/ahrara.sock`.
2. Store the token in `.dev/cloudflared-token`, owned by the container's UID and
   readable only by that user (`chmod 600 .dev/cloudflared-token`).
3. Run `docker compose --profile tunnel up -d`.
4. Clients connect to `wss://ahrara.dev`. Supply the verified
   `server_public_key` when building release binaries or configuring a custom client.

WebSocket connections use the root path `/`. No path rule is needed in the Tunnel;
the relay handles the WebSocket upgrade.

Both containers share `.dev/run` and use `AHRARA_UID`/`AHRARA_GID` (default
`1000:1000`). `make dev-certs` creates the directory with mode `0700`; the relay
creates its socket with mode `0600`. The connector uses the pinned official Cloudflare image.
SMTP ingress remains separate from this Tunnel.

The relay refuses an existing socket path. After an unclean stop, confirm that
the service has stopped and the socket is stale before removing it. Normal
shutdown removes only the socket created by that process.

## Experimental integrated Tunnel

The optional `tunnel` Cargo feature embeds the community `libcfd` connector:

```sh
docker build --build-arg AHRARA_SERVER_FEATURES=tunnel \
  -f src/server/Dockerfile -t ahrara-server:rust-tunnel .
```

Mount a named-Tunnel token read-only and set `AHRARA_TUNNEL_TOKEN_FILE` to it.
`AHRARA_TUNNEL_CONNECTIONS` accepts 1–4, defaulting to 4. Keep the TLS and SMTP
configuration. The public hostname must already route to that Tunnel; do not
start the separate `cloudflared` service for this deployment.

This connector serves the relay's router directly over HTTP/2. It is experimental;
local tests do not establish public Tunnel capacity or failover behavior. See
[vendored sources](third_party/README.md) for its patches and checks.

## Optional SSE

Set `AHRARA_ENABLE_SSE=1` and configure clients with `https://<relay-host>/sse`.
SSE uses the same encrypted protocol and Identity authentication as WSS.
`AHRARA_HTTP2_MAX_STREAMS` defaults to 1024 and accepts 1–16384.
SSE is disabled by default.

## Validation

Run `make check` and `make test` from the repository root. Delivery tests are
listed in [Contributing](../../CONTRIBUTING.md#checks).
