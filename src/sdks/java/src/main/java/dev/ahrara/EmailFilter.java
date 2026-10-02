package dev.ahrara;

import java.util.LinkedHashMap;
import java.util.Map;

/**
 * AND-combined email conditions. Inbox filters discard rejected mail before storage; operation
 * filters leave nonmatching stored messages available.
 *
 * @param sender case-insensitive sender substring, or null to omit
 * @param subject case-insensitive subject substring, or null to omit
 * @param to case-insensitive to header substring, or null to omit
 * @param cc case-insensitive cc header substring, or null to omit
 * @param recipient exact, case-insensitive SMTP recipient, or null to omit
 * @param envelopeSender exact, case-insensitive SMTP sender, or null to omit
 * @param messageId exact, case-insensitive Message-ID header, or null to omit
 * @param text case-insensitive text body substring, or null to omit
 * @param hasAttachments optional attachment-presence condition, or null to omit
 * @param headers header names and value substring conditions, or null to omit
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
    Map<String, String> headers) {
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

  Map<String, Object> toNative() {
    var result = new LinkedHashMap<String, Object>();
    if (sender != null) result.put("sender", sender);
    if (subject != null) result.put("subject", subject);
    if (to != null) result.put("to", to);
    if (cc != null) result.put("cc", cc);
    if (recipient != null) result.put("recipient", recipient);
    if (envelopeSender != null) result.put("envelope_sender", envelopeSender);
    if (messageId != null) result.put("message_id", messageId);
    if (text != null) result.put("text", text);
    if (hasAttachments != null) result.put("has_attachments", hasAttachments);
    if (headers != null) result.put("headers", headers);

    return result;
  }
}
