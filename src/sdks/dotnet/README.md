# Ahrara for .NET

Create an email address from a .NET application or test, wait for messages, and
extract verification codes or links. Requires .NET 8+. Received emails are
standard `System.Net.Mail.MailMessage` objects, with bodies and attachments.
The SDK connects directly to the relay; you do not need to run the Ahrara CLI.

## Receive one email

```csharp
using AhraraSdk;
using System.Net.Mail;

await using var inbox = await Ahrara.CreateInboxAsync();
// Submit inbox.Address to your application.
using MailMessage email = await inbox.WaitForEmailAsync(cancellationToken: cancellationToken);
Console.WriteLine(email.Subject);
Console.WriteLine(email.Body);
```

The default wait has **no time limit**. Creation returns only when reception is
ready, and messages received before the wait remain available. Set an explicit
timeout when appropriate:

```csharp
using var email = await inbox.WaitForEmailAsync(
    timeout: TimeSpan.FromSeconds(30), subject: "Verification",
    cancellationToken: cancellationToken);
```

A finite single wait throws `TimeoutException` when it expires. Cancellation
throws `OperationCanceledException` without disconnecting the inbox. Reception
errors propagate as `AhraraException`, with a machine-readable `Code`. Disposal
interrupts waits and releases resources.

The caller owns every returned `MailMessage`, including its attachments and
alternate views. Dispose each message after use. Its content is held in managed
memory and remains usable after closing the inbox.

For a private relay, pass `InboxOptions` with `ServerUrl`, `ServerPublicKey`, and
optional `TlsCaPath`. The public key must be independently verified. A native
build without an embedded public relay pin also needs `ServerPublicKey` supplied
explicitly. `ConnectTimeoutMilliseconds` bounds connection setup; creation
cancellation waits for that bounded attempt to finish and then cleans it up.

## Receive a stream or use a callback

```csharp
await foreach (var email in inbox.WaitForEmailsAsync(
    duration: TimeSpan.FromMinutes(2)).WithCancellation(cancellationToken))
{
    using (email) await ProcessEmailAsync(email);
    if (EnoughEmailsReceived()) break;
}

await inbox.ReceiveEmailsAsync(
    async (email, cancellation) =>
    {
        using (email) await ProcessEmailAsync(email, cancellation);
    },
    duration: TimeSpan.FromMinutes(2),
    cancellationToken: cancellationToken);
```

These APIs are lazy and process one email at a time. There is no background
prefetch queue. A duration is a **total receiving duration**, measured from first
enumeration and including consumer or callback processing time. Omitting it
receives indefinitely. Duration expiry ends normally; cancellation throws
`OperationCanceledException`. Callbacks are awaited sequentially and their errors
propagate. An in-progress callback is awaited before the receiver returns.

Breaking enumeration, cancelling, or a callback failure leaves the inbox usable.
Delivered messages remain in local history, but receiving operations do not
deliver them again. Unmatched messages remain eligible for later filters. The
async iterator supports the standard
[C# async stream cancellation model](https://learn.microsoft.com/en-us/dotnet/csharp/asynchronous-programming/generate-consume-asynchronous-stream).

## Filter and extract values

```csharp
var filter = new EmailFilter
{
    Sender = "accounts@example.test",
    Subject = "Verification",
    HasAttachments = false
};
using var message = await inbox.WaitForEmailAsync(filter: filter);

var code = await inbox.WaitForRegexMatchAsync(
    @"Code: (?P<code>\d{6})", timeout: TimeSpan.FromSeconds(30), filter: filter);
Console.WriteLine(code.Groups.Single(group => group.Name == "code").Value);

await foreach (var match in inbox.WaitForRegexMatchesAsync(
    new RegexQuery(@"code: (?P<code>\d+)") { IgnoreCase = true },
    duration: TimeSpan.FromMinutes(1), filter: filter).WithCancellation(cancellationToken))
{
    Console.WriteLine(match.Value);
}

var link = await inbox.WaitForXPathAsync("//a[@class='verify']/@href", filter: filter);
await foreach (var value in inbox.WaitForXPathValuesAsync(
    "//a/@href", duration: TimeSpan.FromMinutes(1), filter: filter))
{
    Console.WriteLine(value);
}
```

Single extraction consumes one matching email and returns the first result.
Plural extraction continues across **all emails during the total duration**,
yielding every occurrence in each email. It is lazy and holds only the native
result batch for the current email. Consumer processing counts toward the total
duration. No match leaves an email available for another filter or query. A match
consumes the email once; early termination does not replay remaining occurrences
from that email. Finite single waits throw `TimeoutException`; plural duration
expiry ends normally. Cancellation leaves the inbox usable.

`RegexQuery` selects `text` (default), `html`, `subject`, or `raw` and configures
case, multiline anchors, and dot/newline behavior. `RegexMatch` includes `EmailId`,
the matching `Value`, and indexed/named groups. Group zero is the entire match;
unmatched optional groups have a null value. Queries use the shared Rust regex
syntax; look-around and backreferences are not supported. XPath uses the common
HTML parsing and XPath engine, so it also accepts ordinary, imperfect HTML.

All `EmailFilter` conditions combine with AND. Sender, subject, To/Cc, text, and
header values use case-insensitive substrings. Recipient, envelope sender, and
Message-ID use exact case-insensitive comparison. `Headers` keys match header
names case-insensitively; any repeated value may satisfy a condition.
An operation filter retains nonmatching stored mail. Set `InboxOptions.Filter`
to discard nonmatching incoming mail **before local storage**; this global filter
also combines with each operation filter. Invalid expressions and filter values
raise `AhraraException` without consuming messages. Searches share the inbox's
receiver gate with ordinary email waits.

## Reopen an identity and preserve history

```csharp
var options = new InboxOptions
{
    ProfilePath = Path.GetFullPath("test-state/inbox")
};

await using (var first = await Ahrara.CreateInboxAsync(options))
{
    // Receive mail. The profile retains its address, identity, and encrypted history.
}

await using var reopened = await Ahrara.CreateInboxAsync(options);
var page = await reopened.ListEmailsAsync();
```

Without `ProfilePath`, storage is temporary and removed on disposal. With it,
history and receiving state survive later executions. Protect this directory and
exclude it from version control. Close its active inbox before reopening it.

`inbox.ExportCredentials()` returns `InboxCredentials` containing `Address` and
the secret `RecoveryKey`. Pass it as `InboxOptions.Credentials` to restore that
address and identity elsewhere. Credentials alone do not copy history. To import
an existing identity key directly, use `RecoveryKey` plus optional `Address` in
the options; do not combine these fields with `Credentials`. The core verifies
that the address belongs to the supplied identity.

Credentials and options redact their `ToString()` output. Explicit serialization
still contains the key: never log, commit, or share it.

## Read and manage stored messages

```csharp
var page = await inbox.ListEmailsAsync(limit: 20, subject: "Verification");
Directory.CreateDirectory("downloads");
foreach (var summary in page.Emails)
{
    using var email = await inbox.GetEmailAsync(summary.Id);
    var attachmentIndex = 0;
    foreach (var attachment in email.Attachments)
    {
        // Standard .NET attachment content is already available.
        // Choose the destination and overwrite policy in the application.
        var path = Path.Combine("downloads", $"{summary.Id}-{attachmentIndex++}.bin");
        await using var destination = new FileStream(path, FileMode.CreateNew);
        await attachment.ContentStream.CopyToAsync(destination);
    }
}
// Pass page.NextCursor as afterId to retrieve another page when it is not null.
await inbox.DeleteEmailAsync(emailId);
await inbox.ClearEmailsAsync();
```

Listing and reading do not consume mail. `GetRawEmailAsync` returns original
message bytes; `GetHtmlAsync` returns sanitized HTML with remote resources blocked
by default. `GetEmailId` recovers the storage identifier from an original message
returned by this inbox, without adding headers to the message.
Objects created by the caller or returned by another inbox are rejected.
Attachments expose their bytes through the standard `ContentStream`; application
code controls saving and naming. Missing messages are reported by native error
codes. Deletion and clearing
affect only this inbox's local storage.

`Body` contains the complete decoded plain text when present, otherwise HTML with
`IsBodyHtml` set. Available text and HTML bodies are also exposed as
`AlternateViews`. `Headers` retains repeated source headers. Structured addresses
populate `From`, `Sender`, `To`, `CC`, `Bcc`, and `ReplyToList`. Inline content is
included in `Attachments` with `ContentDisposition.Inline` and `ContentId`.

This is a projection into the standard .NET mail model: `From` and `Sender` select
the first mailbox, while the full source headers remain available. It does not
reproduce an arbitrary nested MIME tree or populate `AlternateView.LinkedResources`.
Use `GetRawEmailAsync` when the original MIME structure and bytes matter.
The standard object's HTML is the original content; use `GetHtmlAsync` when
sanitized HTML is needed. The SDK does not render HTML.

The API follows [.NET MailMessage ownership and attachment conventions](https://learn.microsoft.com/en-us/dotnet/api/system.net.mail.mailmessage?view=net-10.0).

## Development

For source builds and native package preparation, see
[SDK development](../../../CONTRIBUTING.md#sdk-development). Run
`make sdk-dotnet-test` against a development relay, or `make sdk-test` for the
complete matrix with an isolated relay. All builds and tests run in Docker.
