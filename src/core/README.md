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

Run from a Tokio runtime with I/O and timers enabled. The default wait has no
timeout; set `WaitOptions.timeout_ms = Some(milliseconds)` for a finite wait.
`None` in the returned result means an explicit timeout. Dropping a wait future
cancels that wait without disconnecting the inbox. `close().await` interrupts
pending waits and disconnects. Dropping the inbox also requests shutdown; its
encrypted temporary profile is removed after the receiver and storage jobs
release it.

Set `InboxOptions.profile_path` to an absolute directory to preserve identity,
address, encrypted history and consumed-message state across executions.
`export_credentials()` returns secret credentials for restoring the address in
another profile; alternatively supply `recovery_key` to import an identity and
optionally `address` to select an owned alias. WebSocket close waits up to five
seconds for the relay; network failure can delay remote identity release.

`wait_for_emails(options, cancellation_token)` returns a lazy `Stream` with a
total optional duration. `receive_emails(options, cancellation_token, handler)`
awaits each handler in turn. Use `futures_util::pin_mut!` and `StreamExt::next`
to consume the stream without collecting an unbounded inbox.

`InboxOptions.filter` is an admission `EmailFilter`; `WaitOptions.filter` adds
conditions without consuming nonmatches. Set `EmailFilter.regex = true` for
regex conditions. Use `wait_for_regex_match(pattern, options)` for one
`RegexMatch` with named and numbered groups, or
`wait_for_regex_matches(pattern, options, cancel)` for a lazy stream across
emails. `wait_for_xpath` / `wait_for_xpath_values` extract strings from HTML
normalized with an HTML5 parser. `wait_for_query(Query, options)` and
`wait_for_queries(Query, options, cancel)` also expose source/regex flags and
per-email occurrence selection. See the
[shared semantics and limits](../../docs/sdks.md#filters-and-extraction).

`list_emails(Filters)` returns paginated metadata without consuming it.
`get_email(id)`, `raw_email(id)` and `html_email(id, false)` read stored content.
`attachment(id, attachment_id)` returns metadata and bytes; `save_attachment`
writes a new file or uses a sanitized filename in an existing directory.

A source build without the embedded relay public key requires
`InboxOptions.server_public_key`; local test relays also require `server_url`
and their trusted `tls_ca_path`.

Clone the repository beside your application and depend on this checkout with
`ahrara-core = { path = "../Ahrara/src/core" }`. Apply the repository's
`mailparse` patch from [`third_party/`](../third_party/README.md) in your
application's root `Cargo.toml` too; Cargo ignores dependency-local patch
sections:

```toml
[patch.crates-io]
mailparse = { path = "../Ahrara/src/third_party/mailparse" }
```

See [SDK contracts and framework tests](../../docs/sdks.md).
