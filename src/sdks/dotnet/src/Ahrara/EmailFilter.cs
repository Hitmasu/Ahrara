using System.Text.Json.Serialization;

namespace AhraraSdk;

/// <summary>Combines email conditions with AND. Strings are literal unless Regex is enabled. Method filters retain nonmatching mail; inbox filters discard it before storage.</summary>
/// <remarks>With <see cref="Regex"/>, <see cref="Recipient"/>, <see cref="EnvelopeSender"/>, and <see cref="MessageId"/> patterns must match the whole value; other patterns match anywhere.</remarks>
public sealed record EmailFilter
{
    /// <summary>Gets whether string conditions, including header values, use case-insensitive Rust regex. Header names remain literal; <c>(?-i)</c> restores case sensitivity.</summary>
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)]
    public bool Regex { get; init; }

    /// <summary>Gets the optional case-insensitive sender substring.</summary>
    public string? Sender { get; init; }

    /// <summary>Gets the optional case-insensitive subject substring.</summary>
    public string? Subject { get; init; }

    /// <summary>Gets the optional case-insensitive To header substring.</summary>
    public string? To { get; init; }

    /// <summary>Gets the optional case-insensitive Cc header substring.</summary>
    public string? Cc { get; init; }

    /// <summary>Gets the optional whole, case-insensitive SMTP envelope recipient, or an anchored pattern with <see cref="Regex"/>.</summary>
    public string? Recipient { get; init; }

    /// <summary>Gets the optional whole, case-insensitive SMTP envelope sender, or an anchored pattern with <see cref="Regex"/>.</summary>
    public string? EnvelopeSender { get; init; }

    /// <summary>Gets the optional whole, case-insensitive Message-ID header, or an anchored pattern with <see cref="Regex"/>.</summary>
    public string? MessageId { get; init; }

    /// <summary>Gets the optional case-insensitive plain-text body substring.</summary>
    public string? Text { get; init; }

    /// <summary>Gets the optional attachment-presence condition.</summary>
    public bool? HasAttachments { get; init; }

    /// <summary>Gets conditions keyed by literal, case-insensitive header name, with case-insensitive value substrings or unanchored patterns. All conditions must match.</summary>
    public IReadOnlyDictionary<string, string>? Headers { get; init; }
}
