using AhraraSdk;
using Client = AhraraSdk.Ahrara;
using Xunit;

namespace Ahrara.FrameworkTests;

public sealed class XUnitInboxTests
{
    [Fact]
    public async Task ReceivesBufferedUnicodeEmailAndConsumesItOnce()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        await Relay.SendAsync(inbox.Address, "Confirmação 日本語", "Your code: 123456");
        using var email = await inbox.WaitForEmailAsync(
            TimeSpan.FromSeconds(10), new EmailFilter { Subject = "日本語" });
        Assert.Equal("Confirmação 日本語", email.Subject);
        Assert.Contains("123456", email.Body);
        Assert.Equal("sdk@example.test", email.From?.Address);
        await Assert.ThrowsAsync<TimeoutException>(() =>
            inbox.WaitForEmailAsync(TimeSpan.FromMilliseconds(150)));
    }

    [Fact]
    public async Task WaitsForTheRequestedSubject()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        await Relay.SendAsync(inbox.Address, "Unrelated", "ignore");
        await Relay.SendAsync(inbox.Address, "Verification", "654321");
        using var email = await inbox.WaitForEmailAsync(
            TimeSpan.FromSeconds(10), new EmailFilter { Subject = "Verification" });
        Assert.Equal("Verification", email.Subject);
        Assert.Contains("654321", email.Body);
        // A filter must preserve earlier unmatched messages for a later wait.
        using var unmatched = await inbox.WaitForEmailAsync();
        Assert.Equal("Unrelated", unmatched.Subject);
    }

    [Fact]
    public async Task ParallelInboxesAreIsolated()
    {
        await using var first = await Client.CreateInboxAsync(Relay.Options);
        await using var second = await Client.CreateInboxAsync(Relay.Options);
        Assert.NotEqual(first.Address, second.Address);
        var waiting = first.WaitForEmailAsync(TimeSpan.FromSeconds(10));
        await Relay.SendAsync(second.Address, "Second inbox", "second");
        await Relay.SendAsync(first.Address, "First inbox", "first");
        using var firstEmail = await waiting;
        using var secondEmail = await second.WaitForEmailAsync();
        Assert.Equal("First inbox", firstEmail.Subject);
        Assert.Equal("Second inbox", secondEmail.Subject);
    }

    [Fact]
    public async Task CancellationLeavesTheInboxUsable()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        using var cancellation = new CancellationTokenSource(150);
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() =>
            inbox.WaitForEmailAsync(cancellationToken: cancellation.Token));
        await Relay.SendAsync(inbox.Address, "After cancellation", "Still connected");
        using var email = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(10));
        Assert.Equal("After cancellation", email.Subject);
    }

    [Fact]
    public async Task DisposalInterruptsAnActiveWaitAndIsIdempotent()
    {
        var inbox = await Client.CreateInboxAsync(Relay.Options);
        var pending = inbox.WaitForEmailAsync(TimeSpan.FromSeconds(20));
        await Task.Delay(50);
        await inbox.DisposeAsync();
        await inbox.DisposeAsync();
        var error = await Assert.ThrowsAnyAsync<Exception>(async () => await pending.WaitAsync(TimeSpan.FromSeconds(2)));
        Assert.True(error is ObjectDisposedException or AhraraException);
        await Assert.ThrowsAsync<ObjectDisposedException>(() => inbox.WaitForEmailAsync());
    }
}
