using AhraraSdk;
using Client = AhraraSdk.Ahrara;
using XAssert = Xunit.Assert;
using NAssert = NUnit.Framework.Assert;
using MAssert = Microsoft.VisualStudio.TestTools.UnitTesting.Assert;

namespace Ahrara.FrameworkTests;

[Microsoft.VisualStudio.TestTools.UnitTesting.TestClass]
public sealed class MsTestInboxTests
{
    [Microsoft.VisualStudio.TestTools.UnitTesting.TestMethod]
    public async Task WaitsForTheRequestedSubject()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        await Relay.SendAsync(inbox.Address, "Unrelated", "ignore");
        await Relay.SendAsync(inbox.Address, "Verification", "654321");
        using var email = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(10), subject: "Verification");
        MAssert.AreEqual("Verification", email.Subject);
        MAssert.Contains("654321", email.Body);
        // A filter must preserve earlier unmatched messages for a later wait.
        using var unmatched = await inbox.WaitForEmailAsync();
        MAssert.AreEqual("Unrelated", unmatched.Subject);
    }
}
