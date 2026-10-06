package dev.ahrara;

import java.util.List;

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
}
