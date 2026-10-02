package dev.ahrara;

import com.fasterxml.jackson.databind.JsonNode;
import java.util.List;
import java.util.stream.StreamSupport;

/**
 * One page of stored email metadata.
 *
 * @param emails metadata in ascending identifier order
 * @param nextCursor exclusive cursor for another page, or null when complete
 */
public record EmailPage(List<EmailSummary> emails, Long nextCursor) {
  static EmailPage fromNative(JsonNode value) {
    return new EmailPage(
        StreamSupport.stream(value.path("emails").spliterator(), false)
            .map(EmailSummary::fromNative)
            .toList(),
        value.path("next_cursor").isNumber() ? value.path("next_cursor").asLong() : null);
  }
}
