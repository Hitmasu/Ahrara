package dev.ahrara;

import java.util.List;

/**
 * One page of stored email metadata.
 *
 * @param emails metadata in ascending identifier order
 * @param nextCursor exclusive cursor for another page, or null when complete
 */
public record EmailPage(List<EmailSummary> emails, Long nextCursor) {
  /** Copies summaries to keep the page immutable. */
  public EmailPage {
    emails = emails == null ? null : List.copyOf(emails);
  }
}
