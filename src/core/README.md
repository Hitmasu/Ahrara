# Ahrara Rust SDK

Create an email address from a Rust application or test, wait for messages, and
extract verification codes or links. Use `ahrara-core` with a Tokio runtime.
The SDK connects directly to the relay without a running Ahrara CLI.

```rust,no_run
use ahrara_core::{Inbox, InboxOptions, WaitOptions};

# async fn example() -> anyhow::Result<()> {
let inbox = Inbox::create(InboxOptions::default()).await?;
// Trigger the application under test using inbox.address().
let email = inbox.wait_for_email(WaitOptions::default()).await?;
assert!(email.is_some());
inbox.close().await;
# Ok(())
# }
```

Run from a Tokio runtime with I/O and timers enabled. Creation returns only
after authentication succeeds and reception is active. Messages received before
waiting remain available. Each match is consumed once; unmatched messages remain
available to later waits. The default wait has no timeout; set
`WaitOptions.timeout_ms = Some(milliseconds)` for a finite wait. `None` in the
returned result means an explicit timeout. Dropping a wait future cancels
that wait without disconnecting the inbox. `close().await` interrupts pending
waits and disconnects. Dropping the inbox also requests shutdown; its encrypted
temporary profile is removed after the receiver and storage jobs release it.

Set `InboxOptions.profile_path` to an absolute directory to preserve identity,
address, encrypted history and consumed-message state across executions. Close
the inbox before reopening that profile. `export_credentials()` returns secret
credentials for restoring the address in another profile; alternatively supply
`recovery_key` to import an identity and optionally `address` to select an owned
alias. A Recovery Key cannot replace a different identity in an existing profile.
WebSocket close waits up to five seconds for the relay; network failure can delay
remote identity release, and SSE teardown has no remote-close acknowledgement.

`wait_for_emails(options, cancellation_token)` returns a lazy `Stream` with a
total optional duration. `receive_emails(options, cancellation_token, handler)`
awaits each handler sequentially and propagates errors. Cancellation and early
stream termination leave the inbox available. Use `futures_util::pin_mut!` and
`StreamExt::next` to consume the stream without collecting an unbounded inbox.

`InboxOptions.filter` is an admission `EmailFilter`: rejected deliveries never
enter storage. `WaitOptions.filter` adds conditions without consuming nonmatches.
Set `EmailFilter.regex = true` to treat string conditions and header values as
case-insensitive Rust regex patterns. `recipient`, `envelope_sender` and
`message_id` must match the whole value; other fields search, so use `^` / `$`
for full matches there. Use `(?-i)` for case sensitivity. The default remains
literal matching. Invalid patterns are rejected before inbox setup or a receive.
Use `wait_for_regex_match(pattern, options)` for one `RegexMatch` with named and
numbered groups, or `wait_for_regex_matches(pattern, options, cancel)` for a lazy
stream across emails. `wait_for_xpath` / `wait_for_xpath_values` extract strings
from HTML normalized with an HTML5 parser. Their default wait is indefinite.
`wait_for_query(Query, options)` and `wait_for_queries(Query, options, cancel)`
also expose source/regex flags and per-email occurrence selection. See the
[shared semantics and limits](../../docs/sdks.md#filters-and-extraction).

`list_emails(Filters)` returns paginated metadata without consuming it.
`get_email(id)`, `raw_email(id)` and `html_email(id, false)` read stored content.
`attachment(id, attachment_id)` returns metadata and bytes; `save_attachment`
writes a new file or uses a sanitized filename in an existing directory.
`delete_email(id)` and `clear_emails()` affect only this inbox's address. Existing
attachment files are never overwritten. Every operation reports failures as
`anyhow::Result`; list/read operations do not change the receive cursor.

Release builds embed the verified relay public key. A source build without that
key requires `InboxOptions.server_public_key`; local test relays also require
`server_url` and their trusted `tls_ca_path`. The SDK never trusts a key supplied
by the connection itself and never reads a user's CLI profile.

Clone the repository beside your application and depend on this checkout with
`ahrara-core = { path = "../Ahrara/src/core" }`. Apply the repository's
`mailparse` patch from [`third_party/`](../third_party/README.md) in your
application's root `Cargo.toml` too; Cargo ignores dependency-local patch
sections:

```toml
[patch.crates-io]
mailparse = { path = "../Ahrara/src/third_party/mailparse" }
```

Until the patched dependency has a published release, the Rust SDK is
distributed as source with this repository, not as an independently published
crates.io package.

All builds and tests use Docker:

```sh
make cargo ARGS='test --locked --manifest-path /work/src/core/Cargo.toml'
make sdk-rust-e2e
```

See [SDK contracts and framework tests](../../docs/sdks.md).
