using System.Text.Json.Serialization;

namespace AhraraSdk;

/// <summary>Contains stored email metadata. Fetch the full email to read its body and attachments.</summary>
public sealed record EmailSummary
{

    /// <summary>Gets the relay transfer identifier.</summary>
    [JsonPropertyName("transfer_id")]
    public string TransferId { get; init; } = "";

    /// <summary>Gets the public fingerprint of the receiving identity.</summary>
    [JsonPropertyName("identity_fingerprint")]
    public string IdentityFingerprint { get; init; } = "";

    /// <summary>Gets the SMTP envelope recipient.</summary>
    [JsonPropertyName("recipient")]
    public string Recipient { get; init; } = "";

    /// <summary>Gets the SMTP envelope sender.</summary>
    [JsonPropertyName("envelope_sender")]
    public string EnvelopeSender { get; init; } = "";

    /// <summary>Gets the Message-ID header, when present.</summary>
    [JsonPropertyName("message_id")]
    public string? MessageId { get; init; }

    /// <summary>Gets the local email identifier.</summary>
    [JsonPropertyName("id")]
    public long Id { get; init; }

    /// <summary>Gets the decoded subject.</summary>
    [JsonPropertyName("subject")]
    public string Subject { get; init; } = "";

    /// <summary>Gets the decoded sender header.</summary>
    [JsonPropertyName("from")]
    public string From { get; init; } = "";

    /// <summary>Gets the decoded recipient header.</summary>
    [JsonPropertyName("to")]
    public string To { get; init; } = "";

    /// <summary>Gets the time the local receiver stored the message.</summary>
    [JsonPropertyName("received_at")]
    public string ReceivedAt { get; init; } = "";

    /// <summary>Gets the original message size in bytes.</summary>
    [JsonPropertyName("size_bytes")]
    public long SizeBytes { get; init; }
}
