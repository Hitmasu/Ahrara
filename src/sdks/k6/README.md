# Ahrara extension for k6

Use `k6/x/ahrara` to include email delivery in a load test. Each virtual user can
create an address, trigger an email through your application, and wait for its
message, verification code, or link.

This requires a custom k6 binary built with the Ahrara extension. A stock k6
binary cannot load it. Build it with [xk6](https://github.com/grafana/xk6) and
cgo, which the extension needs:

```sh
go install go.k6.io/xk6/cmd/xk6@latest
CGO_ENABLED=1 xk6 build --with github.com/Hitmasu/Ahrara/src/sdks/k6@vX.Y.Z
```

Replace `vX.Y.Z` with an Ahrara release version.

The extension uses the [Go SDK](https://github.com/Hitmasu/Ahrara/blob/main/src/sdks/go/README.md#native-library),
which downloads Ahrara's native library on first use. The inbox connects directly
to the relay, and waiting for mail blocks only the current virtual user.

```javascript
import ahrara from 'k6/x/ahrara';

export default function () {
  const inbox = ahrara.createInbox({});
  try {
    // Trigger the application under test with inbox.address.
    const email = inbox.waitForEmail({ filter: { subject: 'Verify' } });
    // Check email.textBody.
  } finally {
    inbox.close();
  }
}
```

Create inboxes during VU execution. A receive has no timeout unless you pass
`durationMs`, for example `inbox.waitForEmail({ filter: { subject: 'Verify' }, durationMs: 30000 })`;
expiry throws. Otherwise it waits for mail, VU cancellation, close, or an error.
The receiver stores mail that arrives before a wait, and each matched message
is returned once. Subject and sender filters match substrings. Always close in
`finally`; VU cancellation also requests native cleanup.

## Multiple emails and callbacks

`forEachEmail` processes messages synchronously and sequentially without a
prefetch buffer. Return false to stop, or throw to propagate an error. Breaking
the callback loop does not consume the next message or close the inbox:

```javascript
let received = 0;
const processed = inbox.forEachEmail(email => {
  // Check this email before receiving another one.
  received++;
  return received < 3;
}, { filter: { subject: 'Notification' } });
```

An optional `{ durationMs: 60000 }` limits the **whole loop**, including time
between callbacks. It does not restart for each message. Expiration returns the
number processed; VU cancellation, native errors and thrown callback errors
propagate. Callbacks must be synchronous and return cooperatively; async/Promise
callbacks are rejected. This works in k6's JavaScript runtime without Node APIs.

## Filters, Regex and XPath

`createInbox({ filter })` discards rejected messages before storing them. Each
receive/query accepts its own `{ filter }`, applied after admission; these
nonmatches remain available. All populated fields must match. Supported fields
are `sender`, `subject`, `to`, `cc`, `text`, `headers`, `recipient`,
`envelopeSender`, `messageId` and `hasAttachments`. Header/body checks are
case-insensitive substrings; envelope fields and Message-ID match the whole value,
ignoring case. `headers` maps a header name to a substring, including repeated
headers.

Use `filter: { regex: true, subject: '^(Verification|Confirm email)$' }` to
interpret all string conditions as case-insensitive Rust regex patterns; regex
queries are case-sensitive unless `ignoreCase` is set. Invalid patterns fail
before consuming mail. See the
[shared filter rules](https://github.com/Hitmasu/Ahrara/blob/main/docs/sdks.md#filters-and-extraction)
for anchoring and limits.

```javascript
const match = inbox.waitForRegexMatch(
  '(?P<code>[0-9]{6}) is your verification code',
  { filter: { subject: 'Verify' } },
);
const code = match.groups.find(group => group.name === 'code').value;
const link = inbox.waitForXPath('//a/@href', {});
```

Regex options include `source` (`text`, `html`, `subject`, `raw`), `ignoreCase`,
`multiLine` and `dotMatchesNewLine`. They use shared Rust regex syntax, with
named captures but no backreferences/look-around. Results contain `emailId`,
`value` and `groups` (`index`, nullable `name` and nullable `value`); group zero is
the entire match. Text/HTML are decoded in full and exclude attachments.

XPath node values follow document order; scalar values use XPath string
conversion. Empty values and false do not match.

The k6 equivalents of the other SDKs' plural iterators are sequential callbacks:

```javascript
inbox.forEachRegexMatch('[0-9]{6}', match => {
  // Check each occurrence across all emails received during this period.
}, { durationMs: 10000 });
inbox.forEachXPathValue('//a/@href', link => {
  // Check each link in document order across all matching emails.
}, { durationMs: 10000, filter: { sender: 'sender@example.test' } });
```

`durationMs` is optional and covers the entire operation, including callbacks.
Single-result methods throw on expiration; callbacks finish normally with their
invocation count. Without it, the VU context controls lifetime. Returning false
stops immediately; throwing propagates. Selecting results consumes that email;
early exit discards remaining results from it and leaves the next email available.
No further email is prefetched.

## Persistence and recovery

`createInbox({ profilePath: '/absolute/profile' })` preserves the address,
identity, encrypted history and consumed-message state across reopen. Give each
concurrent VU its own profile. The default profile is temporary.

`inbox.exportCredentials()` returns `address` and secret `recoveryKey`.
Pass the value to `createInbox({ credentials })` to restore the same address with
fresh local history, or pass `{ recoveryKey, address }` directly. Do not combine
the two import forms. No offline relay queue is created by restoring credentials.

`String(credentials)` and `JSON.stringify(credentials)` redact the key. Reading
`credentials.recoveryKey` is an explicit secret export; store it securely and
never log it.

Private relay options are `serverUrl`, `serverPublicKey`, `tlsCaPath` and
`connectTimeoutMs`. Omit them for the hosted relay, whose verified public key the
release native library embeds.

## Mailbox operations

| Method | Result |
| --- | --- |
| `listEmails({ afterId, limit, subject, from })` | `{ emails, nextCursor }`, without consuming receive events; default limit 20, maximum 100. |
| `getEmail(id)` | Decoded message and attachment metadata, including already consumed messages. |
| `deleteEmail(id)` / `clearEmails()` | Count of messages deleted from this inbox. |
| `getAttachment(id, attachmentId)` | `{ attachment, content }`, with bytes in an `ArrayBuffer`. |
| `saveAttachment(id, attachmentId, destination)` | Saved path; destination is an absolute filename or directory, with no overwrite. |
| `getRawEmail(id)` | Original RFC 5322 bytes in an `ArrayBuffer`. |
| `getHtml(id, allowRemote)` | `{ html, remoteResources }`; false keeps remote resources blocked. |

Email fields use camel case, unlike the Node.js SDK's snake_case fields,
including `textBody`, `textBodyTruncated`, `htmlAvailable`, `receivedAt`,
`messageId`, `sizeBytes` and `attachments`.
Attachment fields are `id`, `filename`, `contentType`, `contentDisposition` and
`size`. `textBodyTruncated` reports the core's 256 KiB plain-text decoding limit.
