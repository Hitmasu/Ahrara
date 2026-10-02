using System.Text.Json.Serialization;

namespace AhraraSdk;

/// <summary>Contains a page of stored email metadata.</summary>
public sealed record EmailPage
{
    /// <summary>Gets the email metadata in ascending identifier order.</summary>
    [JsonPropertyName("emails")]
    public IReadOnlyList<EmailSummary> Emails { get; init; } = Array.Empty<EmailSummary>();

    /// <summary>Gets the cursor for another page, or null when no additional page is available.</summary>
    [JsonPropertyName("next_cursor")]
    public long? NextCursor { get; init; }
}
