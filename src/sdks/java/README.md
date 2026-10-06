# Ahrara for Java

Create an email address from a Java application or test, wait for messages, and
extract verification codes or links. Requires Java 17+. Use a single blocking
call, a stream, or a callback to receive mail. The SDK connects directly to the
relay without a running Ahrara CLI.

## Receive one email

```java
try (var inbox = Ahrara.createInbox()) {
    // Submit inbox.address() to your application.
    var email = inbox.waitForEmail();
}
```

The default wait has **no time limit**. Reception is ready when creation returns,
and mail received before waiting remains available. A finite wait is explicit:

```java
var email = inbox.waitForEmail(Duration.ofSeconds(30),
    EmailFilter.builder().subject("Verification").build());
```

A finite wait throws `TimeoutException` on expiry. Thread interruption throws
`InterruptedException`, leaving the inbox connected. Native failures propagate as
`AhraraException` with `code()`. Close the inbox with try-with-resources to stop
reception; closing is idempotent and interrupts active waits.

For a private relay, use `new InboxOptions(serverUrl, verifiedPublicKey, caPath,
Duration.ofSeconds(10))`. A native build without an embedded public relay pin also
requires a supplied public key. The CA path is optional when the relay uses a
publicly trusted certificate.

## Lazy streams and callbacks

```java
try (var emails = inbox.emails(Duration.ofMinutes(2))) {
    emails.limit(3).forEach(email -> processEmail(email));
}

inbox.receiveEmails(email -> processEmail(email)); // Unlimited until interrupted/error.
inbox.receiveEmails(email -> processEmail(email), Duration.ofMinutes(2), null);
```

Streams are sequential, lazy, and do not prefetch or create background threads.
The optional duration is a total budget from first traversal, including callback
processing time. Expiry ends normally. A thread interrupt during stream traversal
or callback reception restores the interrupt flag and throws
`CancellationException`. Callback runtime errors propagate; callbacks run one at
a time on the calling thread.

Closing or short-circuiting a stream leaves the inbox connected. Messages already
delivered remain in history but are not received again. Unmatched mail is retained.
Use try-with-resources around streams, following the
[Java Stream lifecycle](https://docs.oracle.com/en/java/javase/24/docs/api/java.base/java/util/stream/Stream.html)
and [AutoCloseable contract](https://docs.oracle.com/en/java/javase/24/docs/api/java.base/java/lang/AutoCloseable.html).

## Filters, regex, and XPath

```java
var filter = EmailFilter.builder()
    .sender("accounts@example.test")
    .subject("Verification")
    .hasAttachments(false)
    .build();

var email = inbox.waitForEmail(Duration.ofSeconds(30), filter);
var code = inbox.waitForRegexMatch("Code: (?P<code>\\d{6})", Duration.ofSeconds(30), filter);

try (var matches = inbox.waitForRegexMatches("\\d{6}", Duration.ofMinutes(1), filter)) {
    matches.forEach(match -> System.out.println(match.value()));
}
var link = inbox.waitForXPath("//a[@class='verify']/@href", Duration.ofSeconds(30), filter);
try (var values = inbox.waitForXPathValues("//a/@href", Duration.ofMinutes(1), filter)) {
    values.forEach(System.out::println);
}
```

Single queries return the first result and consume its email. Plural queries are
lazy streams continuing across **all matching emails during the total duration**,
including every occurrence in each email. Consumer processing counts toward the
deadline. No match leaves the message available. A matching email is consumed
once; ending a stream early does not replay its remaining occurrences. Closing a
stream leaves the inbox open. Single finite waits throw `TimeoutException`;
plural expiry ends normally. Interruption throws `InterruptedException` for
single waits or `CancellationException` for streams, preserving the stream
thread's interrupt flag.

For explicit regex options use `new RegexQuery(pattern, source, ignoreCase,
multiLine, dotMatchesNewLine)`. Sources are `text` (default), `html`, `subject`, and
`raw`. Results expose the local `emailId()`, complete `value()`, and numbered or
named `groups()`. Group zero contains the whole match; unmatched optional groups
have null values. The shared Rust regex syntax excludes look-around and
backreferences. XPath uses the same HTML parser and query engine as other SDKs,
including recovery of ordinary malformed HTML.

Filter conditions combine with AND. Sender, subject, To/Cc, text, and header
values are case-insensitive substrings. Recipient, envelope sender, and Message-ID
match the whole value, ignoring case. Header names are case-insensitive and any
repeated value can match. Operation filters retain nonmatching mail; use
`options.withFilter(filter)` to discard rejected incoming messages **before local
storage**. Global and operation conditions combine. Invalid queries or filters
raise `AhraraException` without consuming messages. Queries and ordinary email
waits share the same receiver lock, so concurrent waits are queued and run one at
a time.

Use `EmailFilter.builder().regex(true).subject("^(Verification|Confirm email)$").build()`
to interpret all string conditions as case-insensitive Rust regex patterns.
Recipient, envelope sender, and Message-ID patterns must match the whole value;
other patterns match anywhere, so use `^` / `$` for a full match. Use `(?-i)` for
case sensitivity; `RegexQuery` is case-sensitive unless `ignoreCase` is true.
Header names stay literal. Invalid patterns fail before consuming mail; omitting
`.regex(true)` keeps literal matching. See the [shared filter rules](../../../docs/sdks.md#filters-and-extraction).

## Persistent profiles and identity import

```java
var options = InboxOptions.defaults()
    .withProfilePath(Path.of("test-state/inbox").toAbsolutePath().toString());

try (var inbox = Ahrara.createInbox(options)) {
    // The address, identity, encrypted messages, and receiving state remain in the profile.
}
try (var reopened = Ahrara.createInbox(options)) {
    var history = reopened.listEmails();
}
```

Without a profile path, storage is temporary and removed on close. Protect durable
profiles and exclude them from version control. Close the previous connection
before reopening the same identity.

`inbox.exportCredentials()` returns its address and secret recovery key. Later,
`Ahrara.createInbox(options.withCredentials(credentials))` restores that identity
and address, but credentials alone do not transfer message history. To import an
identity directly, use `options.withIdentity(recoveryKey, ownedAddress)`; the
address is optional. The core validates address ownership.

Credentials and options redact `toString()`. Explicit field access and serialization
still expose the recovery key, so never log or commit it.

## Stored messages and attachments

```java
var page = inbox.listEmails(0, 20, "Verification", null);
for (var summary : page.emails()) {
    var email = inbox.getEmail(summary.id());
    for (var attachment : email.attachments()) {
        byte[] content = inbox.getAttachment(email.id(), attachment.attachmentId()).content();
        Path saved = inbox.saveAttachment(email.id(), attachment.attachmentId(),
            Path.of("downloads").toAbsolutePath());
    }
}
// Use page.nextCursor() as afterId when it is not null.
inbox.deleteEmail(emailId);
inbox.clearEmails();
```

Listing/reading do not consume mail. `getRawEmail(id)` returns original message
bytes. `getHtml(id, false)` returns sanitized HTML blocking remote resources.
`textBodyTruncated()` reports a limited body response. Attachment saving uses safe
filename rules and refuses overwrites. Deletion is scoped to this inbox's local
storage; missing resources and other native errors propagate through
`AhraraException`.

## Development

For source builds and native package preparation, see
[SDK development](../../../CONTRIBUTING.md#sdk-development). Run
`make sdk-java-test` against a development relay, or `make sdk-test` for the
complete matrix with an isolated relay. All builds and tests run in Docker.
