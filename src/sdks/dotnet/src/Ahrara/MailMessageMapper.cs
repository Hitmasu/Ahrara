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
            // The native receive already consumed this email, so mapping must tolerate
            // received values that MailMessage rejects. GetRawEmailAsync keeps them all.
            foreach (var header in value.GetProperty("headers").EnumerateArray())
            {
                try
                {
                    message.Headers.Add(
                        header.GetProperty("name").GetString(),
                        header.GetProperty("value").GetString());
                }
                catch (Exception error) when (error is ArgumentException or FormatException)
                {
                    // For example, MailMessage rejects empty header values.
                }
            }

            message.Subject = value.GetProperty("subject").GetString()!.ReplaceLineEndings(" ");
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
            attachment = new Attachment(content, ContentTypeOrDefault(value.GetProperty("content_type").GetString()!))
            {
                Name = filename
            };
            attachment.ContentDisposition!.FileName = filename;
            attachment.ContentDisposition.DispositionType = value.GetProperty("content_disposition").GetString()!;
            if (value.GetProperty("content_id").GetString() is { Length: > 0 } contentId
                && contentId.IndexOfAny(['<', '>']) < 0)
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

    private static ContentType ContentTypeOrDefault(string value)
    {
        try
        {
            return new ContentType(value);
        }
        catch (Exception error) when (error is ArgumentException or FormatException)
        {
            return new ContentType(MediaTypeNames.Application.Octet);
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
            if (Address(address) is { } valid)
            {
                return valid;
            }
        }

        return null;
    }

    private static void AddAddresses(MailAddressCollection target, JsonElement addresses)
    {
        foreach (var address in addresses.EnumerateArray())
        {
            if (Address(address) is { } valid)
            {
                target.Add(valid);
            }
        }
    }

    // Mailboxes MailAddress rejects remain available in Headers.
    private static MailAddress? Address(JsonElement address) => MailAddress.TryCreate(
        address.GetProperty("address").GetString(),
        address.GetProperty("name").GetString(),
        Encoding.UTF8,
        out var result) ? result : null;
}
