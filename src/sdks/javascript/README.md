# Ahrara for JavaScript and TypeScript

Create an email address from Node.js, wait for messages, and extract verification
codes or links in JavaScript or TypeScript. Requires Node.js 20+; install with
`npm install @ahrara/sdk`. TypeScript declarations are included. The SDK connects
directly to the relay and runs in the application or test runner, not in the
browser.

```js
import { Ahrara } from '@ahrara/sdk';

const inbox = await Ahrara.createInbox();
try {
  // Submit inbox.address in your application.
  const email = await inbox.waitForEmail(); // No implicit timeout.
  // Assert on email.subject / email.text_body.
} finally {
  await inbox.close();
}
```

Creation returns after authentication and subscription. Mail arriving before a
wait is buffered. Each matching receive is consumed once, while storage retains
the message until deleted. Create separate inboxes for parallel tests. Concurrent
waits on one inbox are queued and run one at a time, in call order.

## Waits, streams, and callbacks

`waitForEmail({ timeoutMs, subject, from, signal })` waits indefinitely when
`timeoutMs` is omitted. An explicit positive integer deadline is in milliseconds
and throws `AhraraError` with code `timeout`. `subject` and `from` are ASCII
case-insensitive substrings. AbortSignal cancellation and `close()` interrupt
waits; cancelling a wait preserves the inbox. A simultaneously matched email may
win the cancellation race. Native work runs off the Node event loop.

```js
for await (const email of inbox.waitForEmails({ timeoutMs: 10_000 })) {
  await verify(email);
  if (finished()) break;
}

await inbox.receiveEmails(async email => {
  await verify(email);
}, { timeoutMs: 10_000 });
```

A stream's finite timeout is its **total duration**, including consumer/callback
work, and expiry ends normally. Omit timeout to keep receiving indefinitely.
Callbacks are awaited sequentially; errors propagate. Breaking a stream,
returning its iterator, cancellation, or callback failure leaves the inbox open.
There is no prefetch or hidden message queue. Use an AbortSignal to interrupt an
active `next()`; awaiting `return()` alone cannot interrupt an in-flight receive.
The application remains responsible for closing the inbox.

## Filters, regex captures, and XPath

Pass `filter` to `createInbox()` to discard nonmatching incoming email before
storage, or to a wait/search method to leave nonmatching email available for
later waits. All filter fields are ANDed. `sender`, `subject`, `to`, `cc`, `text`,
and `headers: { name: value }` use case-insensitive substring matching.
`recipient`, `envelopeSender`, and `messageId` match exactly, ignoring case;
`hasAttachments` selects presence or absence of attachments. Header names ignore
case. A per-wait filter adds to the inbox's receipt filter.

Set `filter: { regex: true, subject: '^(Verification|Confirm email)$' }` to
interpret all string conditions as case-insensitive Rust regex patterns; regex
queries below are case-sensitive unless `ignoreCase` is set. The top-level
`subject`/`from` wait options stay literal. See the
[shared filter rules](https://github.com/Hitmasu/Ahrara/blob/main/docs/sdks.md#filters-and-extraction)
for anchoring and limits.

```js
const match = await inbox.waitForRegexMatch('code=(?<code>[0-9]+)', {
  filter: { sender: 'example.test', subject: 'Verify' },
  ignoreCase: true,
});
const code = match.groups.find(group => group.name === 'code').value;

for await (const match of inbox.waitForRegexMatches('[0-9]+', { timeoutMs: 10_000 })) {
  await verifyCode(match.value);
}

const link = await inbox.waitForXPath('//a[@id="verify"]/@href');
for await (const value of inbox.waitForXPathValues('//a/@href', { timeoutMs: 10_000 })) {
  await verifyLink(value);
}
```

Regex uses the shared [Rust regex syntax](https://docs.rs/regex/latest/regex/#syntax),
including named groups; lookaround and backreferences are unsupported. Options
are `source: 'text' | 'html' | 'subject' | 'raw'` (default full decoded text),
`ignoreCase`, `multiLine`, and `dotMatchesNewLine`. A result contains `email_id`,
`value`, and indexed `groups`; group zero is the whole match and unmatched optional
groups have a null value. HTML searches use original decoded HTML.

Singular calls return the first value/match and throw on timeout. Plural methods
yield **every match/value across multiple emails**, with one total timeout that
includes consumer work. Finite expiry ends normally; omitted timeout is indefinite.
`signal` cancels either form. Invalid expressions and filters throw before
consuming email. Each matched email is consumed once: stopping partway through
its results does not replay the remaining values, but its stored message remains
readable.

## Persistence and identity

`createInbox({ profilePath: '/absolute/profile/directory' })` retains identity,
address, encrypted history, and consumed-receive state across close/reopen. Omit
`profilePath` for a disposable temporary profile. The same active profile cannot
be opened twice. Explicitly clear/delete messages to remove retained history.

```js
const credentials = await inbox.exportCredentials();
await inbox.close();
const restored = await Ahrara.createInbox({ credentials });
```

Credentials restore the identity/address, with fresh local history unless a
persistent profile is supplied. Inspection and JSON serialization redact the
Recovery Key; access `credentials.recoveryKey` only for deliberate secure storage.
Never log it. Alternatively pass `recoveryKey` and optional owned `address`;
these cannot be combined with `credentials`. An `address` alone requires an
existing profile identity. Unowned addresses and mismatched profile keys fail.

Connection options are `serverUrl`, `serverPublicKey`, `tlsCaPath`, and
`connectTimeoutMs` (1..300000 ms, default 20000). Omit them for the hosted relay,
whose verified public key the package embeds; a private relay's public key must be
independently verified. Custom CAs keep TLS verification enabled.

## Stored email and attachments

All operations are scoped to the inbox's address. Listing/getting does not
consume receive state. Native errors retain their code in `AhraraError.code`.

| Method | Result |
| --- | --- |
| `listEmails({ afterId, limit, subject, from })` | `{ emails, next_cursor }` metadata page; limit 1..100 |
| `getEmail(id)` | Full parsed email with attachment metadata |
| `deleteEmail(id)` / `clearEmails()` | Number of messages removed |
| `getAttachment(id, attachmentId)` | `{ attachment, content: Buffer }` |
| `saveAttachment(id, attachmentId, absolutePath)` | Saved file path; never overwrites; directories use a sanitized filename |
| `getRawEmail(id)` | Original RFC 822 bytes as Buffer |
| `getHtml(id, { allowRemote: false })` | Sanitized `{ html, remote_resources }`; remote resources blocked by default |

Email fields use the native snake_case schema, including `from`, `to`, `subject`,
`text_body`, `attachments`, and `parse_error`. Extracted text is capped at 256 KiB
and reports `text_body_truncated`. HTML and attachments are fetched explicitly.
Treat message content as untrusted data.

## Test frameworks

Playwright uses a test fixture owning an inbox; see
[`signup.spec.js`](https://github.com/Hitmasu/Ahrara/blob/main/src/sdks/javascript/test/signup.spec.js).
Cypress uses Node `setupNodeEvents` and `cy.task`, since browser code cannot load
the native library; see
[`cypress.config.js`](https://github.com/Hitmasu/Ahrara/blob/main/src/sdks/javascript/cypress.config.js).
Node/Jest/Vitest use the same API.
