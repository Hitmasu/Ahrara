package dev.ahrara;

/**
 * Metadata for a stored email. Read the full email to obtain its body and attachments.
 *
 * @param id local storage identifier
 * @param subject decoded subject
 * @param from sender header
 * @param to recipient header
 * @param receivedAt local receipt timestamp
 * @param sizeBytes original message size
 * @param transferId relay transfer identifier
 * @param identityFingerprint public fingerprint of the receiving identity
 * @param recipient SMTP envelope recipient
 * @param envelopeSender SMTP envelope sender
 * @param messageId Message-ID header, or null when absent
 */
public record EmailSummary(
    long id,
    String subject,
    String from,
    String to,
    String receivedAt,
    long sizeBytes,
    String transferId,
    String identityFingerprint,
    String recipient,
    String envelopeSender,
    String messageId) {}
