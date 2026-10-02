using AhraraSdk;
using Client = AhraraSdk.Ahrara;
using XAssert = Xunit.Assert;
using NAssert = NUnit.Framework.Assert;
using MAssert = Microsoft.VisualStudio.TestTools.UnitTesting.Assert;

namespace Ahrara.FrameworkTests;

[NUnit.Framework.TestFixture]
public sealed class NUnitInboxTests
{
    [NUnit.Framework.Test]
    public async Task ParallelInboxesAreIsolated()
    {
        await using var first = await Client.CreateInboxAsync(Relay.Options);
        await using var second = await Client.CreateInboxAsync(Relay.Options);
        NAssert.That(first.Address != second.Address, NUnit.Framework.Is.True, "Inboxes must be isolated.");
        var waiting = first.WaitForEmailAsync(TimeSpan.FromSeconds(10));
        await Relay.SendAsync(second.Address, "Second inbox", "second");
        await Relay.SendAsync(first.Address, "First inbox", "first");
        using var firstEmail = await waiting;
        using var secondEmail = await second.WaitForEmailAsync();
        NAssert.That(firstEmail.Subject, NUnit.Framework.Is.EqualTo("First inbox"));
        NAssert.That(secondEmail.Subject, NUnit.Framework.Is.EqualTo("Second inbox"));
    }
}
