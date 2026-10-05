using System.Net.Mail;
using System.Text;
using AhraraSdk;
using MimeKit;
using Client = AhraraSdk.Ahrara;
using Xunit;
using Assert = Xunit.Assert;

namespace Ahrara.FrameworkTests;

public sealed class MailMessageTests
{
    [Fact]
    public async Task StandardMessagePreservesHeadersViewsAndAttachmentContentAfterInboxCloses()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        byte[] expected = [0, 1, 127, 255];
        await Relay.SendAsync(inbox.Address, "Confirmação 日本語", "plain body", configure: message =>
        {
            message.From.Clear();
            message.From.Add(new MailboxAddress("Doe, Jane 日本語", "jane@example.test"));
            message.ReplyTo.Add(new MailboxAddress("Support, Team", "reply@example.test"));
            message.Headers.Add("X-Delivery-Test", "first");
            message.Headers.Add("X-Delivery-Test", "second");
            var body = new BodyBuilder
            {
                TextBody = "plain body",
                HtmlBody = "<p>original html</p><img src=\"cid:logo@example.test\"><script>example()</script>"
            };
            var inline = body.LinkedResources.Add("logo.bin", expected);
            inline.ContentId = "logo@example.test";
            body.Attachments.Add("résumé.bin", expected);
            body.Attachments.Add("café.txt", [79, 108, 225],
                new ContentType("text", "plain") { Charset = "iso-8859-1" });
            message.Body = body.ToMessageBody();
        });

        using MailMessage email = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(5));

        Assert.IsType<MailMessage>(email);
        Assert.Equal("Confirmação 日本語", email.Subject);
        Assert.Equal("Doe, Jane 日本語", email.From?.DisplayName);
        Assert.Equal("jane@example.test", email.From?.Address);
        Assert.Equal("reply@example.test", Assert.Single(email.ReplyToList).Address);
        Assert.Equal(new[] { "first", "second" }, email.Headers.GetValues("X-Delivery-Test"));
        Assert.Equal("plain body", email.Body.TrimEnd());
        Assert.False(email.IsBodyHtml);
        Assert.Equal(2, email.AlternateViews.Count);
        var html = Assert.Single(email.AlternateViews, view => view.ContentType.MediaType == "text/html");

        Assert.Contains("<script>example()</script>", Encoding.UTF8.GetString(await Relay.ReadBytesAsync(html.ContentStream)));

        var id = inbox.GetEmailId(email);

        Assert.True(id > 0);
        var inlineAttachment = Assert.Single(email.Attachments, attachment => attachment.ContentDisposition!.Inline);

        Assert.Equal("logo@example.test", inlineAttachment.ContentId);
        var file = Assert.Single(email.Attachments, attachment => attachment.Name == "résumé.bin");

        Assert.Equal("résumé.bin", file.Name);
        var encodedText = Assert.Single(email.Attachments, attachment => attachment.Name == "café.txt");

        Assert.Equal("iso-8859-1", encodedText.ContentType.CharSet);

        await inbox.DisposeAsync();

        Assert.Equal(expected, await Relay.ReadBytesAsync(file.ContentStream));
        Assert.Equal(expected, await Relay.ReadBytesAsync(inlineAttachment.ContentStream));
        Assert.Equal(new byte[] { 79, 108, 225 }, await Relay.ReadBytesAsync(encodedText.ContentStream));
        var stream = file.ContentStream;
        email.Dispose();

        Assert.False(stream.CanRead);
    }

    [Fact]
    public async Task FieldsMailMessageRejectsDoNotLoseTheConsumedMessage()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        await Relay.SendRawAsync($"""
            From: sdk@example.test
            To: {inbox.Address}
            Reply-To: Broken <broken@>
            Subject: =?UTF-8?Q?Lenient=0D=0Amapping?=
            X-Empty:
            MIME-Version: 1.0
            Content-Type: multipart/mixed; boundary="part"

            --part
            Content-Type: text/plain

            lenient body
            --part
            Content-Type: invalid
            Content-Disposition: attachment; filename="odd.bin"
            Content-Transfer-Encoding: base64

            AQID
            --part--
            """);

        using var email = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(5));

        Assert.Equal("Lenient mapping", email.Subject);
        Assert.Empty(email.ReplyToList);
        Assert.Contains("broken@", email.Headers["Reply-To"]);
        var attachment = Assert.Single(email.Attachments);

        Assert.Equal("application/octet-stream", attachment.ContentType.MediaType);
        Assert.Equal("odd.bin", attachment.Name);
        Assert.Equal(new byte[] { 1, 2, 3 }, await Relay.ReadBytesAsync(attachment.ContentStream));
    }

    [Fact]
    public async Task FullBodyIsNotLimitedByTheParsedEmailPreviewBudget()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        var body = new string('a', 300_000) + " final text";
        await Relay.SendAsync(inbox.Address, "complete body", body);
        using var email = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(5));

        Assert.Equal(body, email.Body.TrimEnd());
    }

    [Fact]
    public async Task StorageIdentifiersBelongToTheOriginalObjectsAndInbox()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        await using var other = await Client.CreateInboxAsync(Relay.Options);
        await Relay.SendAsync(inbox.Address, "stored object", "body", attachment: [1, 2, 3]);
        using var email = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(5));
        using var supplied = new MailMessage();
        supplied.Headers.Add("X-Ahrara-Id", inbox.GetEmailId(email).ToString());

        Assert.Throws<ArgumentException>(() => inbox.GetEmailId(supplied));
        Assert.Throws<ArgumentException>(() => other.GetEmailId(email));
        Assert.Null(email.Headers["X-Ahrara-Id"]);
    }
}
