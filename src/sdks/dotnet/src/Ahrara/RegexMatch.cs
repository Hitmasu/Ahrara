using System.Text.Json.Serialization;

namespace AhraraSdk;

/// <summary>Contains a regular-expression match and its source email identifier.</summary>
public sealed record RegexMatch
{
    /// <summary>Gets the local identifier used for history and raw-message operations.</summary>
    [JsonPropertyName("email_id")]
    public long EmailId { get; init; }

    /// <summary>Gets the complete matching text.</summary>
    [JsonPropertyName("value")]
    public string Value { get; init; } = "";

    /// <summary>Gets all captured groups, including group zero and unmatched optional groups.</summary>
    [JsonPropertyName("groups")]
    public IReadOnlyList<RegexGroup> Groups { get; init; } = Array.Empty<RegexGroup>();
}
