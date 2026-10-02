using AhraraSdk;
using Client = AhraraSdk.Ahrara;
using Xunit;
using Assert = Xunit.Assert;

namespace Ahrara.FrameworkTests;

public sealed class SearchTests
{
    [Fact]
    public async Task RegexStreamReceivesEveryEmailAndAllCapturesWhileRetainingNonmatches()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        await Relay.SendAsync(inbox.Address, "unmatched", "ordinary text");
        await Relay.SendAsync(inbox.Address, "verification one", "CODE:111 CODE:222-YES");
        await Relay.SendAsync(inbox.Address, "verification two", "CODE:333");
        var matches = new List<RegexMatch>();
        var query = new RegexQuery(@"code:(?P<digits>\d+)(?:-(?P<suffix>[A-Z]+))?") { IgnoreCase = true };

        await foreach (var match in inbox.WaitForRegexMatchesAsync(query, TimeSpan.FromMilliseconds(400)))
        {
            matches.Add(match);
        }

        Assert.Equal(new[] { "CODE:111", "CODE:222-YES", "CODE:333" }, matches.Select(match => match.Value));
        Assert.Equal(2, matches.Select(match => match.EmailId).Distinct().Count());
        Assert.Equal("CODE:111", matches[0].Groups[0].Value);
        Assert.Equal("111", Assert.Single(matches[0].Groups, group => group.Name == "digits").Value);
        Assert.Null(Assert.Single(matches[0].Groups, group => group.Name == "suffix").Value);
        using var retained = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(3));

        Assert.Equal("unmatched", retained.Subject);
    }

    [Fact]
    public async Task XPathStreamsValuesAcrossEmailsAndSupportsSingleValueWait()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        await Relay.SendAsync(inbox.Address, "one", "text", "<a href='/one'>One</a><a href='/two'>Two</a>");
        await Relay.SendAsync(inbox.Address, "two", "text", "<a href='/three'>Three</a>");
        var values = new List<string>();

        await foreach (var value in inbox.WaitForXPathValuesAsync("//a/@href", TimeSpan.FromMilliseconds(400)))
        {
            values.Add(value);
        }

        Assert.Equal(new[] { "/one", "/two", "/three" }, values);
        await Relay.SendAsync(inbox.Address, "single", "text", "<p><b id='token'>Olá 日本語</b>");

        Assert.Equal("Olá 日本語", await inbox.WaitForXPathAsync("//b[@id='token']/text()", TimeSpan.FromSeconds(3)));
    }

    [Fact]
    public async Task GlobalAndOperationFiltersComposeWithoutDiscardingOperationNonmatches()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options with
        {
            Filter = new EmailFilter { Sender = "SDK@EXAMPLE.TEST", Subject = "accept" }
        });
        await Relay.SendAsync(inbox.Address, "discard", "never stored");
        await Relay.SendAsync(inbox.Address, "accept later", "retained");
        await Relay.SendAsync(inbox.Address, "accept now", "code 123", attachment: [1], configure: message =>
        {
            message.Headers.Add("X-Test-Case", "expected-value");
        });
        var filter = new EmailFilter
        {
            Subject = "now",
            To = inbox.Address,
            Recipient = inbox.Address,
            EnvelopeSender = "sdk@example.test",
            Text = "CODE",
            HasAttachments = true,
            Headers = new Dictionary<string, string> { ["x-test-case"] = "EXPECTED" }
        };
        using var selected = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(3), filter: filter);

        Assert.Equal("accept now", selected.Subject);
        using var retained = await inbox.WaitForEmailAsync(TimeSpan.FromSeconds(3));

        Assert.Equal("accept later", retained.Subject);
        Assert.Equal(2, (await inbox.ListEmailsAsync()).Emails.Count);
    }

    [Fact]
    public async Task SearchOptionsFilterInvalidExpressionsAndCancellationPreserveInbox()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        await Relay.SendAsync(inbox.Address, "Confirm 456", "body");
        var match = await inbox.WaitForRegexMatchAsync(
            new RegexQuery(@"confirm (?P<code>\d+)") { Source = "subject", IgnoreCase = true },
            TimeSpan.FromSeconds(3), new EmailFilter { Subject = "confirm" });

        Assert.Equal("456", Assert.Single(match.Groups, group => group.Name == "code").Value);
        await Assert.ThrowsAsync<AhraraException>(() => inbox.WaitForRegexMatchAsync("[", TimeSpan.FromSeconds(1)));
        await Assert.ThrowsAsync<AhraraException>(() => inbox.WaitForXPathAsync("//*[", TimeSpan.FromSeconds(1)));
        await Assert.ThrowsAsync<TimeoutException>(() => inbox.WaitForRegexMatchAsync("none", TimeSpan.FromMilliseconds(100)));
        using var cancellation = new CancellationTokenSource(150);
        await Assert.ThrowsAnyAsync<OperationCanceledException>(async () =>
        {
            await foreach (var _ in inbox.WaitForXPathValuesAsync("//a/@href").WithCancellation(cancellation.Token)) { }
        });
        await Relay.SendAsync(inbox.Address, "after cancellation", "ready 789");

        Assert.Equal("789", (await inbox.WaitForRegexMatchAsync(@"\d+", TimeSpan.FromSeconds(3))).Value);
    }

    [Fact]
    public async Task PluralSearchDurationIncludesConsumerTimeAndEarlyBreakLeavesLaterEmails()
    {
        await using var inbox = await Client.CreateInboxAsync(Relay.Options);
        await Relay.SendAsync(inbox.Address, "first", "1 2 3");
        var matches = new List<string>();

        await foreach (var match in inbox.WaitForRegexMatchesAsync(@"\d", TimeSpan.FromMilliseconds(100)))
        {
            matches.Add(match.Value);
            await Task.Delay(180);
        }

        Assert.Single(matches);
        await Relay.SendAsync(inbox.Address, "second", "4");
        await Relay.SendAsync(inbox.Address, "third", "5");

        await foreach (var _ in inbox.WaitForRegexMatchesAsync(@"\d")) break;

        Assert.Equal("5", (await inbox.WaitForRegexMatchAsync(@"\d", TimeSpan.FromSeconds(3))).Value);
    }
}
