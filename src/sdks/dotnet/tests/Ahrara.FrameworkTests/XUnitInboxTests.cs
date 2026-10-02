using AhraraSdk;
using Client = AhraraSdk.Ahrara;
using XAssert = Xunit.Assert;
using NAssert = NUnit.Framework.Assert;
using MAssert = Microsoft.VisualStudio.TestTools.UnitTesting.Assert;

namespace Ahrara.FrameworkTests;

public sealed class XUnitInboxTests
{
    [Xunit.Fact]
    public async Task ReceivesBufferedUnicodeEmailAndConsumesItOnce()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        await Relay.SendAsync(inbox.Address, "Confirmação 日本語", "Your code: 123456");
        using var email = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(10), subject: "日本語");
        XAssert.Equal("Confirmação 日本語", email.Subject);
        XAssert.Contains("123456", email.Body);
        XAssert.Equal("sdk@example.test", email.From?.Address);
        await XAssert.ThrowsAsync<TimeoutException>(() =>
            inbox.WaitForEmailAsync(TimeSpan.FromMilliseconds(150)));
    }

    [Xunit.Fact]
    public async Task CancellationLeavesTheInboxUsable()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        using var cancellation = new CancellationTokenSource(150);
        await XAssert.ThrowsAnyAsync<OperationCanceledException>(() =>
            inbox.WaitForEmailAsync(cancellationToken: cancellation.Token));
        await Relay.SendAsync(inbox.Address, "After cancellation", "Still connected");
        using var email = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(10));
        XAssert.Equal("After cancellation", email.Subject);
    }

    [Xunit.Fact]
    public async Task DisposalInterruptsAnActiveWaitAndIsIdempotent()
    {
        var inbox = await Client.CreateInboxAsync(Relay.Options);
        var pending = inbox.WaitForEmailAsync(TimeSpan.FromSeconds(20));
        await Task.Delay(50);
        await inbox.DisposeAsync();
        await inbox.DisposeAsync();
        var error = await XAssert.ThrowsAnyAsync<Exception>(async () => await pending.WaitAsync(TimeSpan.FromSeconds(2)));
        XAssert.True(error is ObjectDisposedException or AhraraException);
        await XAssert.ThrowsAsync<ObjectDisposedException>(() => inbox.WaitForEmailAsync());
    }
}
