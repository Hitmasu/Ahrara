package dev.ahrara;

import static org.junit.jupiter.api.Assertions.*;

import java.time.Duration;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import org.junit.jupiter.api.Test;

class InboxTest {
  @Test
  void buffersUnicodeMailAndConsumesOnlyOnce() throws Exception {
    try (var inbox = Ahrara.createInbox(Relay.options())) {
      Relay.send(inbox.address(), "Confirmação 日本語", "Verification code: 123456");
      var email =
          inbox.waitForEmail(Duration.ofSeconds(10), EmailFilter.builder().subject("日本語").build());

      assertEquals("Confirmação 日本語", email.subject());
      assertTrue(email.textBody().contains("123456"));
      assertTrue(email.from().contains("sdk@example.test"));
      assertThrows(TimeoutException.class, () -> inbox.waitForEmail(Duration.ofMillis(150)));
    }
  }

  @Test
  void isolatesInboxesAndPreservesUnmatchedMessages() throws Exception {
    try (var first = Ahrara.createInbox(Relay.options());
        var second = Ahrara.createInbox(Relay.options())) {
      assertTrue(!first.address().equals(second.address()), "Inboxes must be isolated.");
      Relay.send(first.address(), "Earlier", "first");
      Relay.send(second.address(), "Second inbox", "second");
      Relay.send(first.address(), "Verification", "filtered");

      assertEquals(
          "Verification",
          first
              .waitForEmail(
                  Duration.ofSeconds(10), EmailFilter.builder().subject("Verification").build())
              .subject());
      assertEquals("Earlier", first.waitForEmail().subject());
      assertEquals("Second inbox", second.waitForEmail().subject());
    }
  }

  @Test
  void interruptionLeavesInboxUsable() throws Exception {
    try (var inbox = Ahrara.createInbox(Relay.options())) {
      var failure = new CompletableFuture<Throwable>();
      var waiter =
          new Thread(
              () -> {
                try {
                  inbox.waitForEmail();
                  failure.complete(null);
                } catch (Throwable error) {
                  failure.complete(error);
                }
              });
      waiter.start();
      Thread.sleep(150);
      waiter.interrupt();

      assertInstanceOf(InterruptedException.class, failure.get(2, TimeUnit.SECONDS));
      waiter.join(2000);
      Relay.send(inbox.address(), "After interruption", "still connected");

      assertEquals("After interruption", inbox.waitForEmail().subject());
    }
  }

  @Test
  void closingInterruptsWaitAndIsIdempotent() throws Exception {
    var inbox = Ahrara.createInbox(Relay.options());
    var failure =
        CompletableFuture.supplyAsync(
            () -> {
              try {
                inbox.waitForEmail(Duration.ofSeconds(20));
                return null;
              } catch (Exception error) {
                return error;
              }
            });
    Thread.sleep(50);
    inbox.close();
    inbox.close();
    var error = failure.get(2, TimeUnit.SECONDS);

    assertTrue(error instanceof IllegalStateException || error instanceof AhraraException);
    assertThrows(IllegalStateException.class, inbox::waitForEmail);
  }
}
