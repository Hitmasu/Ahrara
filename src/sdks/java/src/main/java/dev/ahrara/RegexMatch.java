package dev.ahrara;

import com.fasterxml.jackson.databind.JsonNode;
import java.util.List;
import java.util.stream.StreamSupport;

/**
 * A regex match with its source email identifier and captured groups.
 *
 * @param emailId local history identifier
 * @param value complete matching text
 * @param groups all groups, including group zero and unmatched optional groups
 */
public record RegexMatch(long emailId, String value, List<RegexGroup> groups) {
  /** Copies captured groups to keep the result immutable. */
  public RegexMatch {
    groups = List.copyOf(groups);
  }

  static RegexMatch fromNative(JsonNode value) {
    return new RegexMatch(
        value.path("email_id").asLong(),
        value.path("value").asText(),
        StreamSupport.stream(value.path("groups").spliterator(), false)
            .map(RegexGroup::fromNative)
            .toList());
  }
}
