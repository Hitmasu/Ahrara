# Ahrara wire protocol v1

SMTP is STARTTLS-only. The native client connects to `wss://localhost:8443/ws`.
There is no WebSocket compression, subscription frame or alias registry.
The server only holds authenticated routes and bounded in-flight transactions.

## Identity and addresses

An Identity is an Ed25519 key pair. Its private seed never leaves the client.
The fingerprint is lowercase hexadecimal SHA-256 of the 32 public-key bytes.

```
route_id = SHA256(ASCII("ahrara-route-v1") || 0x00 || public_key_raw)[0:16]
alias_id = CSPRNG(8 bytes)
address  = Base64URL_NoPadding(route_id || alias_id) + "@ahrara.dev"
```

The local part is exactly 32 characters representing exactly 24 bytes. Decode
strictly, then re-encode to check canonical form. Preserve local-part case;
normalize only the domain. IDs are byte arrays, not platform-endian integers.
Diagnostic route/alias/transfer strings are lowercase hex (32/16/32 characters).
`vectors.json` is consumed by both Go and Rust tests.

An address is public routing information. An invented alias with a known route
is routed to that Identity and rejected by its local SQLite lookup. It never
grants authentication or the ability to read someone else's messages.

## Binary envelope

Each WebSocket binary message contains exactly one envelope:

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | Version, `1` |
| 1 | 1 | Kind |
| 2 | 4 | Payload length, unsigned big endian |
| 6 | length | Payload |

Controls are UTF-8 JSON, at most 16,384 bytes. Chunks are binary and contain at
most 65,536 EML bytes. The largest envelope is 65,562 bytes. Version, exact
length, direction, state and correlation are validated before processing.

| Kind | Name | Direction | JSON fields / payload |
|---|---|---|---|
| 1 | HELLO | client → server | `public_key`: Base64URL of 32 bytes |
| 2 | CHALLENGE | server → client | `challenge`: Base64URL of 32 random bytes |
| 3 | AUTH | client → server | `signature`: Base64URL of 64 bytes |
| 4 | READY | server → client | `route_id`, `fingerprint` |
| 5 | FAILURE | server → client | `code` |
| 10 | DELIVERY_OFFER | server → client | `transfer_id`, `recipient`, `alias_id`, `envelope_sender`, `declared_size` (bytes or null) |
| 11 | DELIVERY_ACCEPT | client → server | `transfer_id`, `recipient` |
| 12 | DELIVERY_REJECT | client → server | `transfer_id`, `recipient` |
| 20 | EMAIL_START | server → client | `transfer_id`, `recipients` (nonempty array), `envelope_sender`, `declared_size` |
| 21 | EMAIL_CHUNK | server → client | transfer ID: 16 raw bytes; sequence: 4-byte big-endian; nonempty EML bytes |
| 22 | EMAIL_END | server → client | `transfer_id`, `total_bytes`, `chunks`, `sha256` (hex) |
| 23 | EMAIL_ABORT | server → client | `transfer_id` |
| 24 | ACK | client → server | `transfer_id` |
| 25 | NACK | client → server | `transfer_id`, `code` (e.g. `persistence_failed`) |

All strings use JSON escaping. Transfer IDs are 16 CSPRNG bytes and have no
relationship to MIME `Message-ID`. Sequence starts at zero per transfer.

## Authentication

The client signs these exact bytes:

```
ASCII("ahrara-auth-v1") || 0x00 || public_key_raw || challenge
```

The handshake has a 15-second deadline and one signature attempt. Challenges
are per connection. Invalid, expired or replayed responses close the connection.
The server rejects unexpected identity fields, including client-supplied routes.
Only after signature verification does it derive and register the route.

Registration is atomic. A second authenticated connection for the same Identity
receives `identity_in_use`, with no takeover. Cleanup compares session identity
before deleting a map entry. Public key and route alone are never credentials.

WebSocket ping/pong runs every approximately 90 seconds (±20% jitter), regardless
of email activity. Ping/write/commit deadlines are 30 seconds. The client
reconnects at bases 1, 2, 4, 8, 16, 30 seconds (jitter 0.8–1.0), resetting after
a successful connection. Authentication failures and `identity_in_use` are
reported explicitly rather than silently replacing another listener.

## SMTP recipient handshake

At each `RCPT TO`, validate and extract route/alias, look up the authenticated
connection, then offer the recipient. Lookup on the client requires the full
canonical address, active fingerprint and `enabled=1`. The offer deadline is
5 seconds including its write. There is no EML transmission before acceptance.

| Result | SMTP response |
|---|---|
| Invalid token/domain, offline route, rejected alias | `550 5.1.1` |
| Offer timeout or disconnect | `451 4.4.2` |
| Another transaction reserved this Identity | `450 4.2.1` |
| More than 100 distinct recipients | `452 4.5.3` |
| Accepted enabled alias | `250` for RCPT |

The server reserves one transaction per Identity. Several aliases of the same
Identity in that transaction share the reservation; repeated canonical RCPTs
are deduplicated. These temporary recipient lists disappear on completion,
error, timeout, RSET or disconnect. There is no alias cache or message queue.
At most 100 distinct recipients can be offered per transaction. A repeated
accepted RCPT is idempotent; retrying a failed offer requires RSET and a new
MAIL transaction (451 otherwise), so a late reply cannot authorize a new offer
under the same correlation key. This does not limit aliases stored locally.

## Streaming, commit and failure

The server groups accepted recipients by WSS. Each Identity receives the EML
once and only its own recipients, preserving envelope BCC across Identities.
Headers inside the EML remain unchanged.

STARTTLS is required before MAIL/RCPT/DATA. SIZE advertises 10,485,760 bytes.
An excessive declaration is rejected before DATA; actual DATA bytes are also
counted. Exactly 10 MiB is valid. An excess returns 552 and closes the SMTP
connection without unbounded draining. CHUNKING/BDAT is disabled in V1.

One 64 KiB payload buffer is reused by the server, with synchronous fan-out and
backpressure. No complete EML is buffered or spooled by Go. SMTP read timeout
is 2 minutes; total transaction deadline is 5 minutes. SMTP sessions and WSS
connections are bounded (128 / 4096), independently of total aliases created.

The client keeps one temporary EML buffer of at most 10 MiB per active transfer.
It validates transfer, sequence, byte/chunk count and SHA-256, then commits one
row per recipient in a single SQLite transaction. ACK follows commit. Events
are published after commit, including when the network subsequently loses ACK.
Incomplete or corrupt transfers are discarded. MIME parsing cannot change the
original EML and is not a prerequisite for commit.

SMTP 250 after DATA requires ACK from every accepted Identity. NACK, disconnect
or ACK timeout yields temporary failure. Another recipient may already have
committed; retry can duplicate messages. There is no exactly-once guarantee.
Local uniqueness is `(transfer_id, recipient)`, not MIME `Message-ID`.

ABORT cancels both pre-DATA accepted offers and partial EML. Late replies cannot
authorize a closed transfer. Disabling/deleting an alias blocks new offers but
does not revoke an acceptance already granted to an in-flight transaction.
