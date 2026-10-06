package dev.ahrara;

import java.util.List;

/**
 * A parsed email.
 *
 * @param id local storage identifier
 * @param subject decoded subject
 * @param from decoded sender header
 * @param to decoded recipient header
 * @param textBody text body limited by the native response budget
 * @param textBodyTruncated whether the full body exceeds that budget
 * @param htmlAvailable whether the original message contains HTML
 * @param attachments attachment metadata, with content available through the inbox
 * @param transferId relay transfer identifier
 * @param identityFingerprint public fingerprint of the receiving identity
 * @param recipient SMTP envelope recipient
 * @param envelopeSender SMTP envelope sender
 * @param receivedAt local receipt timestamp
 * @param sizeBytes original message size
 * @param messageId Message-ID header, or null when absent
 * @param cc decoded Cc header
 * @param date Date header supplied by the message
 * @param forwardedTo original forwarded recipient, or null when absent
 * @param parseError whether parsing the original message encountered an error
 */
public record Email(
    long id,
    String subject,
    String from,
    String to,
    String textBody,
    boolean textBodyTruncated,
    boolean htmlAvailable,
    List<Attachment> attachments,
    String transferId,
    String identityFingerprint,
    String recipient,
    String envelopeSender,
    String receivedAt,
    long sizeBytes,
    String messageId,
    String cc,
    String date,
    String forwardedTo,
    boolean parseError) {
  /** Copies attachment metadata to keep the email immutable. */
  public Email {
    attachments = attachments == null ? null : List.copyOf(attachments);
  }
}
