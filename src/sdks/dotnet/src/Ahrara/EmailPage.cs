namespace AhraraSdk;

/// <summary>Contains a page of stored email metadata.</summary>
public sealed record EmailPage
{
    /// <summary>Gets the email metadata in ascending identifier order.</summary>
    public IReadOnlyList<EmailSummary> Emails { get; init; } = Array.Empty<EmailSummary>();

    /// <summary>Gets the cursor for another page, or null when no additional page is available.</summary>
    public long? NextCursor { get; init; }
}
