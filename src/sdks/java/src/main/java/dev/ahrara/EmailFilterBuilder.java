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

  /** Creates an empty filter builder. */
  public EmailFilterBuilder() {}

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
   * Sets exact smtp recipient.
   *
   * @param value the condition, or null to omit
   * @return this builder
   */
  public EmailFilterBuilder recipient(String value) {
    recipient = value;
    return this;
  }

  /**
   * Sets exact smtp sender.
   *
   * @param value the condition, or null to omit
   * @return this builder
   */
  public EmailFilterBuilder envelopeSender(String value) {
    envelopeSender = value;
    return this;
  }

  /**
   * Sets exact message-id header.
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
        headers);
  }
}
