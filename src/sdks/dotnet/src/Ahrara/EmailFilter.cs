using System.Text.Json.Serialization;

namespace AhraraSdk;

/// <summary>Combines email conditions with AND. Method filters retain nonmatching mail; inbox filters discard it before storage.</summary>
public sealed record EmailFilter
{
    /// <summary>Gets the optional case-insensitive sender substring.</summary>
    [JsonPropertyName("sender")]
    public string? Sender { get; init; }

    /// <summary>Gets the optional case-insensitive subject substring.</summary>
    [JsonPropertyName("subject")]
    public string? Subject { get; init; }

    /// <summary>Gets the optional case-insensitive To header substring.</summary>
    [JsonPropertyName("to")]
    public string? To { get; init; }

    /// <summary>Gets the optional case-insensitive Cc header substring.</summary>
    [JsonPropertyName("cc")]
    public string? Cc { get; init; }

    /// <summary>Gets the optional exact, case-insensitive SMTP envelope recipient.</summary>
    [JsonPropertyName("recipient")]
    public string? Recipient { get; init; }

    /// <summary>Gets the optional exact, case-insensitive SMTP envelope sender.</summary>
    [JsonPropertyName("envelope_sender")]
    public string? EnvelopeSender { get; init; }

    /// <summary>Gets the optional exact, case-insensitive Message-ID header.</summary>
    [JsonPropertyName("message_id")]
    public string? MessageId { get; init; }

    /// <summary>Gets the optional case-insensitive plain-text body substring.</summary>
    [JsonPropertyName("text")]
    public string? Text { get; init; }

    /// <summary>Gets the optional attachment-presence condition.</summary>
    [JsonPropertyName("has_attachments")]
    public bool? HasAttachments { get; init; }

    /// <summary>Gets conditions keyed by case-insensitive header name, with case-insensitive value substrings. All conditions must match.</summary>
    [JsonPropertyName("headers")]
    public IReadOnlyDictionary<string, string>? Headers { get; init; }
}
