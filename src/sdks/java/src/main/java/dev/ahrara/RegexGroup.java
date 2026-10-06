package dev.ahrara;

/**
 * One captured group, including group zero for the complete match.
 *
 * @param index zero-based group index
 * @param name group name, or null for unnamed groups
 * @param value captured text, or null when an optional group did not participate
 */
public record RegexGroup(int index, String name, String value) {}
