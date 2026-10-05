using System.Net.Security;
using System.Security.Cryptography.X509Certificates;
using System.Text;
using AhraraSdk;
using MailKit.Net.Smtp;
using MailKit.Security;
using MimeKit;

namespace Ahrara.FrameworkTests;

internal static class Relay
{
    internal static InboxOptions Options => new()
    {
        ServerUrl = Required("AHRARA_TEST_SERVER_URL"),
        ServerPublicKey = Required("AHRARA_TEST_SERVER_PUBLIC_KEY"),
        TlsCaPath = Required("AHRARA_TEST_TLS_CA"),
        ConnectTimeoutMilliseconds = 10000
    };

    private static string Required(string name) => Environment.GetEnvironmentVariable(name)
        ?? throw new InvalidOperationException($"Set {name} for the real relay tests.");

    internal static async Task<byte[]> ReadBytesAsync(Stream source)
    {
        using var content = new MemoryStream();
        await source.CopyToAsync(content);
        return content.ToArray();
    }

    internal static async Task SendAsync(
        string address,
        string subject,
        string body,
        string? html = null,
        byte[]? attachment = null,
        Action<MimeMessage>? configure = null)
    {
        using var message = new MimeMessage();
        message.From.Add(MailboxAddress.Parse("sdk@example.test"));
        message.To.Add(MailboxAddress.Parse(address));
        message.Subject = subject;
        var content = new BodyBuilder { TextBody = body, HtmlBody = html };
        if (attachment is not null)
        {
            content.Attachments.Add("proof.bin", attachment, new ContentType("application", "octet-stream"));
        }

        message.Body = content.ToMessageBody();
        configure?.Invoke(message);
        await SendAsync(message);
    }

    // MimeKit keeps parsed header text, so malformed test headers reach the relay unchanged.
    internal static async Task SendRawAsync(string raw)
    {
        using var message = MimeMessage.Load(new MemoryStream(Encoding.UTF8.GetBytes(raw)));
        await SendAsync(message);
    }

    private static async Task SendAsync(MimeMessage message)
    {
        var endpoint = new Uri("smtp://" + Required("AHRARA_TEST_SMTP_ADDRESS"));
        using var ca = X509Certificate2.CreateFromPem(File.ReadAllText(Required("AHRARA_TEST_TLS_CA")));
        using var client = new SmtpClient();
        client.ServerCertificateValidationCallback = (_, certificate, _, errors) =>
        {
            if (certificate is null || (errors & SslPolicyErrors.RemoteCertificateNameMismatch) != 0)
                return false;

            using var chain = new X509Chain();
            chain.ChainPolicy.TrustMode = X509ChainTrustMode.CustomRootTrust;
            chain.ChainPolicy.CustomTrustStore.Add(ca);
            chain.ChainPolicy.RevocationMode = X509RevocationMode.NoCheck;
            using var leaf = new X509Certificate2(certificate);
            return chain.Build(leaf);
        };
        await client.ConnectAsync(endpoint.Host, endpoint.Port, SecureSocketOptions.StartTls);
        await client.SendAsync(message);
        await client.DisconnectAsync(true);
    }
}
