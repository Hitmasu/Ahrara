namespace AhraraSdk;

/// <summary>Creates isolated inboxes connected directly to an Ahrara relay.</summary>
public static class Ahrara
{
    /// <summary>Connects an isolated inbox directly to the relay and waits until it can receive mail.</summary>
    /// <param name="options">Connection, identity, and storage settings; null uses the native defaults.</param>
    /// <param name="cancellationToken">Cancels creation. A pending connection attempt is bounded by the connection timeout and cleaned up before cancellation is reported.</param>
    /// <returns>The connected inbox. Dispose it with <c>await using</c> when finished.</returns>
    /// <exception cref="OperationCanceledException">The cancellation token was cancelled.</exception>
    /// <exception cref="AhraraException">The configuration is invalid or the relay connection fails.</exception>
    public static async Task<Inbox> CreateInboxAsync(
        InboxOptions? options = null, CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        var result = await Task.Run(() => Native.Call(new
        {
            op = "create",
            options = options ?? new InboxOptions()
        }), CancellationToken.None).ConfigureAwait(false);
        var inbox = new Inbox(result.GetProperty("handle").GetUInt64(),
            result.GetProperty("address").GetString()!);
        if (cancellationToken.IsCancellationRequested)
        {
            inbox.Dispose();
            cancellationToken.ThrowIfCancellationRequested();
        }

        return inbox;
    }
}
