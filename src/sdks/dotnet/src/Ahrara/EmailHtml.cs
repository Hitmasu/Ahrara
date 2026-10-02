using System.Text.Json.Serialization;

namespace AhraraSdk;

/// <summary>Contains HTML sanitized by the Rust core and information about remote resources.</summary>
public sealed record EmailHtml
{
    /// <summary>Gets the sanitized HTML.</summary>
    [JsonPropertyName("html")]
    public string Html { get; init; } = "";

    /// <summary>Gets the number of detected remote resources.</summary>
    [JsonPropertyName("remote_resources")]
    public int RemoteResources { get; init; }
}
