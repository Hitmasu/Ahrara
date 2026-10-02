package dev.ahrara;

import static org.junit.jupiter.api.Assertions.*;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.concurrent.TimeoutException;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class PersistenceTest {
  @TempDir Path directory;

  @Test
  void persistentProfileRetainsIdentityHistoryAndAttachments() throws Exception {
    var options = Relay.options().withProfilePath(directory.resolve("profile").toString());
    byte[] expected = new byte[] {0, 1, 2, 127, (byte) 255};
    InboxCredentials credentials;
    long id;
    try (var inbox = Ahrara.createInbox(options)) {
      credentials = inbox.exportCredentials();
      assertFalse(
          credentials.toString().contains(credentials.recoveryKey()),
          "Credentials must redact the key");
      assertFalse(
          options
              .withIdentity(credentials.recoveryKey(), null)
              .toString()
              .contains(credentials.recoveryKey()),
          "Options must redact the key");
      Relay.send(
          inbox.address(),
          "persistent attachment",
          "stored body",
          "<p>stored html</p><script>alert(1)</script>",
          expected);
      id = inbox.waitForEmail(Duration.ofSeconds(5)).id();
    }
    try (var reopened = Ahrara.createInbox(options)) {
      assertTrue(credentials.address().equals(reopened.address()), "The address must persist");
      var page = reopened.listEmails(0, 1, null, null);

      assertEquals(1, page.emails().size());
      assertEquals(id, page.emails().get(0).id());
      var email = reopened.getEmail(id);

      assertTrue(email.textBody().contains("stored body"));
      assertTrue(
          email.recipient().equals(reopened.address()),
          "Stored recipient must belong to this inbox");
      assertTrue(email.sizeBytes() > 0);
      assertFalse(email.receivedAt().isBlank());
      assertFalse(email.parseError());
      assertEquals(1, email.attachments().size());
      String attachmentId = email.attachments().get(0).attachmentId();

      assertArrayEquals(expected, reopened.downloadAttachment(id, attachmentId).content());
      Path saved = reopened.saveAttachment(id, attachmentId, directory);

      assertArrayEquals(expected, Files.readAllBytes(saved));
      assertThrows(AhraraException.class, () -> reopened.saveAttachment(id, attachmentId, saved));
      assertTrue(
          new String(reopened.getRawEmail(id), StandardCharsets.UTF_8)
              .contains("persistent attachment"));
      assertFalse(reopened.getHtml(id, false).html().contains("<script"));
      assertThrows(TimeoutException.class, () -> reopened.waitForEmail(Duration.ofMillis(120)));
      assertEquals(1, reopened.deleteEmail(id));
      assertTrue(reopened.listEmails().emails().isEmpty());
      Relay.send(reopened.address(), "to clear", "clear");
      reopened.waitForEmail(Duration.ofSeconds(3));

      assertEquals(1, reopened.clearEmails());
    }
    try (var imported =
        Ahrara.createInbox(
            Relay.options().withIdentity(credentials.recoveryKey(), credentials.address()))) {
      assertTrue(
          credentials.address().equals(imported.address()),
          "Identity import must preserve the address");
      assertTrue(imported.listEmails().emails().isEmpty());
      Relay.send(imported.address(), "imported identity", "received");

      assertEquals("imported identity", imported.waitForEmail(Duration.ofSeconds(3)).subject());
    }
  }

  @Test
  void exportedCredentialsReconnectAndRejectUnrelatedAddress() throws Exception {
    InboxCredentials credentials;
    String otherAddress;
    try (var inbox = Ahrara.createInbox(Relay.options())) {
      credentials = inbox.exportCredentials();
    }
    try (var other = Ahrara.createInbox(Relay.options())) {
      otherAddress = other.address();
    }
    var wrong = new InboxCredentials(otherAddress, credentials.recoveryKey());

    assertThrows(
        AhraraException.class, () -> Ahrara.createInbox(Relay.options().withCredentials(wrong)));
    try (var restored = Ahrara.createInbox(Relay.options().withCredentials(credentials))) {
      assertTrue(
          credentials.address().equals(restored.address()), "Credentials must restore the address");
      Relay.send(restored.address(), "credentials restored", "received");

      assertEquals("credentials restored", restored.waitForEmail(Duration.ofSeconds(3)).subject());
    }
  }
}
