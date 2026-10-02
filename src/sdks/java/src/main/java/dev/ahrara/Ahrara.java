package dev.ahrara;

import java.util.Map;

/** Creates inboxes using the embedded Rust core, without a CLI or local API process. */
public final class Ahrara {
  private Ahrara() {}

  /**
   * Creates a new temporary inbox using native relay defaults.
   *
   * @return a connected inbox; close it with try-with-resources
   * @throws AhraraException if configuration or connection fails
   */
  public static Inbox createInbox() {
    return createInbox(InboxOptions.defaults());
  }

  /**
   * Opens an inbox and waits until it is ready to receive mail.
   *
   * @param options relay, identity, and optional persistent profile settings
   * @return a connected inbox; closing preserves a configured persistent profile
   * @throws AhraraException if configuration, identity validation, or connection fails
   * @throws NullPointerException if options is null
   */
  public static Inbox createInbox(InboxOptions options) {
    var result = NativeCore.call(Map.of("op", "create", "options", options.toNative()));
    return new Inbox(result.path("handle").asLong(), result.path("address").asText());
  }
}
