package dev.ahrara;

/**
 * Sanitized HTML and detected remote resources.
 *
 * @param html sanitized HTML content
 * @param remoteResources number of detected remote references
 */
public record EmailHtml(String html, int remoteResources) {}
