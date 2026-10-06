using System.Text.Json.Serialization;

namespace AhraraSdk;

/// <summary>Configures the relay, inbox identity, and optional persistent storage.</summary>
public sealed record InboxOptions
{
    /// <summary>Gets an optional global filter. Nonmatching incoming mail is discarded before local storage.</summary>
    public EmailFilter? Filter { get; init; }

    /// <summary>Gets the secure relay URL; null uses the native default.</summary>
    public string? ServerUrl { get; init; }

    /// <summary>Gets the independently verified relay public key.</summary>
    public string? ServerPublicKey { get; init; }

    /// <summary>Gets an optional PEM CA file for a private relay.</summary>
    public string? TlsCaPath { get; init; }

    /// <summary>Gets the connection timeout in milliseconds, between 1 and 300,000; null uses the native default.</summary>
    [JsonPropertyName("connect_timeout_ms")]
    public uint? ConnectTimeoutMilliseconds { get; init; }

    /// <summary>Gets the credentials to restore an inbox identity and address. Close its previous connection first.</summary>
    public InboxCredentials? Credentials { get; init; }

    /// <summary>Gets an optional persistent profile directory. Null creates temporary storage removed on disposal.</summary>
    public string? ProfilePath { get; init; }

    /// <summary>Gets an optional secret identity key to import. Cannot be combined with <see cref="Credentials"/>.</summary>
    public string? RecoveryKey { get; init; }

    /// <summary>Gets an optional owned address to reopen with <see cref="RecoveryKey"/> or an existing profile.</summary>
    public string? Address { get; init; }

    /// <summary>Returns connection settings with all potentially sensitive values redacted.</summary>
    /// <returns>A redacted representation.</returns>
    public override string ToString() => "InboxOptions { [REDACTED] }";
}
