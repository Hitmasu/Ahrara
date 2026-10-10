# Ahrara for Go

Create an email address from a Go application or test, receive messages, and
extract verification codes or links. Go 1.23+ and cgo are required; install with
`go get github.com/Hitmasu/Ahrara/src/sdks/go@latest` and import the package as
`ahrara`. The SDK connects directly to the relay without a running Ahrara CLI.

```go
inbox, err := ahrara.CreateInbox(ctx, ahrara.Options{})
if err != nil { return err }
defer inbox.Close()

// Trigger the application under test with inbox.Address.
email, err := inbox.WaitForEmail(ctx, ahrara.Filter{Subject: "Verify"})
```

There is no receive timeout. A background context waits until mail arrives,
the inbox closes, or an error occurs. A caller-owned context can cancel the
operation or apply an explicit deadline. Canceling a wait leaves its inbox open.
The core buffers messages arriving before the first wait. Matches are consumed
once; unmatched messages remain available. `Filter.Subject` and `Filter.From`
match Unicode case-insensitive header substrings. Close the inbox with `defer`
or `t.Cleanup`.

## Filtering and extracting values

`Options.Filter` is the inbox admission filter: rejected messages are discarded
before local storage. A receive's `Filter` runs after admission and leaves its
nonmatching messages available for another call. Every populated condition must
match. Filters support `From`, `Subject`, `To`, `CC`, `Text`, `Headers`,
`Recipient`, `EnvelopeSender`, `MessageID` and `HasAttachments`. Header/text
conditions match substrings; envelope fields and Message-ID match the complete
value. Matching ignores case. `Text` searches the full decoded body.

Set `ahrara.Filter{Regex: true, Subject: "^(Verification|Confirm email)$"}` to
interpret all string conditions as case-insensitive Rust regex patterns;
`RegexOptions` queries are case-sensitive unless `IgnoreCase` is set. Invalid
patterns fail before consuming mail. See the
[shared filter rules](https://github.com/Hitmasu/Ahrara/blob/main/docs/sdks.md#filters-and-extraction)
for anchoring and limits.

```go
match, err := inbox.WaitForRegexMatch(ctx,
    `(?P<code>[0-9]{6}) is your verification code`,
    ahrara.RegexOptions{Filter: ahrara.Filter{Subject: "Verify"}},
)
if err != nil { return err }
for _, group := range match.Groups {
    if group.Name != nil && *group.Name == "code" && group.Value != nil {
        code := *group.Value // "123456"
        _ = code
    }
}
```

Regex syntax is shared across SDKs: named/numbered captures and Unicode are
supported; backreferences and look-around are not. Group zero is the entire
match; optional unmatched groups have a nil `Value`. `RegexOptions.Source`
selects `RegexText` (default), `RegexHTML`, `RegexSubject` or `RegexRaw`.
`IgnoreCase`, `MultiLine` and `DotMatchesNewLine` control matching. Body searches
use complete decoded content and exclude attachment bodies. Raw matching replaces
invalid UTF-8 with U+FFFD; `RawEmail` preserves the exact original bytes.

`WaitForXPath(ctx, "//a/@href", filter)` returns the first value from HTML.
Node values are returned in document order; strings/numbers/true use XPath string
conversion. Empty strings, empty node sets and false leave the message available.

Plural methods are lazy iterators across **all matching emails** during one
context deadline, including time spent in the consumer:

```go
period, cancel := context.WithTimeout(ctx, 10*time.Second)
defer cancel()
for match, err := range inbox.WaitForRegexMatches(period, `[0-9]{6}`, ahrara.RegexOptions{}) {
    if err != nil { return err } // Includes context.DeadlineExceeded at period end.
    use(match.Value)
}
// WaitForXPathValues has the same lifetime and yields string values.
```

Each selected email is consumed once; breaking iteration discards any remaining
results from that email and leaves subsequent emails available. Queries never
start background work or prefetch another email.

## Sequential message processing

`Emails` is an `iter.Seq2[*Email, error]`. It has no prefetch buffer or background
goroutine. Breaking iteration leaves the next message available and does not
close the inbox:

```go
for email, err := range inbox.Emails(ctx, ahrara.Filter{}) {
    if err != nil { return err }
    if err := process(email); err != nil { return err }
    if finished { break }
}
```

`ForEachEmail` calls its callback sequentially and returns the first receive,
context, or callback error unchanged. A deadline applies to the complete loop:

```go
processing, cancel := context.WithTimeout(ctx, time.Minute) // Optional total limit.
defer cancel()
err := inbox.ForEachEmail(processing, ahrara.Filter{}, process)
```

Callbacks must return cooperatively; capture the same context if callback work
needs cancellation. Methods are safe for concurrent callers, while concurrent
receive operations queue and run one at a time on each inbox. Always `Close` the
inbox when done.

## Persistence and recovery

Pass an absolute `Options.ProfilePath` to keep the identity, inbox address,
encrypted history and consumed-message state across close and reopen. Without
it, the core owns an encrypted temporary profile and removes it after close.

`inbox.ExportCredentials()` returns `*Credentials` with `Address` and
`RecoveryKey`. Reopen with `Options{Credentials: credentials}` or supply
`RecoveryKey` and optional `Address` directly. These two import forms are mutually
exclusive. Credentials restore the identity/address, with fresh local history;
they do not copy an existing profile or create an offline queue at the relay.

Recovery keys are secrets. `fmt` string and Go-syntax formatting of `Credentials`
and `Options` are redacted. Explicit JSON serialization includes the key for
backup/import and must be stored securely, never logged or committed.

## Stored messages and attachments

| Method | Result |
| --- | --- |
| `ListEmails(ListOptions)` | `EmailPage` with metadata and optional `NextCursor`; default limit 20, maximum 100. |
| `GetEmail(id)` | Full decoded `Email`, including attachment metadata. |
| `DeleteEmail(id)` | Number of messages deleted from this inbox. |
| `ClearEmails()` | Number of inbox messages removed. |
| `GetAttachment(id, attachmentID)` | Metadata and exact decoded bytes in `AttachmentData.Content`. |
| `SaveAttachment(id, attachmentID, destination)` | Saved absolute path; existing files are never overwritten. |
| `RawEmail(id)` | Original RFC 5322 message bytes. |
| `GetHTML(id, allowRemote)` | Sanitized HTML and remote-resource count; pass false to block remote resources. |

List and get include previously consumed messages without changing receive
state. Pagination passes a page's `NextCursor` as the next `ListOptions.AfterID`.
`ListOptions.Subject` and `ListOptions.From` are ASCII case-insensitive storage
substrings (`From` also matches the envelope sender); unlike `Filter.From`, they
never use regex.
The attachment ID comes from `Email.Attachments`. Save accepts an absolute file
path or an existing directory, where the core chooses a sanitized filename.

`Email.TextBody` is capped at 256 KiB; `TextBodyTruncated` reports truncation.
Use `RawEmail` for original bytes and `GetHTML` for the HTML part. Native failures
are `*ahrara.Error` with a stable `Code`. Closed inbox operations return
`ahrara.ErrClosed`; waits and streams return standard context errors.

## Native library

The SDK runs Ahrara's Rust core as a native library, which Go modules cannot
carry. On first use it downloads the library for your platform from the
matching [GitHub Release](https://github.com/Hitmasu/Ahrara/releases), checks it
against the SHA-256 pinned in the module version and keeps it in the user cache
directory. Each later run checks the cached copy again before loading it. Linux,
macOS and Windows on x64 and ARM64 are supported, and Linux needs glibc 2.17 or later.

Offline or air-gapped, download your platform's file from that release yourself,
such as `ahrara-native-linux-x86_64.so`, and set `AHRARA_NATIVE_LIBRARY` to its
absolute path. That library is loaded without a checksum check, so protect the
variable as you would `LD_PRELOAD`. Builds need cgo and a C compiler. On Windows,
install a GCC such as MinGW-w64.

## Connection settings

For a private relay, use `ServerURL`, `ServerPublicKey`, `TLSCAPath`, and
`ConnectTimeoutMS`. See the [connection guide](https://github.com/Hitmasu/Ahrara/blob/main/docs/sdks.md#connection-settings).
