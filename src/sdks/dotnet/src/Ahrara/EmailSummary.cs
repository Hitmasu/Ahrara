namespace AhraraSdk;

/// <summary>Contains stored email metadata. Fetch the full email to read its body and attachments.</summary>
public sealed record EmailSummary
{

    /// <summary>Gets the relay transfer identifier.</summary>
    public string TransferId { get; init; } = "";

    /// <summary>Gets the public fingerprint of the receiving identity.</summary>
    public string IdentityFingerprint { get; init; } = "";

    /// <summary>Gets the SMTP envelope recipient.</summary>
    public string Recipient { get; init; } = "";

    /// <summary>Gets the SMTP envelope sender.</summary>
    public string EnvelopeSender { get; init; } = "";

    /// <summary>Gets the Message-ID header, when present.</summary>
    public string? MessageId { get; init; }

    /// <summary>Gets the local email identifier.</summary>
    public long Id { get; init; }

    /// <summary>Gets the decoded subject.</summary>
    public string Subject { get; init; } = "";

    /// <summary>Gets the decoded sender header.</summary>
    public string From { get; init; } = "";

    /// <summary>Gets the decoded recipient header.</summary>
    public string To { get; init; } = "";

    /// <summary>Gets the time the local receiver stored the message.</summary>
    public string ReceivedAt { get; init; } = "";

    /// <summary>Gets the original message size in bytes.</summary>
    public long SizeBytes { get; init; }
}
