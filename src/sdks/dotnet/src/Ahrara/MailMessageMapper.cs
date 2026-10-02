using System.Net.Mail;
using System.Net.Mime;
using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;

namespace AhraraSdk;

internal sealed class MailMessageMapper
{
    private readonly ConditionalWeakTable<MailMessage, StrongBox<long>> _emailIds = new();

    internal MailMessage ToMailMessage(JsonElement value)
    {
        var message = new MailMessage();
        try
        {
            foreach (var header in value.GetProperty("headers").EnumerateArray())
            {
                message.Headers.Add(
                    header.GetProperty("name").GetString(),
                    header.GetProperty("value").GetString());
            }

            message.Subject = value.GetProperty("subject").GetString()!;
            message.SubjectEncoding = Encoding.UTF8;
            message.HeadersEncoding = Encoding.UTF8;
            message.BodyEncoding = Encoding.UTF8;
            if (FirstAddress(value.GetProperty("from")) is { } from)
            {
                message.From = from;
            }
            if (FirstAddress(value.GetProperty("sender")) is { } sender)
            {
                message.Sender = sender;
            }

            AddAddresses(message.To, value.GetProperty("to"));
            AddAddresses(message.CC, value.GetProperty("cc"));
            AddAddresses(message.Bcc, value.GetProperty("bcc"));
            AddAddresses(message.ReplyToList, value.GetProperty("reply_to"));

            var plain = value.GetProperty("text_body").GetString();
            var html = value.GetProperty("html_body").GetString();
            message.Body = plain ?? html ?? string.Empty;
            message.IsBodyHtml = plain is null && html is not null;
            if (plain is not null)
            {
                message.AlternateViews.Add(AlternateView.CreateAlternateViewFromString(
                    plain, Encoding.UTF8, MediaTypeNames.Text.Plain));
            }
            if (html is not null)
            {
                message.AlternateViews.Add(AlternateView.CreateAlternateViewFromString(
                    html, Encoding.UTF8, MediaTypeNames.Text.Html));
            }

            foreach (var attachment in value.GetProperty("attachments").EnumerateArray())
            {
                message.Attachments.Add(ToAttachment(attachment));
            }

            _emailIds.Add(message, new StrongBox<long>(
                value.GetProperty("metadata").GetProperty("id").GetInt64()));
            return message;
        }
        catch
        {
            message.Dispose();
            throw;
        }
    }

    private static Attachment ToAttachment(JsonElement value)
    {
        var content = new MemoryStream(
            Convert.FromBase64String(value.GetProperty("content_base64").GetString()!), writable: false);
        Attachment? attachment = null;
        try
        {
            var filename = value.GetProperty("filename").GetString()!;
            attachment = new Attachment(content, filename, value.GetProperty("content_type").GetString()!);
            attachment.ContentDisposition!.FileName = filename;
            attachment.ContentDisposition.DispositionType = value.GetProperty("content_disposition").GetString()!;
            if (value.GetProperty("content_id").GetString() is { Length: > 0 } contentId)
            {
                attachment.ContentId = contentId;
            }

            return attachment;
        }
        catch
        {
            if (attachment is not null)
            {
                attachment.Dispose();
            }
            else
            {
                content.Dispose();
            }

            throw;
        }
    }

    internal long GetEmailId(MailMessage message)
    {
        ArgumentNullException.ThrowIfNull(message);
        return _emailIds.TryGetValue(message, out var identity)
            ? identity.Value
            : throw new ArgumentException("The message was not returned by this inbox.", nameof(message));
    }

    private static MailAddress? FirstAddress(JsonElement addresses)
    {
        foreach (var address in addresses.EnumerateArray())
        {
            return Address(address);
        }

        return null;
    }

    private static void AddAddresses(MailAddressCollection target, JsonElement addresses)
    {
        foreach (var address in addresses.EnumerateArray())
        {
            target.Add(Address(address));
        }
    }

    private static MailAddress Address(JsonElement address) => new(
        address.GetProperty("address").GetString()!,
        address.GetProperty("name").GetString(),
        Encoding.UTF8);
}
