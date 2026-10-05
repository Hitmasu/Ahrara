using System.Diagnostics;
using System.Net.Mail;
using System.Runtime.CompilerServices;
using System.Text.Json;

namespace AhraraSdk;

/// <summary>Owns a connected inbox and exposes lazy, cancellable email reception.</summary>
/// <remarks>Dispose the inbox explicitly. Ending a stream or callback receiver leaves the inbox connected.</remarks>
public sealed class Inbox : IDisposable, IAsyncDisposable
{
    private readonly InboxHandle _handle;
    private readonly SemaphoreSlim _wait = new(1, 1);
    private readonly MailMessageMapper _messages = new();
    private int _disposed;

    internal Inbox(ulong handle, string address)
    {
        _handle = new InboxHandle(handle);
        Address = address;
    }

    /// <summary>Gets the connected inbox address.</summary>
    public string Address { get; }

    /// <summary>Exports the address and secret recovery key for reconnecting this identity.</summary>
    /// <returns>Credentials that must be stored securely. Email history is preserved separately in a persistent profile.</returns>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="AhraraException">The native export fails.</exception>
    public InboxCredentials ExportCredentials()
    {
        ObjectDisposedException.ThrowIf(_handle.IsClosed, this);
        var result = Call(new { op = "export_credentials", handle = _handle.Id });
        return new InboxCredentials(
            result.GetProperty("address").GetString()!,
            result.GetProperty("recovery_key").GetString()!);
    }

    /// <summary>Waits for and consumes the next matching email, including buffered mail.</summary>
    /// <param name="timeout">An optional positive timeout. Null waits indefinitely until mail, cancellation, disposal, or an error.</param>
    /// <param name="subject">Optional subject substring filter.</param>
    /// <param name="from">Optional sender substring filter.</param>
    /// <param name="cancellationToken">Cancels only this wait; the inbox remains usable.</param>
    /// <param name="filter">Additional AND conditions. Nonmatching messages remain available.</param>
    /// <returns>The next matching message, owned by the caller. Dispose it after use. Other messages remain available.</returns>
    /// <exception cref="ArgumentOutOfRangeException">The timeout is not positive.</exception>
    /// <exception cref="TimeoutException">The supplied timeout expires.</exception>
    /// <exception cref="OperationCanceledException">The cancellation token was cancelled.</exception>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="AhraraException">Reception fails in the native core.</exception>
    public async Task<MailMessage> WaitForEmailAsync(
        TimeSpan? timeout = null,
        string? subject = null,
        string? from = null,
        CancellationToken cancellationToken = default,
        EmailFilter? filter = null)
    {
        var result = await WaitForResultAsync(timeout, cancellationToken, timeoutMs => new
        {
            op = "wait",
            format = "mail_message",
            handle = _handle.Id,
            timeout_ms = timeoutMs,
            subject,
            from,
            filter
        }).ConfigureAwait(false);

        return _messages.ToMailMessage(result.GetProperty("email"));
    }

    private async Task<JsonElement> WaitForResultAsync(
        TimeSpan? timeout,
        CancellationToken cancellationToken,
        Func<uint, object> request)
    {
        ValidateDuration(timeout, nameof(timeout));
        ObjectDisposedException.ThrowIf(_handle.IsClosed, this);
        var timer = Stopwatch.StartNew();

        while (!await _wait.WaitAsync(TimeSpan.FromMilliseconds(100), cancellationToken).ConfigureAwait(false))
        {
            ObjectDisposedException.ThrowIf(_handle.IsClosed, this);
            if (timeout is { } limit && timer.Elapsed >= limit)
            {
                throw TimedOut();
            }
        }
        try
        {
            while (true)
            {
                cancellationToken.ThrowIfCancellationRequested();
                ObjectDisposedException.ThrowIf(_handle.IsClosed, this);
                var remaining = timeout - timer.Elapsed;
                if (remaining <= TimeSpan.Zero)
                {
                    throw TimedOut();
                }

                var slice = remaining is { } duration
                    ? Math.Min(100, Math.Ceiling(duration.TotalMilliseconds))
                    : 100;
                var result = await Task.Run(
                    () => Call(request((uint)Math.Max(1, slice))), CancellationToken.None)
                    .ConfigureAwait(false);
                if (result.GetProperty("status").GetString() == "matched")
                {
                    return result;
                }
            }
        }
        finally
        {
            _wait.Release();
        }
    }

    /// <summary>Lazily receives matching emails until cancellation, an optional total duration, or an early break.</summary>
    /// <param name="duration">Optional positive total receiving duration, measured from first enumeration and including consumer processing time.</param>
    /// <param name="subject">Optional subject substring filter.</param>
    /// <param name="from">Optional sender substring filter.</param>
    /// <param name="cancellationToken">Cancels enumeration. Supports <c>WithCancellation</c>.</param>
    /// <param name="filter">Additional AND conditions that retain nonmatching mail.</param>
    /// <returns>A sequence with no background queue. The caller owns and must dispose each message. Ending enumeration leaves the inbox connected.</returns>
    /// <exception cref="ArgumentOutOfRangeException">The duration is not positive.</exception>
    /// <exception cref="OperationCanceledException">Enumeration is cancelled.</exception>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="AhraraException">Reception fails in the native core.</exception>
    public async IAsyncEnumerable<MailMessage> WaitForEmailsAsync(
        TimeSpan? duration = null,
        string? subject = null,
        string? from = null,
        [EnumeratorCancellation] CancellationToken cancellationToken = default,
        EmailFilter? filter = null)
    {
        ValidateDuration(duration, nameof(duration));
        var timer = Stopwatch.StartNew();

        while (true)
        {
            cancellationToken.ThrowIfCancellationRequested();
            var remaining = duration - timer.Elapsed;
            if (remaining <= TimeSpan.Zero)
            {
                yield break;
            }

            MailMessage email;
            try
            {
                email = await WaitForEmailAsync(remaining, subject, from, cancellationToken, filter)
                    .ConfigureAwait(false);
            }
            catch (TimeoutException)
            {
                yield break;
            }

            yield return email;
        }
    }

    /// <summary>Receives emails and awaits each callback sequentially.</summary>
    /// <param name="handler">The asynchronous handler owns each message and must dispose it. Handler failures propagate; delivered mail remains consumed.</param>
    /// <param name="duration">Optional positive total duration, including handler time. A running handler is awaited before return.</param>
    /// <param name="subject">Optional subject substring filter.</param>
    /// <param name="from">Optional sender substring filter.</param>
    /// <param name="cancellationToken">Cancels receiving and is passed to the handler.</param>
    /// <param name="filter">Additional AND conditions that retain nonmatching mail.</param>
    /// <returns>A task completing after receiving stops and the current handler finishes. The inbox remains connected.</returns>
    /// <exception cref="ArgumentNullException">The handler is null.</exception>
    /// <exception cref="ArgumentOutOfRangeException">The duration is not positive.</exception>
    /// <exception cref="OperationCanceledException">Receiving or the handler is cancelled.</exception>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="AhraraException">Reception fails in the native core.</exception>
    public async Task ReceiveEmailsAsync(
        Func<MailMessage, CancellationToken, Task> handler,
        TimeSpan? duration = null,
        string? subject = null,
        string? from = null,
        CancellationToken cancellationToken = default,
        EmailFilter? filter = null)
    {
        ArgumentNullException.ThrowIfNull(handler);
        await foreach (var email in WaitForEmailsAsync(duration, subject, from, cancellationToken, filter)
            .ConfigureAwait(false))
        {
            await handler(email, cancellationToken).ConfigureAwait(false);
        }
    }


    /// <summary>Waits for the first regex match in one matching email.</summary>
    /// <param name="query">The regex pattern, source, and flags for the shared Rust engine.</param>
    /// <param name="timeout">Optional positive timeout; null waits indefinitely.</param>
    /// <param name="filter">Optional additional email conditions. Nonmatching emails remain available.</param>
    /// <param name="cancellationToken">Cancels this wait while leaving the inbox usable.</param>
    /// <returns>The first matching result. Its source email is consumed once.</returns>
    /// <exception cref="ArgumentNullException">The query is null.</exception>
    /// <exception cref="ArgumentOutOfRangeException">The timeout is not positive.</exception>
    /// <exception cref="TimeoutException">The finite timeout expires.</exception>
    /// <exception cref="OperationCanceledException">The wait is cancelled.</exception>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="AhraraException">The expression is invalid or the search fails.</exception>
    public async Task<RegexMatch> WaitForRegexMatchAsync(
        RegexQuery query,
        TimeSpan? timeout = null,
        EmailFilter? filter = null,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(query);
        var result = await SearchOnceAsync(query.ToNative(false), timeout, filter, cancellationToken)
            .ConfigureAwait(false);
        return Native.Deserialize<RegexMatch>(result.GetProperty("matches")[0]);
    }

    /// <summary>Lazily yields all regex matches across emails until the total duration expires.</summary>
    /// <param name="query">The regex pattern, source, and flags for the shared Rust engine.</param>
    /// <param name="duration">Optional positive total duration, including consumer work; null is unlimited.</param>
    /// <param name="filter">Optional additional email conditions that retain nonmatching messages.</param>
    /// <param name="cancellationToken">Cancels enumeration; also supports WithCancellation.</param>
    /// <returns>A lazy sequence with one native email batch at a time. Expiry ends normally; stopping leaves the inbox open.</returns>
    /// <remarks>Once an email yields a match it is consumed. An early break does not replay its remaining matches.</remarks>
    /// <exception cref="ArgumentNullException">The query is null.</exception>
    /// <exception cref="ArgumentOutOfRangeException">The duration is not positive.</exception>
    /// <exception cref="OperationCanceledException">Enumeration is cancelled.</exception>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="AhraraException">The expression is invalid or the search fails.</exception>
    public IAsyncEnumerable<RegexMatch> WaitForRegexMatchesAsync(
        RegexQuery query,
        TimeSpan? duration = null,
        EmailFilter? filter = null,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(query);
        return SearchResultsAsync<RegexMatch>(
            query.ToNative(true), "matches", duration, filter, cancellationToken);
    }

    /// <summary>Waits for the first XPath value in one matching email.</summary>
    /// <param name="expression">The XPath expression evaluated against the email HTML.</param>
    /// <param name="timeout">Optional positive timeout; null waits indefinitely.</param>
    /// <param name="filter">Optional additional email conditions. Nonmatching emails remain available.</param>
    /// <param name="cancellationToken">Cancels this wait while leaving the inbox usable.</param>
    /// <returns>The first matching result. Its source email is consumed once.</returns>
    /// <exception cref="ArgumentNullException">The query is null.</exception>
    /// <exception cref="ArgumentOutOfRangeException">The timeout is not positive.</exception>
    /// <exception cref="TimeoutException">The finite timeout expires.</exception>
    /// <exception cref="OperationCanceledException">The wait is cancelled.</exception>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="AhraraException">The expression is invalid or the search fails.</exception>
    public async Task<string> WaitForXPathAsync(
        string expression,
        TimeSpan? timeout = null,
        EmailFilter? filter = null,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(expression);
        var result = await SearchOnceAsync(new { kind = "xpath", expression, all = false }, timeout, filter, cancellationToken)
            .ConfigureAwait(false);
        return Native.Deserialize<string>(result.GetProperty("values")[0]);
    }

    /// <summary>Lazily yields all XPath values across emails until the total duration expires.</summary>
    /// <param name="expression">The XPath expression evaluated against the email HTML.</param>
    /// <param name="duration">Optional positive total duration, including consumer work; null is unlimited.</param>
    /// <param name="filter">Optional additional email conditions that retain nonmatching messages.</param>
    /// <param name="cancellationToken">Cancels enumeration; also supports WithCancellation.</param>
    /// <returns>A lazy sequence with one native email batch at a time. Expiry ends normally; stopping leaves the inbox open.</returns>
    /// <remarks>Once an email yields a match it is consumed. An early break does not replay its remaining matches.</remarks>
    /// <exception cref="ArgumentNullException">The query is null.</exception>
    /// <exception cref="ArgumentOutOfRangeException">The duration is not positive.</exception>
    /// <exception cref="OperationCanceledException">Enumeration is cancelled.</exception>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="AhraraException">The expression is invalid or the search fails.</exception>
    public IAsyncEnumerable<string> WaitForXPathValuesAsync(
        string expression,
        TimeSpan? duration = null,
        EmailFilter? filter = null,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(expression);
        return SearchResultsAsync<string>(
            new { kind = "xpath", expression, all = true }, "values", duration, filter, cancellationToken);
    }


    /// <summary>Waits for the first regex match with default plain-text, case-sensitive options.</summary>
    /// <param name="pattern">The Rust regex pattern.</param>
    /// <param name="timeout">Optional positive timeout; null is unlimited.</param>
    /// <param name="filter">Additional AND conditions retaining nonmatching email.</param>
    /// <param name="cancellationToken">Cancels receiving while leaving the inbox usable.</param>
    /// <returns>The first matching result.</returns>
    /// <exception cref="ArgumentOutOfRangeException">The timeout is not positive.</exception>
    /// <exception cref="TimeoutException">The finite timeout expires.</exception>
    /// <exception cref="OperationCanceledException">Receiving is cancelled.</exception>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="AhraraException">The expression is invalid or the search fails.</exception>
    public Task<RegexMatch> WaitForRegexMatchAsync(
        string pattern,
        TimeSpan? timeout = null,
        EmailFilter? filter = null,
        CancellationToken cancellationToken = default) =>
        WaitForRegexMatchAsync(new RegexQuery(pattern), timeout, filter, cancellationToken);

    /// <summary>Streams all regex matches with default plain-text, case-sensitive options.</summary>
    /// <param name="pattern">The Rust regex pattern.</param>
    /// <param name="duration">Optional positive duration; null is unlimited.</param>
    /// <param name="filter">Additional AND conditions retaining nonmatching email.</param>
    /// <param name="cancellationToken">Cancels receiving while leaving the inbox usable.</param>
    /// <returns>A lazy sequence across all emails during the total duration.</returns>
    /// <exception cref="ArgumentOutOfRangeException">The duration is not positive.</exception>

    /// <exception cref="OperationCanceledException">Receiving is cancelled.</exception>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="AhraraException">The expression is invalid or the search fails.</exception>
    public IAsyncEnumerable<RegexMatch> WaitForRegexMatchesAsync(
        string pattern,
        TimeSpan? duration = null,
        EmailFilter? filter = null,
        CancellationToken cancellationToken = default) =>
        WaitForRegexMatchesAsync(new RegexQuery(pattern), duration, filter, cancellationToken);

    private Task<JsonElement> SearchOnceAsync(
        object query, TimeSpan? timeout, EmailFilter? filter, CancellationToken cancellationToken) =>
        WaitForResultAsync(timeout, cancellationToken, timeoutMs => new
        {
            op = "search",
            handle = _handle.Id,
            timeout_ms = timeoutMs,
            filter,
            query
        });

    private async IAsyncEnumerable<T> SearchResultsAsync<T>(
        object query,
        string resultField,
        TimeSpan? duration,
        EmailFilter? filter,
        [EnumeratorCancellation] CancellationToken cancellationToken)
    {
        ValidateDuration(duration, nameof(duration));
        var timer = Stopwatch.StartNew();

        while (true)
        {
            cancellationToken.ThrowIfCancellationRequested();
            var remaining = duration - timer.Elapsed;
            if (remaining <= TimeSpan.Zero) yield break;

            JsonElement result;
            try
            {
                result = await SearchOnceAsync(query, remaining, filter, cancellationToken)
                    .ConfigureAwait(false);
            }
            catch (TimeoutException)
            {
                yield break;
            }

            foreach (var value in result.GetProperty(resultField).EnumerateArray())
            {
                cancellationToken.ThrowIfCancellationRequested();
                if (duration is { } limit && timer.Elapsed >= limit) yield break;

                yield return Native.Deserialize<T>(value);
            }
        }
    }

    /// <summary>Lists currently stored emails without consuming them.</summary>
    /// <param name="afterId">Exclusive identifier cursor; use zero for the first page.</param>
    /// <param name="limit">Maximum results, from 1 to 100.</param>
    /// <param name="subject">Optional subject substring.</param>
    /// <param name="from">Optional sender substring.</param>
    /// <param name="cancellationToken">Cancels work that has not started; an in-flight native call completes.</param>
    /// <returns>The metadata page and optional next cursor.</returns>
    /// <exception cref="OperationCanceledException">The operation is cancelled before starting.</exception>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="AhraraException">The arguments are invalid or listing fails.</exception>
    public async Task<EmailPage> ListEmailsAsync(
        long afterId = 0,
        int limit = 20,
        string? subject = null,
        string? from = null,
        CancellationToken cancellationToken = default)
    {
        var result = await CallAsync(new
        {
            op = "list",
            handle = _handle.Id,
            after_id = afterId,
            limit,
            subject,
            from
        }, cancellationToken).ConfigureAwait(false);

        return Native.Deserialize<EmailPage>(result);
    }

    /// <summary>Reads a stored message and its attachment content without consuming it.</summary>
    /// <param name="id">The local email identifier.</param>
    /// <param name="cancellationToken">Cancels work that has not started.</param>
    /// <returns>A caller-owned message that remains usable after the inbox closes. Dispose it after use.</returns>
    /// <exception cref="AhraraException">The email is not found or reading fails.</exception>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="OperationCanceledException">The operation is cancelled before starting.</exception>
    public async Task<MailMessage> GetEmailAsync(long id, CancellationToken cancellationToken = default)
    {
        var result = await CallAsync(new { op = "get", format = "mail_message", handle = _handle.Id, id }, cancellationToken)
            .ConfigureAwait(false);
        return _messages.ToMailMessage(result);
    }

    /// <summary>Gets the local storage identifier associated with a message returned by this inbox.</summary>
    /// <param name="message">The original message instance returned by a receiving or reading operation.</param>
    /// <returns>The identifier accepted by history, deletion, and raw-message operations.</returns>
    /// <exception cref="ArgumentNullException">The message is null.</exception>
    /// <exception cref="ArgumentException">The message was not returned by this inbox.</exception>
    public long GetEmailId(MailMessage message) => _messages.GetEmailId(message);

    /// <summary>Deletes one email from this inbox's local storage.</summary>
    /// <param name="id">The email identifier.</param>
    /// <param name="cancellationToken">Cancels work that has not started.</param>
    /// <returns>The number of deleted emails.</returns>
    /// <exception cref="AhraraException">Deletion fails.</exception>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="OperationCanceledException">The operation is cancelled before starting.</exception>
    public async Task<int> DeleteEmailAsync(long id, CancellationToken cancellationToken = default)
    {
        var result = await CallAsync(new { op = "delete", handle = _handle.Id, id }, cancellationToken)
            .ConfigureAwait(false);
        return result.GetProperty("deleted").GetInt32();
    }

    /// <summary>Deletes all emails belonging to this inbox from local storage.</summary>
    /// <param name="cancellationToken">Cancels work that has not started.</param>
    /// <returns>The number of deleted emails.</returns>
    /// <exception cref="AhraraException">Clearing fails.</exception>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="OperationCanceledException">The operation is cancelled before starting.</exception>
    public async Task<int> ClearEmailsAsync(CancellationToken cancellationToken = default)
    {
        var result = await CallAsync(new { op = "clear", handle = _handle.Id }, cancellationToken)
            .ConfigureAwait(false);
        return result.GetProperty("deleted").GetInt32();
    }

    /// <summary>Reads the original RFC 5322 message bytes.</summary>
    /// <param name="id">The email identifier.</param>
    /// <param name="cancellationToken">Cancels work that has not started.</param>
    /// <returns>The original message bytes.</returns>
    /// <exception cref="AhraraException">The email is missing or reading fails.</exception>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="OperationCanceledException">The operation is cancelled before starting.</exception>
    public async Task<byte[]> GetRawEmailAsync(long id, CancellationToken cancellationToken = default)
    {
        var result = await CallAsync(new { op = "raw", handle = _handle.Id, id }, cancellationToken)
            .ConfigureAwait(false);
        return Convert.FromBase64String(result.GetProperty("content_base64").GetString()!);
    }

    /// <summary>Reads sanitized HTML, blocking remote resources by default.</summary>
    /// <param name="id">The email identifier.</param>
    /// <param name="allowRemote">Whether sanitized output may reference remote resources.</param>
    /// <param name="cancellationToken">Cancels work that has not started.</param>
    /// <returns>The sanitized HTML and remote-resource count.</returns>
    /// <exception cref="AhraraException">The email is missing or reading fails.</exception>
    /// <exception cref="ObjectDisposedException">The inbox is disposed.</exception>
    /// <exception cref="OperationCanceledException">The operation is cancelled before starting.</exception>
    public async Task<EmailHtml> GetHtmlAsync(
        long id,
        bool allowRemote = false,
        CancellationToken cancellationToken = default)
    {
        var result = await CallAsync(new
        {
            op = "html",
            handle = _handle.Id,
            id,
            allow_remote = allowRemote
        }, cancellationToken).ConfigureAwait(false);

        return Native.Deserialize<EmailHtml>(result);
    }

    private Task<JsonElement> CallAsync(object request, CancellationToken cancellationToken)
    {
        ObjectDisposedException.ThrowIf(_handle.IsClosed, this);
        return Task.Run(() => Call(request), cancellationToken);
    }

    // A call racing disposal reports the documented disposal error, not the native one.
    private JsonElement Call(object request)
    {
        try
        {
            return Native.Call(request);
        }
        catch (AhraraException error) when (error.Code == "inbox_closed" && _handle.IsClosed)
        {
            throw new ObjectDisposedException(GetType().FullName);
        }
    }

    /// <summary>Disconnects the inbox, interrupts waits, and releases its native resources. Repeated calls are safe.</summary>
    /// <remarks>Native shutdown can wait several seconds for the relay; prefer <see cref="DisposeAsync"/> in asynchronous code.</remarks>
    /// <exception cref="AhraraException">Native cleanup fails.</exception>
    public void Dispose()
    {
        if (Interlocked.Exchange(ref _disposed, 1) != 0)
        {
            return;
        }

        // Waits observe disposal at once; the handle finalizer no longer closes it.
        _handle.SetHandleAsInvalid();
        Native.Call(new { op = "close", handle = _handle.Id });
    }

    /// <summary>Disconnects the inbox and releases its resources for use with <c>await using</c>.</summary>
    /// <returns>A task completing after native shutdown, which runs without blocking the caller.</returns>
    /// <exception cref="AhraraException">Native cleanup fails.</exception>
    public ValueTask DisposeAsync() => new(Task.Run(Dispose));

    private static void ValidateDuration(TimeSpan? duration, string name)
    {
        if (duration <= TimeSpan.Zero)
        {
            throw new ArgumentOutOfRangeException(name, "Duration must be positive when supplied.");
        }
    }

    private static TimeoutException TimedOut() => new("Timed out waiting for an Ahrara email.");
}
