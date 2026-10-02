using System.Text;
using AhraraSdk;
using Client = AhraraSdk.Ahrara;
using Xunit;
using Assert = Xunit.Assert;

namespace Ahrara.FrameworkTests;

public sealed class PersistenceTests
{
    [Fact]
    public async Task PersistentProfileRetainsAddressHistoryAndAttachmentContent()
    {
        var directory = Directory.CreateTempSubdirectory("ahrara-dotnet-");
        var options = Relay.Options with { ProfilePath = Path.Combine(directory.FullName, "profile") };
        byte[] expected = [0, 1, 2, 127, 255];
        InboxCredentials credentials;
        long id;
        try
        {
            await using (var inbox = await Client.CreateInboxAsync(options))
            {
                credentials = inbox.ExportCredentials();
                Assert.False(credentials.ToString().Contains(credentials.RecoveryKey), "Credentials must redact the key.");
                Assert.False((options with { RecoveryKey = credentials.RecoveryKey }).ToString().Contains(credentials.RecoveryKey), "Options must redact the key.");
                await Relay.SendAsync(inbox.Address, "persistent attachment", "stored body",
                    "<p>stored html</p><script>alert(1)</script>", expected);
                using var received = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(5));
                id = inbox.GetEmailId(received);
            }
            await using (var reopened = await Client.CreateInboxAsync(options))
            {
                Assert.True(credentials.Address == reopened.Address, "The persistent address must remain unchanged.");
                var page = await reopened.ListEmailsAsync(limit: 1);

                Assert.Single(page.Emails);
                Assert.Equal(id, page.Emails[0].Id);
                using var email = await reopened.GetEmailAsync(id);

                Assert.Contains("stored body", email.Body);
                Assert.True(email.To[0].Address == reopened.Address, "The stored recipient must belong to this inbox.");
                Assert.True(page.Emails[0].SizeBytes > 0);
                Assert.False(string.IsNullOrWhiteSpace(page.Emails[0].ReceivedAt));
                var attachment = Assert.Single(email.Attachments);

                Assert.Equal(expected, await Relay.ReadBytesAsync(attachment.ContentStream));
                attachment.ContentStream.Position = 0;
                var saved = Path.Combine(directory.FullName, "proof.bin");
                await using (var destination = new FileStream(saved, FileMode.CreateNew))
                {
                    await attachment.ContentStream.CopyToAsync(destination);
                }

                Assert.Equal(expected, await File.ReadAllBytesAsync(saved));
                Assert.Contains("persistent attachment", Encoding.UTF8.GetString(await reopened.GetRawEmailAsync(id)));
                Assert.DoesNotContain("<script", (await reopened.GetHtmlAsync(id)).Html);
                await Assert.ThrowsAsync<TimeoutException>(() => reopened.WaitForEmailAsync(TimeSpan.FromMilliseconds(120)));

                Assert.Equal(1, await reopened.DeleteEmailAsync(id));
                Assert.Empty((await reopened.ListEmailsAsync()).Emails);
                await Relay.SendAsync(reopened.Address, "to clear", "clear");
                using var clear = await reopened.WaitForEmailAsync(TimeSpan.FromSeconds(3));

                Assert.Equal(1, await reopened.ClearEmailsAsync());
            }
            await using var imported = await Client.CreateInboxAsync(Relay.Options with
            {
                RecoveryKey = credentials.RecoveryKey,
                Address = credentials.Address
            });

            Assert.True(credentials.Address == imported.Address, "Import must restore the original address.");
            Assert.Empty((await imported.ListEmailsAsync()).Emails);
            await Relay.SendAsync(imported.Address, "imported identity", "received");
            using var importedEmail = await imported.WaitForEmailAsync(TimeSpan.FromSeconds(3));

            Assert.Equal("imported identity", importedEmail.Subject);
        }
        finally
        {
            directory.Delete(true);
        }
    }

    [Fact]
    public async Task CredentialsReconnectAndRejectAnAddressFromAnotherIdentity()
    {
        InboxCredentials first;
        string otherAddress;
        await using (var inbox = await Client.CreateInboxAsync(Relay.Options)) first = inbox.ExportCredentials();
        await using (var other = await Client.CreateInboxAsync(Relay.Options)) otherAddress = other.Address;
        await Assert.ThrowsAsync<AhraraException>(() => Client.CreateInboxAsync(Relay.Options with
        {
            Credentials = new InboxCredentials(otherAddress, first.RecoveryKey)
        }));
        await using var restored = await Client.CreateInboxAsync(Relay.Options with { Credentials = first });

        Assert.True(first.Address == restored.Address, "Credentials must restore the original address.");
        await Relay.SendAsync(restored.Address, "credentials restored", "received");
        using var restoredEmail = await restored.WaitForEmailAsync(TimeSpan.FromSeconds(3));

        Assert.Equal("credentials restored", restoredEmail.Subject);
    }
}
