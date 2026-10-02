using System.Text.Json.Serialization;

namespace AhraraSdk;

/// <summary>Contains one captured group; group zero is the complete match.</summary>
public sealed record RegexGroup
{
    /// <summary>Gets the zero-based group index.</summary>
    [JsonPropertyName("index")]
    public int Index { get; init; }

    /// <summary>Gets the group name, or null for an unnamed group.</summary>
    [JsonPropertyName("name")]
    public string? Name { get; init; }

    /// <summary>Gets the capture text, or null when an optional group did not participate.</summary>
    [JsonPropertyName("value")]
    public string? Value { get; init; }
}
