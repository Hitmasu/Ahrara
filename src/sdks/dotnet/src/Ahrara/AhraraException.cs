namespace AhraraSdk;

/// <summary>Reports an error returned by the embedded Rust core.</summary>
public sealed class AhraraException : Exception
{
    /// <summary>Initializes an error using the native code and message.</summary>
    /// <param name="code">The stable machine-readable error code.</param>
    /// <param name="message">The diagnostic message.</param>
    public AhraraException(string code, string message) : base(message)
    {
        Code = code;
    }

    /// <summary>Gets the stable machine-readable native error code.</summary>
    public string Code { get; }
}
