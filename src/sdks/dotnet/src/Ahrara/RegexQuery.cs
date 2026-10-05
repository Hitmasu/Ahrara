namespace AhraraSdk;

/// <summary>Configures a regular-expression query evaluated by the shared Rust engine.</summary>
/// <param name="Pattern">The Rust regex expression; look-around and backreferences are not supported.</param>
public sealed record RegexQuery(string Pattern)
{
    /// <summary>Gets the message source: text, html, subject, or raw. The default is text.</summary>
    public string Source { get; init; } = "text";

    /// <summary>Gets whether matching ignores case. False by default, unlike <see cref="EmailFilter.Regex"/> patterns.</summary>
    public bool IgnoreCase { get; init; }

    /// <summary>Gets whether anchors match individual lines.</summary>
    public bool MultiLine { get; init; }

    /// <summary>Gets whether a dot also matches newline characters.</summary>
    public bool DotMatchesNewLine { get; init; }

    internal object ToNative(bool all) => new
    {
        kind = "regex",
        pattern = Pattern,
        source = Source,
        ignore_case = IgnoreCase,
        multi_line = MultiLine,
        dot_matches_new_line = DotMatchesNewLine,
        all
    };
}
