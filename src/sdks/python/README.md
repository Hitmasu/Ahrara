# Ahrara for Python

Create an email address from a Python application or test, wait for messages,
and extract verification codes or links. Requires Python 3.11+ and provides
synchronous and asynchronous APIs, plus a Robot Framework library. The SDK
connects directly to the relay without a running Ahrara CLI.

```python
from ahrara import create_inbox

with create_inbox() as inbox:
    # Submit inbox.address in your application.
    email = inbox.wait_for_email()  # No implicit timeout.
```

```python
from ahrara import create_inbox_async

async def verify_signup():
    async with await create_inbox_async() as inbox:
        # Submit inbox.address in your application.
        email = await inbox.wait_for_email()
        return email
```

Creation waits for authentication/subscription. Reception continues before the
first wait. Matching receives are consumed once, while stored messages remain
available until deleted. Use separate inboxes for parallel tests. Concurrent waits
on one inbox are queued and run one at a time. Always close the inbox or use its
context manager.

## Waiting and continuous reception

`wait_for_email(timeout=None, subject=None, from_address=None)` waits indefinitely
by default. A positive finite timeout is in **seconds** and raises `TimeoutError`.
Filters use the core's ASCII case-insensitive substring matching. Other native
failures expose `AhraraError.code`. The sync API also accepts a `threading.Event`
as `cancel`; cancellation raises `AhraraError` with code `cancelled`. Async task
cancellation raises `asyncio.CancelledError`. Both preserve the inbox for reuse.

```python
for email in inbox.wait_for_emails(timeout=10):
    verify(email)
    if finished():
        break

inbox.receive_emails(verify, timeout=10)
```

Async inboxes expose the same stream and callback methods:

```python
from contextlib import aclosing

async with aclosing(inbox.wait_for_emails(timeout=10)) as emails:
    async for email in emails:
        await verify(email)
        if finished():
            break

await inbox.receive_emails(verify, timeout=10)
```

A finite stream timeout is the **total duration**, including consumer/callback
work, and ends iteration normally. Omit it for an indefinite subscription.
Callbacks execute sequentially; async receivers await async callbacks and also
accept synchronous ones. Callback errors propagate. Breaking, closing a generator,
normal completion, and cancellation keep the inbox open. No messages are prefetched
while the consumer handles the previous message. Cancel an active async `anext()`
task before closing its generator; synchronous waits accept a cancellation event.

Repeated cancellation cannot abandon native creation or cleanup. Cancelled
creation waits for its bounded connection attempt, then closes any result.
A concurrently committed result may be delivered once before a stream propagates
cancellation; cancellation never starts another receive.

## Filters and extracted values

`EmailFilter` is an optional TypedDict accepted by creation and wait/search calls.
At creation, `filter` discards rejected arrivals before storage. On individual
waits, it leaves rejected messages available for a later wait. Every field is
ANDed: `sender`, `subject`, `to`, `cc`, `text`, and `headers={name: value}` match
case-insensitive substrings. `recipient`, `envelope_sender`, and `message_id`
match the whole value, ignoring case. `has_attachments` selects presence or
absence; header names ignore case. Per-wait criteria add to the global receipt
filter.

Use `filter={"regex": True, "subject": r"^(Verification|Confirm email)$"}`
to interpret all string conditions as case-insensitive Rust regex patterns.
`recipient`, `envelope_sender`, and `message_id` patterns must match the whole
value; other patterns match anywhere, so use `^` / `$` for a full match. Use
`(?-i)` for case sensitivity; regex queries below are case-sensitive unless
`ignore_case=True`. Header names and the `subject`/`from_address` wait arguments
stay literal. Invalid patterns fail before consuming mail; omitting `regex` keeps
literal matching. Robot Framework accepts the same dictionary, for example
`&{filter}=    Create Dictionary    regex=${TRUE}    subject=^Verify$`. See the
[shared filter rules](../../../docs/sdks.md#filters-and-extraction).

```python
match = inbox.wait_for_regex_match(
    r"code=(?<code>[0-9]+)",
    filter={"sender": "example.test", "subject": "Verify"},
    ignore_case=True,
)
code = next(group["value"] for group in match["groups"] if group.get("name") == "code")

for match in inbox.wait_for_regex_matches(r"[0-9]+", timeout=10):
    verify_code(match["value"])

link = inbox.wait_for_xpath('//a[@id="verify"]/@href')
for value in inbox.wait_for_xpath_values("//a/@href", timeout=10):
    verify_link(value)
```

`AsyncInbox` provides the same singular methods with `await` and plural methods
with `async for`. Regex uses shared [Rust syntax](https://docs.rs/regex/latest/regex/#syntax),
including named captures; lookaround/backreferences are unsupported. Options are
`source="text"` (full decoded text), `"html"`, `"subject"`, or `"raw"`, plus
`ignore_case`, `multi_line`, and `dot_matches_new_line`. Results contain
`email_id`, `value`, and `groups`; group zero is the full match and optional
unmatched groups contain `None`. XPath evaluates the original HTML through the
shared core, including repaired HTML documents, and returns strings.
XPath expressions are limited to 32 nested groups and 256 syntax tokens;
quoted literals do not count their contents as syntax.

Singular searches throw `TimeoutError` on finite expiry. Plural searches yield
**all matches/values across multiple emails** and end normally at their total
deadline, including consumer work. None/omitted timeout is indefinite. Invalid
queries raise `AhraraError` before consuming messages. Sync cancellation uses
`cancel=threading.Event()`; async cancellation uses the task. Closing a generator
preserves the inbox. Each matched email is consumed once, so stopping midway
through its results does not replay unused values; stored mail remains readable.

## Persistence and credentials

Pass `profile_path='/absolute/profile'` to retain identity, address, encrypted
history, and consumed-receive state across reopen. Omit it for an isolated
temporary profile. The same active profile cannot be opened twice.

```python
credentials = inbox.export_credentials()  # Await this for AsyncInbox.
inbox.close()
with create_inbox(credentials=credentials) as restored:
    email = restored.wait_for_email(timeout=10)
```

Exported `InboxCredentials(address, recovery_key)` restores identity/address with
fresh local history unless a persistent profile is supplied. Its repr is redacted;
access `recovery_key` only for deliberate secure storage and never log it.
Alternatively pass `recovery_key` with optional owned `address`; these options
cannot be combined with `credentials`. An `address` alone needs an existing
profile identity. Mismatched keys or unowned addresses fail.

Connection options are `server_url`, `server_public_key`, `tls_ca_path`, and
`connect_timeout_ms` (1..300000 ms, core default 20000). Production defaults require
a core release embedding the independently verified production Noise pin. Custom
CA files keep peer and hostname verification enabled.

## Email CRUD and attachments

All operations are scoped to the inbox's address and have async counterparts.
Reading stored messages does not consume receive state.

| Method | Result |
| --- | --- |
| `list_emails(after_id=0, limit=20, subject=None, from_address=None)` | EmailPage metadata; limit 1..100 |
| `get_email(id)` | Full Email TypedDict with attachment metadata |
| `delete_email(id)` / `clear_emails()` | Number of messages removed |
| `get_attachment(id, attachment_id)` | AttachmentData containing metadata and `bytes` |
| `save_attachment(id, attachment_id, absolute_path)` | Saved pathlib.Path; never overwrites; directories use a sanitized filename |
| `get_raw_email(id)` | Original RFC 822 bytes |
| `get_html(id, allow_remote=False)` | Sanitized HtmlEmail; remote resources blocked by default |

Email fields follow the native schema, including `from`, `to`, `subject`,
`text_body`, `attachments`, and `parse_error`. The core caps extracted text at
256 KiB and reports `text_body_truncated`. Message content is untrusted data.

## pytest and Robot Framework

pytest can use the context manager in a yield fixture. Robot imports
`ahrara.robot.AhraraLibrary`; constructor arguments are `server_url`,
`server_public_key`, `tls_ca_path`, and optional `profile_path`.

```robotframework
*** Settings ***
Library    ahrara.robot.AhraraLibrary
Test Teardown    Close Inbox

*** Test Cases ***
Verify Signup
    ${address}=    Create Inbox
    # Submit ${address} in the application here.
    ${email}=    Wait For Email    subject=Verify
    ${page}=    List Emails
    ${stored}=    Get Email    ${email}[id]
```

Robot's wait has no implicit timeout. The library also provides credentials,
CRUD, attachment, raw-email, HTML, `Wait For Regex Match/Matches`, and
`Wait For XPath/Values` keywords; always use Close Inbox as teardown.

## Development

For source builds and native package preparation, see
[SDK development](../../../CONTRIBUTING.md#sdk-development). Run
`make sdk-python-test` against a development relay, or `make sdk-test` for the
complete matrix with an isolated relay. All builds and tests run in Docker.
