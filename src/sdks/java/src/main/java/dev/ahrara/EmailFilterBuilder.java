package dev.ahrara;

import java.util.Map;

/** Builds immutable email filters without requiring positional optional arguments. */
public final class EmailFilterBuilder {
  private String sender;
  private String subject;
  private String to;
  private String cc;
  private String recipient;
  private String envelopeSender;
  private String messageId;
  private String text;
  private Boolean hasAttachments;
  private Map<String, String> headers;
  private boolean regex;

  /** Creates an empty filter builder. */
  public EmailFilterBuilder() {}

  /**
   * Interprets string conditions as case-insensitive Rust regex. Header names stay literal;
   * recipient, envelope sender and Message-ID patterns must match the whole value.
   *
   * @param value true for regex patterns, false for literal conditions
   * @return this builder
   */
  public EmailFilterBuilder regex(boolean value) {
    regex = value;
    return this;
  }

  /**
   * Sets case-insensitive sender substring.
   *
   * @param value the condition, or null to omit
   * @return this builder
   */
  public EmailFilterBuilder sender(String value) {
    sender = value;
    return this;
  }

  /**
   * Sets case-insensitive subject substring.
   *
   * @param value the condition, or null to omit
   * @return this builder
   */
  public EmailFilterBuilder subject(String value) {
    subject = value;
    return this;
  }

  /**
   * Sets case-insensitive to header substring.
   *
   * @param value the condition, or null to omit
   * @return this builder
   */
  public EmailFilterBuilder to(String value) {
    to = value;
    return this;
  }

  /**
   * Sets case-insensitive cc header substring.
   *
   * @param value the condition, or null to omit
   * @return this builder
   */
  public EmailFilterBuilder cc(String value) {
    cc = value;
    return this;
  }

  /**
   * Sets the whole SMTP recipient, ignoring case; an anchored pattern when regex is enabled.
   *
   * @param value the condition, or null to omit
   * @return this builder
   */
  public EmailFilterBuilder recipient(String value) {
    recipient = value;
    return this;
  }

  /**
   * Sets the whole SMTP sender, ignoring case; an anchored pattern when regex is enabled.
   *
   * @param value the condition, or null to omit
   * @return this builder
   */
  public EmailFilterBuilder envelopeSender(String value) {
    envelopeSender = value;
    return this;
  }

  /**
   * Sets the whole Message-ID header, ignoring case; an anchored pattern when regex is enabled.
   *
   * @param value the condition, or null to omit
   * @return this builder
   */
  public EmailFilterBuilder messageId(String value) {
    messageId = value;
    return this;
  }

  /**
   * Sets case-insensitive text body substring.
   *
   * @param value the condition, or null to omit
   * @return this builder
   */
  public EmailFilterBuilder text(String value) {
    text = value;
    return this;
  }

  /**
   * Sets optional attachment-presence condition.
   *
   * @param value the condition, or null to omit
   * @return this builder
   */
  public EmailFilterBuilder hasAttachments(Boolean value) {
    hasAttachments = value;
    return this;
  }

  /**
   * Sets header names and value substring conditions.
   *
   * @param value the condition, or null to omit
   * @return this builder
   */
  public EmailFilterBuilder headers(Map<String, String> value) {
    headers = value;
    return this;
  }

  /**
   * @return a new immutable filter
   */
  public EmailFilter build() {
    return new EmailFilter(
        sender,
        subject,
        to,
        cc,
        recipient,
        envelopeSender,
        messageId,
        text,
        hasAttachments,
        headers,
        regex);
  }
}
