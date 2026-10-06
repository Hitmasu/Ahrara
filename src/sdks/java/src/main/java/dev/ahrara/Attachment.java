package dev.ahrara;

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
    String contentDisposition) {}
