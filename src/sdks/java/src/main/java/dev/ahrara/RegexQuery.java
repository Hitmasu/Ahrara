package dev.ahrara;

import java.util.Map;
import java.util.Objects;

/**
 * A regex evaluated by the shared Rust engine, without look-around or backreferences.
 *
 * @param pattern regex expression
 * @param source text, html, subject, or raw
 * @param ignoreCase whether matching ignores case; unlike filter regex, queries are case-sensitive
 *     by default
 * @param multiLine whether anchors match individual lines
 * @param dotMatchesNewLine whether a dot also matches newline characters
 */
public record RegexQuery(
    String pattern,
    String source,
    boolean ignoreCase,
    boolean multiLine,
    boolean dotMatchesNewLine) {
  /** Validates required values. */
  public RegexQuery {
    Objects.requireNonNull(pattern, "pattern");
    Objects.requireNonNull(source, "source");
  }

  /**
   * Creates a case-sensitive query over plain text.
   *
   * @param pattern regex expression
   */
  public RegexQuery(String pattern) {
    this(pattern, "text", false, false, false);
  }

  Map<String, Object> toNative(boolean all) {
    var query = NativeCore.encode(this);
    query.put("kind", "regex");
    query.put("all", all);
    return query;
  }
}
