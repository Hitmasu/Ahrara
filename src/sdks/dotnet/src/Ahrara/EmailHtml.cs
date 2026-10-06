namespace AhraraSdk;

/// <summary>Contains HTML sanitized by the Rust core and information about remote resources.</summary>
public sealed record EmailHtml
{
    /// <summary>Gets the sanitized HTML.</summary>
    public string Html { get; init; } = "";

    /// <summary>Gets the number of detected remote resources.</summary>
    public int RemoteResources { get; init; }
}
