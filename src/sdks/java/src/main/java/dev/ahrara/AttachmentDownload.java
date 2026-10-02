package dev.ahrara;

/**
 * Attachment metadata and decoded bytes.
 *
 * @param attachment metadata identifying the content
 * @param content decoded attachment bytes owned by the caller
 */
public record AttachmentDownload(Attachment attachment, byte[] content) {}
