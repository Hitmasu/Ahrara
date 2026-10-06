package dev.ahrara;

import java.util.Map;

/**
 * AND-combined email conditions. Inbox filters discard rejected mail before storage; operation
 * filters leave nonmatching stored messages available. String conditions use literal matching
 * unless regex is enabled; then recipient, envelopeSender and messageId patterns must match the
 * whole value, while other patterns match anywhere.
 *
 * @param sender case-insensitive sender substring, or null to omit
 * @param subject case-insensitive subject substring, or null to omit
 * @param to case-insensitive to header substring, or null to omit
 * @param cc case-insensitive cc header substring, or null to omit
 * @param recipient whole, case-insensitive SMTP recipient (anchored pattern with regex), or null
 * @param envelopeSender whole, case-insensitive SMTP sender (anchored pattern with regex), or null
 * @param messageId whole, case-insensitive Message-ID header (anchored pattern with regex), or null
 * @param text case-insensitive text body substring, or null to omit
 * @param hasAttachments optional attachment-presence condition, or null to omit
 * @param headers header names and value substring conditions, or null to omit
 * @param regex interpret string conditions as case-insensitive Rust regex; header names stay
 *     literal and {@code (?-i)} restores case sensitivity
 */
public record EmailFilter(
    String sender,
    String subject,
    String to,
    String cc,
    String recipient,
    String envelopeSender,
    String messageId,
    String text,
    Boolean hasAttachments,
    Map<String, String> headers,
    boolean regex) {
  /** Copies header conditions to keep the filter immutable. */
  public EmailFilter {
    headers = headers == null ? null : Map.copyOf(headers);
  }

  /**
   * @return a builder for specifying only the desired conditions
   */
  public static EmailFilterBuilder builder() {
    return new EmailFilterBuilder();
  }
}
