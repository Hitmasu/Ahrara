package dev.ahrara;

import static org.junit.jupiter.api.Assertions.*;

import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CancellationException;
import java.util.concurrent.TimeoutException;
import org.junit.jupiter.api.Test;

class SearchTest {
  @Test
  void regexStreamReadsAllOccurrencesAcrossEmailsAndRetainsNonmatches() throws Exception {
    try (var inbox = Ahrara.createInbox(Relay.options())) {
      Relay.send(inbox.address(), "unmatched", "ordinary text");
      Relay.send(inbox.address(), "one", "CODE:111 CODE:222-YES");
      Relay.send(inbox.address(), "two", "CODE:333");
      var query =
          new RegexQuery(
              "code:(?P<digits>\\d+)(?:-(?P<suffix>[A-Z]+))?", "text", true, false, false);
      List<RegexMatch> matches;
      try (var stream = inbox.waitForRegexMatches(query, Duration.ofMillis(400), null)) {
        matches = stream.toList();
      }

      assertEquals(
          List.of("CODE:111", "CODE:222-YES", "CODE:333"),
          matches.stream().map(RegexMatch::value).toList());
      assertEquals(2, matches.stream().map(RegexMatch::emailId).distinct().count());
      assertEquals("CODE:111", matches.get(0).groups().get(0).value());
      assertEquals(
          "111",
          matches.get(0).groups().stream()
              .filter(group -> "digits".equals(group.name()))
              .findFirst()
              .orElseThrow()
              .value());
      assertNull(
          matches.get(0).groups().stream()
              .filter(group -> "suffix".equals(group.name()))
              .findFirst()
              .orElseThrow()
              .value());
      assertEquals("unmatched", inbox.waitForEmail(Duration.ofSeconds(3)).subject());
    }
  }

  @Test
  void xpathStreamsAcrossEmailsAndWaitsForOneValue() throws Exception {
    try (var inbox = Ahrara.createInbox(Relay.options())) {
      Relay.send(
          inbox.address(), "one", "text", "<a href='/one'>One</a><a href='/two'>Two</a>", null);
      Relay.send(inbox.address(), "two", "text", "<a href='/three'>Three</a>", null);
      try (var stream = inbox.waitForXPathValues("//a/@href", Duration.ofMillis(400), null)) {
        assertEquals(List.of("/one", "/two", "/three"), stream.toList());
      }
      Relay.send(inbox.address(), "single", "text", "<p><b id='token'>Olá 日本語</b>", null);

      assertEquals(
          "Olá 日本語", inbox.waitForXPath("//b[@id='token']/text()", Duration.ofSeconds(3), null));
    }
  }

  @Test
  void globalAndOperationFiltersComposeWithoutDiscardingOperationNonmatches() throws Exception {
    var global = EmailFilter.builder().sender("SDK@EXAMPLE.TEST").subject("accept").build();
    try (var inbox = Ahrara.createInbox(Relay.options().withFilter(global))) {
      Relay.send(inbox.address(), "discard", "never stored");
      Relay.send(inbox.address(), "accept later", "retained");
      Relay.send(
          inbox.address(),
          "accept now",
          "code 123",
          null,
          new byte[] {1},
          Map.of("X-Test-Case", "expected-value"));
      var filter =
          EmailFilter.builder()
              .subject("now")
              .to(inbox.address())
              .recipient(inbox.address())
              .envelopeSender("sdk@example.test")
              .text("CODE")
              .hasAttachments(true)
              .headers(Map.of("x-test-case", "EXPECTED"))
              .build();

      assertEquals("accept now", inbox.waitForEmail(Duration.ofSeconds(3), filter).subject());
      assertEquals("accept later", inbox.waitForEmail(Duration.ofSeconds(3)).subject());
      assertEquals(2, inbox.listEmails().emails().size());
    }
  }

  @Test
  void optionsErrorsAndInterruptionKeepInboxUsable() throws Exception {
    try (var inbox = Ahrara.createInbox(Relay.options())) {
      Relay.send(inbox.address(), "Confirm 456", "body");
      var query = new RegexQuery("confirm (?P<code>\\d+)", "subject", true, false, false);
      var match =
          inbox.waitForRegexMatch(
              query, Duration.ofSeconds(3), EmailFilter.builder().subject("confirm").build());

      assertEquals(
          "456",
          match.groups().stream()
              .filter(group -> "code".equals(group.name()))
              .findFirst()
              .orElseThrow()
              .value());
      assertThrows(
          AhraraException.class, () -> inbox.waitForRegexMatch("[", Duration.ofSeconds(1), null));
      assertThrows(
          AhraraException.class, () -> inbox.waitForXPath("//*[", Duration.ofSeconds(1), null));
      assertThrows(
          TimeoutException.class,
          () -> inbox.waitForRegexMatch("none", Duration.ofMillis(100), null));
      try {
        Thread.currentThread().interrupt();
        try (var stream = inbox.waitForXPathValues("//a/@href")) {
          assertThrows(CancellationException.class, stream::findFirst);
          assertTrue(Thread.currentThread().isInterrupted());
        }
      } finally {
        Thread.interrupted();
      }

      Relay.send(inbox.address(), "after cancellation", "ready 789");

      assertEquals("789", inbox.waitForRegexMatch("\\d+", Duration.ofSeconds(3), null).value());
    }
  }

  @Test
  void totalDurationIncludesConsumerTimeAndEarlyStopLeavesLaterEmails() throws Exception {
    try (var inbox = Ahrara.createInbox(Relay.options())) {
      Relay.send(inbox.address(), "first", "1 2 3");
      var matches = new ArrayList<String>();
      try (var stream = inbox.waitForRegexMatches("\\d", Duration.ofMillis(100), null)) {
        stream.forEach(
            match -> {
              matches.add(match.value());
              assertDoesNotThrow(() -> Thread.sleep(180));
            });
      }

      assertEquals(1, matches.size());
      Relay.send(inbox.address(), "second", "4");
      Relay.send(inbox.address(), "third", "5");
      try (var stream = inbox.waitForRegexMatches("\\d")) {
        assertTrue(stream.findFirst().isPresent());
      }

      assertEquals("5", inbox.waitForRegexMatch("\\d+", Duration.ofSeconds(3), null).value());
    }
  }
}
