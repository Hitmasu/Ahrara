package dev.ahrara;

import static org.junit.jupiter.api.Assertions.*;

import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CancellationException;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import org.junit.jupiter.api.Test;

class ReceivingTest {
  @Test
  void lazyStreamPreservesMailAfterEarlyTermination() throws Exception {
    try (var inbox = Ahrara.createInbox(Relay.options())) {
      Relay.send(inbox.address(), "first", "one");
      Relay.send(inbox.address(), "second", "two");
      Relay.send(inbox.address(), "third", "three");
      try (var emails = inbox.emails()) {
        assertEquals(List.of("first", "second"), emails.limit(2).map(Email::subject).toList());
      }

      assertEquals("third", inbox.waitForEmail(Duration.ofSeconds(3)).subject());
    }
  }

  @Test
  void finiteCallbackReceivesSequentiallyAndEndsNormally() throws Exception {
    try (var inbox = Ahrara.createInbox(Relay.options())) {
      Relay.send(inbox.address(), "one", "one");
      Relay.send(inbox.address(), "two", "two");
      var received = new ArrayList<String>();
      Thread caller = Thread.currentThread();
      inbox.receiveEmails(
          email -> {
            assertSame(caller, Thread.currentThread());
            assertDoesNotThrow(() -> Thread.sleep(30));
            received.add(email.subject());
          },
          Duration.ofMillis(250),
          null);

      assertEquals(List.of("one", "two"), received);
      try (var empty = inbox.emails(Duration.ofMillis(120))) {
        assertEquals(0, empty.count());
      }
    }
  }

  @Test
  void handlerFailureDoesNotCloseOrPrefetch() throws Exception {
    try (var inbox = Ahrara.createInbox(Relay.options())) {
      Relay.send(inbox.address(), "failure", "one");
      Relay.send(inbox.address(), "next", "two");
      var expected = new IllegalStateException("synthetic callback failure");

      assertSame(
          expected,
          assertThrows(
              IllegalStateException.class,
              () ->
                  inbox.receiveEmails(
                      email -> {
                        throw expected;
                      })));
      assertEquals("next", inbox.waitForEmail(Duration.ofSeconds(3)).subject());
    }
  }

  @Test
  void callbackInterruptionLeavesFollowingMailAvailable() throws Exception {
    try (var inbox = Ahrara.createInbox(Relay.options())) {
      Relay.send(inbox.address(), "cancel handler", "one");
      Relay.send(inbox.address(), "after handler", "two");
      try {
        assertThrows(
            CancellationException.class,
            () ->
                inbox.receiveEmails(
                    email -> {
                      assertDoesNotThrow(() -> Thread.sleep(200));
                      Thread.currentThread().interrupt();
                    },
                    Duration.ofMillis(100),
                    null));
        assertTrue(Thread.currentThread().isInterrupted());
      } finally {
        Thread.interrupted();
      }

      assertEquals("after handler", inbox.waitForEmail(Duration.ofSeconds(3)).subject());
    }
  }

  @Test
  void interruptionCancelsStreamAndPreservesFlagWithoutClosingInbox() throws Exception {
    try (var inbox = Ahrara.createInbox(Relay.options())) {
      var failure = new CompletableFuture<Throwable>();
      var interruptPreserved = new AtomicBoolean();
      var receiver =
          new Thread(
              () -> {
                try (var stream = inbox.emails()) {
                  stream.forEach(email -> fail("No message expected"));
                } catch (Throwable error) {
                  interruptPreserved.set(Thread.currentThread().isInterrupted());
                  failure.complete(error);
                }
              });
      receiver.start();
      Thread.sleep(150);
      receiver.interrupt();

      assertInstanceOf(CancellationException.class, failure.get(2, TimeUnit.SECONDS));
      assertTrue(interruptPreserved.get());
      receiver.join(2000);
      Relay.send(inbox.address(), "after interruption", "usable");

      assertEquals("after interruption", inbox.waitForEmail(Duration.ofSeconds(3)).subject());
    }
  }
}
