using System.Text.Json.Serialization;

namespace AhraraSdk;

/// <summary>Contains the secret identity key and address needed to reconnect an inbox.</summary>
/// <remarks>Store securely and never log or commit the recovery key. Credentials alone do not contain email history.</remarks>
public sealed class InboxCredentials
{
    /// <summary>Initializes credentials for an existing inbox.</summary>
    /// <param name="address">The full inbox address belonging to the identity.</param>
    /// <param name="recoveryKey">The secret exported recovery key.</param>
    /// <exception cref="ArgumentException">An argument is empty or whitespace.</exception>
    public InboxCredentials(string address, string recoveryKey)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(address);
        ArgumentException.ThrowIfNullOrWhiteSpace(recoveryKey);
        Address = address;
        RecoveryKey = recoveryKey;
    }

    /// <summary>Gets the address restored with this identity.</summary>
    [JsonPropertyName("address")]
    public string Address { get; }

    /// <summary>Gets the secret recovery key. It grants access to the identity.</summary>
    [JsonPropertyName("recovery_key")]
    public string RecoveryKey { get; }

    /// <summary>Returns a redacted representation suitable for diagnostics.</summary>
    /// <returns>A representation containing no address or recovery key.</returns>
    public override string ToString() => "InboxCredentials { [REDACTED] }";
}
