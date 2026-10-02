package dev.ahrara;

import com.fasterxml.jackson.databind.JsonNode;

/**
 * Describes an attachment without loading its content.
 *
 * @param attachmentId identifier within the email
 * @param filename filename supplied by the message
 * @param contentType declared MIME type
 * @param size decoded content size in bytes
 * @param contentDisposition MIME content disposition
 */
public record Attachment(
    String attachmentId,
    String filename,
    String contentType,
    long size,
    String contentDisposition) {
  static Attachment fromNative(JsonNode value) {
    return new Attachment(
        value.path("attachment_id").asText(),
        value.path("filename").asText(""),
        value.path("content_type").asText(),
        value.path("size").asLong(),
        value.path("content_disposition").asText());
  }
}
