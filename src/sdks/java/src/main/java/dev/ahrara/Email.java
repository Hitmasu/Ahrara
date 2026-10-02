package dev.ahrara;

import com.fasterxml.jackson.databind.JsonNode;
import java.util.List;
import java.util.stream.StreamSupport;

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
  static Email fromNative(JsonNode node) {
    return new Email(
        node.path("id").asLong(),
        node.path("subject").asText(""),
        node.path("from").asText(""),
        node.path("to").asText(""),
        node.path("text_body").asText(""),
        node.path("text_body_truncated").asBoolean(),
        node.path("html_available").asBoolean(),
        StreamSupport.stream(node.path("attachments").spliterator(), false)
            .map(Attachment::fromNative)
            .toList(),
        node.path("transfer_id").asText(),
        node.path("identity_fingerprint").asText(),
        node.path("recipient").asText(),
        node.path("envelope_sender").asText(),
        node.path("received_at").asText(),
        node.path("size_bytes").asLong(),
        node.path("message_id").asText(null),
        node.path("cc").asText(),
        node.path("date").asText(),
        node.path("forwarded_to").asText(null),
        node.path("parse_error").asBoolean());
  }
}
