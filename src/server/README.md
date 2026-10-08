# Ahrara relay

The Rust relay receives SMTP over STARTTLS and streams messages to authenticated
clients over WSS. It keeps no inbox or offline queue. SMTP success requires the
client to acknowledge durable local storage. Messages are limited to 10 MiB.

## Build and run

From the repository root:

```sh
make dev-certs
docker compose build server
AHRARA_MAX_CLIENTS=1000 docker compose up -d --wait server
```

`make dev-certs` builds the static server locally. The Dockerfile only packages
`dist/ahrara-server`; it does not compile source. For production, build and save
the image locally, then transfer it to the VPS and load it with `docker load`.
The VPS needs only the runtime image, Compose configuration, and mounted secrets.

This exposes SMTP on `127.0.0.1:2525` and WSS on `127.0.0.1:8443`. It is a local
development setup; public delivery also requires DNS, MX records, SMTP ingress,
and valid certificates. See [Run locally](../../CONTRIBUTING.md#run-locally) for client setup.

## Configuration

| Environment variable | Default | Purpose |
|---|---|---|
| `AHRARA_SMTP_BIND` | `:2525` | SMTP listener |
| `AHRARA_HTTPS_BIND` | `:8443` | HTTPS/WSS listener |
| `AHRARA_TLS_CERT` | `/certs/server.pem` | Server certificate |
| `AHRARA_TLS_KEY` | `/certs/server-key.pem` | TLS private key |
| `AHRARA_TLS_CA` | `/certs/ca.pem` | CA for the local HTTPS health check |
| `AHRARA_HEALTHCHECK_SERVER_NAME` | `localhost` | Certificate name verified by the local health check |
| `AHRARA_TRANSPORT_KEY_FILE` | `/certs/transport.key` | Noise private key |
| `AHRARA_UNIX_SOCKET` | Unset | Optional HTTP/WebSocket Unix socket |
| `AHRARA_WEB_ORIGINS` | Empty | Browser origins allowed to connect, such as `https://app.ahrara.dev` |
| `AHRARA_TRUSTED_EMAIL_ROUTING_CIDRS` | Empty | Trusted SMTP forwarding sources |
| `AHRARA_EMAIL_ROUTING_ADDRESS` | Unset | Exact SMTP sink for native Email Routing |
| `AHRARA_EMAIL_ROUTING_VERIFICATION_RECIPIENT` | Unset | Temporary Ahrara inbox for verifying the sink |

The `healthcheck` command checks the local HTTPS endpoint. With a public
certificate, set `AHRARA_HEALTHCHECK_SERVER_NAME` to its hostname and mount a CA
bundle at `AHRARA_TLS_CA`. `make dev-certs` creates the Noise key and adds the
public pin to new client profiles. For other deployments:

```sh
ahrara-server transport-keygen /secure/transport.key
ahrara-server transport-public-key /secure/transport.key
```

Both commands print only the public pin. Distribute it independently to clients
as `server_public_key`. Keep the private key read-only to the relay; rotating it
requires updating client pins.

Direct SMTP is routed by `RCPT TO`. Mail from trusted forwarding sources is
routed by its forwarding headers; with `AHRARA_EMAIL_ROUTING_ADDRESS`, only mail
forwarded to that sink is accepted and other sources are refused before TLS.
Bcc recipients are supported. Restrict SMTP ingress to the provider before
enabling its CIDR allowlist; Docker-published ports bypass host firewalls such
as UFW, so filter in the `DOCKER-USER` chain or the provider's firewall.

### Cloudflare Email Routing to a VPS

Keep the MX records for `ahrara.dev` pointing to Cloudflare Email Routing.
The forwarding destination uses a separate subdomain, for example:

| Type | Name | Value | Options |
|---|---|---|---|
| A | `smtp` | Your VPS IPv4 address | DNS only |
| MX | `ingest` | `smtp.ahrara.dev` | Priority 10 |

1. Publish TCP port 25 on the VPS to the container's port 2525. Restrict incoming
   SMTP to the [Email Routing outbound prefixes](https://developers.cloudflare.com/email-service/reference/postmaster/#outbound-prefixes).
   Set those same prefixes in `AHRARA_TRUSTED_EMAIL_ROUTING_CIDRS`.
2. Mount a publicly trusted certificate and key for `smtp.ahrara.dev`, configure
   renewal, and set `AHRARA_HEALTHCHECK_SERVER_NAME=smtp.ahrara.dev`. Reload the
   container after renewal; certificates are loaded at startup.
3. Set `AHRARA_EMAIL_ROUTING_ADDRESS=relay@ingest.ahrara.dev`. Trusted SMTP
   peers may deliver only to this exact sink; the relay refuses other sources.
4. To verify the destination, create a temporary Ahrara address and keep its
   client connected. Set `AHRARA_EMAIL_ROUTING_VERIFICATION_RECIPIENT` to that
   address and restart the relay. During this temporary setup mode, sink mail
   is delivered to that operator inbox for destination verification and inspection
   of the provider's forwarding headers.
5. Add `relay@ingest.ahrara.dev` under Email Routing's destination addresses,
   receive the verification email, and follow its confirmation link.
6. Use the native catch-all action **Send to an email**, targeting that verified
   destination. Remove `AHRARA_EMAIL_ROUTING_VERIFICATION_RECIPIENT` and restart
   the relay to enable delivery to individual Ahrara inboxes. Validate with a real
   forwarded message while its recipient's client is connected.

Public SMTP reception and the client Tunnel are separate connections. No Worker
is used, and the VPS needs neither application source nor a compiler.

### SMTP limits

All values accept integers from 1 to 1024. The per-source cap cannot exceed the
global session cap, and trusted forwarding sources are exempt from it.

| Variable | Default | Limits |
|---|---:|---|
| `AHRARA_SMTP_MAX_SESSIONS` | 256 | Simultaneous SMTP sessions |
| `AHRARA_SMTP_MAX_SESSIONS_PER_SOURCE` | min(16, global cap) | Simultaneous sessions per sender |
| `AHRARA_SMTP_OFFER_TIMEOUT_SECONDS` | 5 | Wait for the client to accept a recipient |
| `AHRARA_SMTP_ACK_TIMEOUT_SECONDS` | 30 | Wait for the client to confirm it saved a message |
| `AHRARA_SMTP_TRANSACTION_TIMEOUT_SECONDS` | 300 | Time a session has to deliver each message |
| `AHRARA_SMTP_COMMAND_TIMEOUT_SECONDS` | 120 | Idle time between SMTP commands |

### Client limits

`AHRARA_MAX_CLIENTS_PER_SOURCE` (default 100, up to 100000) caps the WebSocket
connections of one source, counting unauthenticated ones; beyond it the relay
answers HTTP 429. A source is an IPv4 address or an IPv6 /64: the TCP peer on the
TLS listener, and Cloudflare's `cf-connecting-ip` on the integrated Tunnel and the
Unix socket, which only cloudflared reaches. Keep that header: if a Cloudflare
Transform Rule removes it, connections have no source and are not limited, and the
relay logs `client_source_unknown` once.

`AHRARA_MAX_CLIENTS` (required, up to 10000000) caps the WebSocket connections of
all sources together, including those without a source, so many sources cannot
exhaust the relay's memory. It has no default because it depends on the host's
memory: an idle connection takes about 3 KB, and about 9 KB after delivering mail.

The first quarter of a source's connections is free. Each further connection must
prove work before authenticating: a SHA-256 hash over its login challenge with 12
to 20 leading zero bits, about 4 thousand to 1 million attempts, rising toward the
cap. Clients solve it automatically; it costs a legitimate user nothing until one
address holds many connections. Clients without this support are disconnected
without an error and reconnect later.

### Browser clients

Native clients send no `Origin` header. Browsers always do, so the relay accepts
their WebSocket upgrades only from the exact origins in `AHRARA_WEB_ORIGINS`
(comma-separated, no path or trailing slash) and answers 403 to any other site.
Browsers cannot send WebSocket PINGs; the web client instead sends an empty
`KEEPALIVE` frame, which the relay echoes and counts as liveness. Update the
relay before publishing a web client that sends it.

## Cloudflare Tunnel with WSS and Unix

The optional Compose `tunnel` profile connects a named Tunnel to the relay:

```text
Client → WSS → Cloudflare → encrypted Tunnel → cloudflared → Unix socket → relay
```

1. Configure `relay.ahrara.dev` as the Tunnel's public hostname with origin service
   `unix:/run/ahrara/ahrara.sock`.
2. Store the token in `.dev/cloudflared-token`, owned by the container's UID and
   readable only by that user (`chmod 600 .dev/cloudflared-token`).
3. Run `docker compose --profile tunnel up -d` with `AHRARA_MAX_CLIENTS` set.
4. Clients connect to `wss://relay.ahrara.dev`. Supply the verified
   `server_public_key` when building release binaries or configuring a custom client.

The relay serves WebSocket on the root path `/`, so the Tunnel needs no path rule.
Both containers share `.dev/run` and run as `AHRARA_UID`/`AHRARA_GID` (default
`1000:1000`).

## Experimental integrated Tunnel

The optional `tunnel` Cargo feature runs the named Tunnel inside the relay
process, without a `cloudflared` sidecar. Its edge
client is derived from [libcfd](https://github.com/apersomany/libcfd) by
apersomany, whom we thank for the original connector; see [NOTICE](NOTICE).

```sh
make cargo ARGS='build --locked --release --features tunnel --target x86_64-unknown-linux-musl --manifest-path /work/src/server/Cargo.toml'
cp .dev/target/x86_64-unknown-linux-musl/release/ahrara-server dist/ahrara-server
docker build --build-context ahrara-binary=./dist \
  -f src/server/Dockerfile -t ahrara-server:rust-tunnel .
```

Use the target matching the VPS architecture; these commands target Linux x64.

Mount a named-Tunnel token read-only and set `AHRARA_TUNNEL_TOKEN_FILE` to it.
`AHRARA_TUNNEL_CONNECTIONS` accepts 1–4, defaulting to 4. Keep the TLS and SMTP
configuration. The public hostname must already route to that Tunnel; do not
start the separate `cloudflared` service for this deployment.

The dashboard's origin service URL is ignored in this mode; clients still use
`wss://relay.ahrara.dev`.

The connector speaks HTTP/2 to the edge and serves WebSocket upgrades on `/`
and `GET /healthz`; other requests receive an error status. It is experimental;
local tests do not establish public Tunnel capacity or failover behavior.

## Validation

Run `make check` and `make test` from the repository root. Delivery tests are
listed in [Contributing](../../CONTRIBUTING.md#checks).
