using System.Net.Mail;
using AhraraSdk;
using Client = AhraraSdk.Ahrara;
using Xunit;
using Assert = Xunit.Assert;

namespace Ahrara.FrameworkTests;

public sealed class ReceivingTests
{
    [Fact]
    public async Task LazyStreamReceivesBufferedMailAndEarlyBreakLeavesInboxUsable()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        await Relay.SendAsync(inbox.Address, "first", "one");
        await Relay.SendAsync(inbox.Address, "second", "two");
        await Relay.SendAsync(inbox.Address, "third", "three");
        var received = new List<string>();

        await foreach (var email in inbox.WaitForEmailsAsync())
        {
            using (email) received.Add(email.Subject);
            if (received.Count == 2) break;
        }

        Assert.Equal(new[] { "first", "second" }, received);
        using var third = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(3));

        Assert.Equal("third", third.Subject);
    }

    [Fact]
    public async Task CallbackIsAwaitedSequentiallyAndTotalDurationEndsNormally()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        await Relay.SendAsync(inbox.Address, "one", "one");
        await Relay.SendAsync(inbox.Address, "two", "two");
        var active = 0;
        var received = new List<string>();
        await inbox.ReceiveEmailsAsync(async (email, cancellation) =>
        {
            using var ownedMessage = email;
            Assert.Equal(1, Interlocked.Increment(ref active));
            await Task.Delay(30, cancellation);
            received.Add(email.Subject);
            Interlocked.Decrement(ref active);
        }, duration: TimeSpan.FromMilliseconds(250));

        Assert.Equal(new[] { "one", "two" }, received);
        Assert.Equal(0, active);
    }

    [Fact]
    public async Task CallbackFailurePropagatesWithoutClosingOrPrefetching()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        await Relay.SendAsync(inbox.Address, "failure", "one");
        await Relay.SendAsync(inbox.Address, "next", "two");
        var expected = new InvalidOperationException("synthetic handler failure");
        var actual = await Assert.ThrowsAsync<InvalidOperationException>(() =>
            inbox.ReceiveEmailsAsync((email, _) =>
            {
                email.Dispose();
                return Task.FromException(expected);
            }));

        Assert.Same(expected, actual);
        using var next = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(3));

        Assert.Equal("next", next.Subject);
    }

    [Fact]
    public async Task WithCancellationStopsDefaultIndefiniteStreamAndKeepsInboxUsable()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        using var cancellation = new CancellationTokenSource(150);
        await Assert.ThrowsAnyAsync<OperationCanceledException>(async () =>
        {
            await foreach (var email in inbox.WaitForEmailsAsync().WithCancellation(cancellation.Token)) email.Dispose();
        });
        await Relay.SendAsync(inbox.Address, "after cancellation", "still connected");
        using var after = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(3));

        Assert.Equal("after cancellation", after.Subject);
    }

    [Fact]
    public async Task CancellationReachesTheAwaitedCallbackAndLeavesFollowingMailAvailable()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        await Relay.SendAsync(inbox.Address, "cancel handler", "one");
        await Relay.SendAsync(inbox.Address, "after handler", "two");
        using var cancellation = new CancellationTokenSource();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() =>
            inbox.ReceiveEmailsAsync(async (email, token) =>
            {
                using var ownedMessage = email;
                cancellation.Cancel();
                await Task.Delay(Timeout.Infinite, token);
            }, cancellationToken: cancellation.Token));
        using var after = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(3));

        Assert.Equal("after handler", after.Subject);
    }

    [Fact]
    public async Task EmptyFiniteStreamCompletesNormally()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        var received = new List<string>();

        await foreach (var email in inbox.WaitForEmailsAsync(TimeSpan.FromMilliseconds(120)))
        {
            using (email) received.Add(email.Subject);
        }

        Assert.Empty(received);
        await Assert.ThrowsAsync<TimeoutException>(() => inbox.WaitForEmailAsync(TimeSpan.FromMilliseconds(120)));
    }
}
