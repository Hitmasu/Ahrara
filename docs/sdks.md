# SDKs

Ahrara SDKs give your application or test its own email inbox. Create an address,
use it in a signup or password-reset flow, then wait for the message, extract a
verification code or link, or inspect an attachment. Ahrara receives email; it
does not send it.

Each inbox connects directly to the relay. You do not need to run the Ahrara CLI,
Web interface, or local API alongside your application.

## Choose your SDK

Each guide contains examples and the options for that language.

| SDK | Requirements | Receiving messages |
| --- | --- | --- |
| [.NET](../src/sdks/dotnet/README.md) | .NET 8+ | Async calls, async streams, or callbacks; returns `MailMessage`. |
| [JavaScript / TypeScript](../src/sdks/javascript/README.md) | Node.js 20+ | Promises, async iterators, or async callbacks. |
| [Python](../src/sdks/python/README.md) | Python 3.11+ | Synchronous and asynchronous APIs, plus Robot Framework keywords. |
| [Go](../src/sdks/go/README.md) | Go 1.23+ with cgo | Calls, iterators, or callbacks, with context cancellation. |
| [Java](../src/sdks/java/README.md) | Java 17+ | Blocking calls, closeable streams, or callbacks. |
| [Rust](../src/core/README.md) | Tokio runtime | Async calls, streams, or async callbacks. |
| [k6](../src/sdks/k6/README.md) | Custom k6 binary with the Ahrara extension | Calls and synchronous callbacks within each virtual user. |

Except for Rust, the SDKs run the shared core as a native library built for
Linux, macOS and Windows on x64 and ARM64. On Linux it needs glibc 2.17 or later,
so Alpine and other musl-based systems are not supported. The .NET, JavaScript,
Python and Java packages include it. The Go SDK, and with it the k6 extension,
downloads it from the GitHub Release on first use, as the
[Go guide](../src/sdks/go/README.md#native-library) explains. The full test suite runs on
Linux x64. The JavaScript SDK runs in Node.js, not browser code; stock k6 cannot
load the Ahrara extension.

## How an inbox works

1. **Create an inbox.** Ahrara creates an identity and address, connects, and
   returns once reception is ready. A new inbox is isolated by default.
2. **Use its address.** Submit it to the application that should send the email.
3. **Wait for the result.** Receive the full message or wait directly for a code
   or link. Messages arriving before the wait remain available.
4. **Inspect or keep the message.** Receiving marks it as handled; it remains
   readable until deleted or its temporary profile is removed.
5. **Close the inbox.** Use your language's disposal or cleanup mechanism when
   the application or test is finished. SDKs with garbage collection also close
   an unreachable inbox, but only as a fallback.

Keep the inbox connected while expecting mail. The relay has no offline queue;
reopening an address does not recover messages sent while it was disconnected.
Use a separate inbox for each parallel test or k6 virtual user.

## Receive your first email

This Node.js example uses the JavaScript SDK. Submit the printed address through
your application before the wait expires; other languages follow the same flow.

```js
import { Ahrara } from '@ahrara/sdk';

const inbox = await Ahrara.createInbox();
try {
  console.log(inbox.address);
  const email = await inbox.waitForEmail({
    timeoutMs: 30_000,
    filter: { subject: 'Verification' },
  });
  // Use email.subject, email.text_body, and email.attachments in your checks.
} finally {
  await inbox.close();
}
```

**There is no receive timeout unless you set one.** Set a deadline in tests so a
missing email does not leave the test waiting indefinitely. Connection setup has
its own timeout, which defaults to 20 seconds.

## Receive one message or keep listening

Use a single-message wait for a verification email. Use an iterator or stream
for several messages, or a callback to handle them one at a time. The language
guides show the corresponding methods and cancellation options.

A stream's duration covers the entire operation, including message processing.
Callbacks run sequentially; async callbacks are awaited. Finite streams finish
normally, except Go, which reports its context deadline. Single waits report a
timeout when no message arrives; Rust returns `None`.

Cancelling a wait or leaving a stream keeps the inbox connected. Close the inbox
to stop reception. Callback errors propagate; callbacks must cooperate with
cancellation during their own work.

Concurrent waits, streams, and extraction calls on one inbox are queued and run
one at a time; a queued call still honours its own timeout and cancellation. Use
separate inboxes for work that must receive in parallel.

Receive and extraction calls share handled-message state. A message selected by
one call is not delivered again by another, even if a callback fails. Use message
IDs and the stored history when you need to retry processing.

## Filters and extraction

Filter by sender, subject, To/Cc, body text, headers, SMTP envelope fields,
Message-ID, or attachment presence. All supplied conditions must match. Field
names follow each language's conventions, such as `envelopeSender`,
`EnvelopeSender`, or `envelope_sender`; Go names `sender` `From`.

| Field | Compares | Literal value | With `regex: true` |
| --- | --- | --- | --- |
| `sender` | Decoded From header, including display names | Case-insensitive substring | Matches anywhere |
| `subject` | Decoded Subject header | Case-insensitive substring | Matches anywhere |
| `to`, `cc` | Decoded To or Cc headers | Case-insensitive substring | Matches anywhere |
| `text` | Full decoded plain-text body, excluding attachments | Case-insensitive substring | Matches anywhere |
| `headers` | Values of each named header; any repeated value can match | Case-insensitive substring | Matches anywhere |
| `recipient` | SMTP envelope recipient | Whole value, ignoring case | Must match the whole value |
| `envelope_sender` | SMTP envelope sender | Whole value, ignoring case | Must match the whole value |
| `message_id` | Message-ID header | Whole value, ignoring case | Must match the whole value |
| `has_attachments` | Presence or absence of attachments | Boolean | Boolean |

Header names are always literal and case-insensitive. The separate `subject` and
`from` wait options, where an SDK offers them, remain literal substrings.

Set `regex: true` on the filter to interpret every string condition, including
header values, as a Rust regular expression. Patterns are strings rather than
language-native regex objects. Header names remain literal and case-insensitive;
the attachment flag remains boolean. All conditions still combine with AND.

```js
const email = await inbox.waitForEmail({
  timeoutMs: 30_000,
  filter: {
    regex: true,
    subject: '^(Verification|Confirm email)( [0-9]+)?$',
    hasAttachments: false,
  },
});
```

Recipient, envelope sender, and Message-ID patterns are anchored to the whole
value, so `sender@example` does not match `sender@example.test`. Other patterns
search anywhere in the field; use `^` and `$` for a complete match. Filter
matching ignores case by default; `(?-i)` overrides this for a pattern. Regex
extraction queries are the opposite: case-sensitive unless their ignore-case
option is set. Groups, alternation, character
classes, repetition and inline flags such as `(?m)` and `(?s)` are supported;
lookaround and backreferences are not. Escape metacharacters in literal values:
`example\.com` matches a literal dot. This is regex syntax, not file globbing:
`*` repeats the preceding expression; `.*` matches arbitrary text.

Patterns are validated before opening a profile or starting a receive. Invalid
or oversized expressions return `invalid_argument` without consuming mail.
Filter values require 1–4096 UTF-8 bytes without NUL; at most 64 header conditions
are allowed. Each SDK request, including all of its filter values, is limited to
1 MiB of JSON. Omitting `regex` or setting it to `false` preserves literal
matching.

Where you apply a filter changes what happens to other mail:

| Filter location | Nonmatching messages |
| --- | --- |
| Inbox creation | Discarded before local storage. |
| A wait or extraction call | Kept for another call with different criteria. |

For verification codes, wait for a regex match instead of receiving the whole
message first. This JavaScript example returns a six-digit code and reads the
same email from history:

```js
const match = await inbox.waitForRegexMatch('[0-9]{6}', {
  timeoutMs: 30_000,
  filter: { subject: 'Verification' },
});
const code = match.value;
const email = await inbox.getEmail(match.email_id);
```

For a confirmation link, use XPath instead:

```js
const link = await inbox.waitForXPath('//a[@id="verify"]/@href', {
  timeoutMs: 30_000,
});
```

Regex can search full decoded text, HTML, subject, or raw email and return named
and numbered capture groups. All SDKs use the same syntax; lookaround and
backreferences are unsupported. XPath extracts strings from HTML without running
scripts or loading external resources.

Plural extraction methods return occurrences across matching emails; k6 uses
callbacks for this. Stopping partway through an email's results does not replay
its remaining occurrences, but the message stays readable. Invalid expressions
and query-limit errors leave the message available.

## Persistent profiles and identity reuse

| Option | What survives closing the inbox |
| --- | --- |
| Default temporary inbox | Nothing on disk after cleanup. Use it for isolated tests. |
| Persistent profile | Identity, address, encrypted messages, and handled-message state. |
| Exported credentials | Identity and address only; message history is not included. |

To keep history, set the profile path to an absolute directory and reuse that
path next time. Close the previous inbox before reopening it. Keep profiles
private and outside version control; one identity can have only one active
receiver. On Linux and macOS, a reused profile directory must belong to you and
must not be writable by other users.

Export credentials when you need to restore the address in another environment.
They contain a secret Recovery Key: store them securely and never log them.
Importing a Recovery Key alone restores the identity and may create a new alias;
exported credentials include the exact address to restore.

## Stored email and attachments

All SDKs can list messages, read by ID, retrieve original MIME bytes or sanitized
HTML, delete a message, and clear the inbox. Listing and reading do not mark mail
as handled. Use the page's next cursor to continue listing; IDs belong to the
local profile. Operations affect only the current inbox's address.

.NET returns `System.Net.Mail.MailMessage`, including bodies and attachments;
dispose each returned message and copy attachment streams to your chosen file.
Other SDKs expose attachment metadata and methods to retrieve bytes or save to a
file. Their save helpers refuse to overwrite existing files.

Messages can be up to 10 MiB. Outside .NET's `MailMessage`, text previews are
limited to 256 KiB and report when truncated; retrieve the original MIME for the
complete message. Regex and XPath searches use the full decoded bodies.
Sanitized HTML blocks remote resources by default. Treat email content and
attachments as untrusted input.

## Use with test frameworks

Create the inbox during test setup and close it in teardown, even when an
assertion fails. The same APIs work with xUnit, NUnit, MSTest, pytest, and JUnit.
For browser tests, keep the SDK in the test runner and pass its address or
extracted code to the page:

- **Playwright:** own the inbox in a Node test fixture.
- **Cypress:** call the Node SDK through `setupNodeEvents` and `cy.task`.
- **Selenium:** use the Java SDK with try-with-resources.
- **Robot Framework:** load `ahrara.robot.AhraraLibrary` and use `Close Inbox`
  as teardown.
- **k6:** create and close an inbox within each virtual user; callbacks are
  synchronous.

See the [language guides](#choose-your-sdk) for examples.

## Connection settings

The default relay is `wss://relay.ahrara.dev`. Release builds include its verified
public transport key. For a custom relay or a source build without that key,
provide the relay URL and verified public key in the SDK's creation options.
A private certificate authority also requires its trusted CA file. The SDK does
not read the CLI's configuration.

For builds, package preparation, and the Docker test matrix, see
[SDK development](../CONTRIBUTING.md#sdk-development).
