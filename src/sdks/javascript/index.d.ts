/** An address and its sensitive Recovery Key; never log the key. */
export interface CredentialsInput {
  address: string;
  recoveryKey: string;
}

/** Exported credentials redact inspection and JSON; access recoveryKey explicitly. */
export class InboxCredentials implements CredentialsInput {
  constructor(address: string, recoveryKey: string);
  readonly address: string;
  readonly recoveryKey: string;
  toJSON(): { address: string; recoveryKey: string };
}

/** Connection, local persistence, and optional identity restoration. */
export interface InboxOptions {
  /** Receipt filter applied before storage. Rejected emails are dropped. */
  filter?: EmailFilter;
  /** wss:// relay URL; defaults to the release core's production endpoint. */
  serverUrl?: string;
  /** Independently verified public Noise key, not a Recovery Key. */
  serverPublicKey?: string;
  /** Optional custom trusted CA PEM file. TLS verification remains enabled. */
  tlsCaPath?: string;
  /** Connection deadline in milliseconds, 1..300000; core default 20000. */
  connectTimeoutMs?: number;
  /** Absolute persistent profile directory. Omit for an isolated temporary profile. */
  profilePath?: string;
  /** Restore exported credentials; mutually exclusive with recoveryKey/address. */
  credentials?: CredentialsInput;
  /** Sensitive identity import, optionally paired with an owned address. */
  recoveryKey?: string;
  /** Owned address to select; alone requires an existing profile identity. */
  address?: string;
}

export interface WaitOptions {
  /** Additional filter; nonmatching stored emails remain available for later waits. */
  filter?: EmailFilter;
  /** Positive integer milliseconds. Omitted means indefinite; streams use a total deadline. */
  timeoutMs?: number;
  /** Substring filter using the core's ASCII case-insensitive matching. */
  subject?: string;
  /** Sender substring filter using the same matching rules. */
  from?: string;
  /** Abort only this wait/subscription. Cancellation leaves the inbox open. */
  signal?: AbortSignal;
}

/** Fields are ANDed. Text fields match case-insensitive substrings unless stated otherwise. */
export interface EmailFilter {
  sender?: string;
  subject?: string;
  to?: string;
  cc?: string;
  text?: string;
  /** Exact case-insensitive SMTP recipient. */
  recipient?: string;
  /** Exact case-insensitive SMTP envelope sender. */
  envelopeSender?: string;
  /** Exact case-insensitive Message-ID header. */
  messageId?: string;
  hasAttachments?: boolean;
  /** Header names are case-insensitive; each value must occur as a substring. */
  headers?: Record<string, string>;
}

/** Shared search lifecycle. Single waits throw on timeout; streams end normally. */
export interface SearchOptions {
  /** Positive integer milliseconds, or omitted for indefinite. Streams use total duration. */
  timeoutMs?: number;
  /** Abort this search/stream while keeping the inbox open. */
  signal?: AbortSignal;
  /** Nonmatching stored emails remain unconsumed. */
  filter?: EmailFilter;
}

export interface RegexOptions extends SearchOptions {
  /** Search full decoded text (default), original HTML, subject, or raw RFC 822 text. */
  source?: "text" | "html" | "subject" | "raw";
  ignoreCase?: boolean;
  multiLine?: boolean;
  dotMatchesNewLine?: boolean;
}

export interface RegexGroup {
  /** Zero identifies the whole match. Remaining indices follow capture order. */
  index: number;
  /** Named capture identifier, or null for unnamed groups. */
  name?: string | null;
  /** Null when an optional group did not participate in this match. */
  value?: string | null;
}

/** A matching substring and all its captures, associated with an inbox-owned email. */
export interface RegexMatch {
  email_id: number;
  value: string;
  groups: RegexGroup[];
}

export interface ListOptions {
  /** Exclusive message-ID cursor; defaults to 0. */
  afterId?: number;
  /** Page size 1..100; defaults to 20. */
  limit?: number;
  subject?: string;
  from?: string;
}

/** Stored metadata. Field names match the shared native email schema. */
export interface EmailMetadata {
  id: number;
  transfer_id: string;
  identity_fingerprint: string;
  recipient: string;
  envelope_sender: string;
  received_at: string;
  from: string;
  to: string;
  subject: string;
  message_id: string | null;
  size_bytes: number;
}

export interface Attachment {
  attachment_id: string;
  filename: string;
  content_type: string;
  size: number;
  content_disposition: string;
}

export interface Email extends EmailMetadata {
  cc: string;
  date: string;
  forwarded_to: string | null;
  /** Extracted text, capped by the core at 256 KiB. */
  text_body: string;
  text_body_truncated: boolean;
  html_available: boolean;
  attachments: Attachment[];
  parse_error: boolean;
}

export interface EmailPage {
  emails: EmailMetadata[];
  next_cursor: number | null;
}

export interface AttachmentData {
  attachment: Attachment;
  content: Buffer;
}

export interface HtmlEmail {
  html: string;
  remote_resources: number;
}

/** Native errors retain a stable code, including timeout/inbox_closed/wait_in_progress. */
export class AhraraError extends Error {
  constructor(code: string, message: string);
  readonly code: string;
}

/** A ready, caller-owned inbox. Close explicitly or use await using. */
export class Inbox {
  private constructor();
  readonly address: string;
  /** Consume one matching email. Omitted timeout waits indefinitely. Throws on finite timeout. */
  waitForEmail(options?: WaitOptions): Promise<Email>;
  /** Sequential stream; total timeout includes consumer work and expires normally. */
  waitForEmails(options?: WaitOptions): AsyncGenerator<Email, void, unknown>;
  /** Await each callback; propagate callback errors. No prefetch or hidden background queue. */
  receiveEmails(
    handler: (email: Email) => void | Promise<void>,
    options?: WaitOptions,
  ): Promise<void>;
  /** First regex match with captures. Invalid queries never consume email. */
  waitForRegexMatch(
    pattern: string,
    options?: RegexOptions,
  ): Promise<RegexMatch>;
  /** Every match across emails; total deadline includes consumer work. Expiry ends normally. */
  waitForRegexMatches(
    pattern: string,
    options?: RegexOptions,
  ): AsyncGenerator<RegexMatch, void, unknown>;
  /** First XPath string value from a matching email's HTML. */
  waitForXPath(expression: string, options?: SearchOptions): Promise<string>;
  /** Every XPath value across emails; same lifetime and deadline rules as regex streams. */
  waitForXPathValues(
    expression: string,
    options?: SearchOptions,
  ): AsyncGenerator<string, void, unknown>;
  /** Export identity credentials explicitly; their Recovery Key is sensitive. */
  exportCredentials(): Promise<InboxCredentials>;
  /** Read metadata without consuming the receive cursor. */
  listEmails(options?: ListOptions): Promise<EmailPage>;
  /** Read full parsed email; IDs are scoped to this inbox's address. */
  getEmail(id: number): Promise<Email>;
  /** Delete one stored message and return the number removed. */
  deleteEmail(id: number): Promise<number>;
  /** Clear only this inbox's messages and return the number removed. */
  clearEmails(): Promise<number>;
  /** Return attachment metadata and bytes without touching the filesystem. */
  getAttachment(id: number, attachmentId: string): Promise<AttachmentData>;
  /** Save to an absolute file/directory path; never overwrite. Returns the saved path. */
  saveAttachment(
    id: number,
    attachmentId: string,
    destination: string,
  ): Promise<string>;
  /** Original RFC 822 message bytes. */
  getRawEmail(id: number): Promise<Buffer>;
  /** Sanitized HTML. Remote resources are blocked by default. */
  getHtml(id: number, options?: { allowRemote?: boolean }): Promise<HtmlEmail>;
  /** Idempotent shutdown. Interrupts waits; persistent profile files remain. */
  close(): Promise<void>;
  [Symbol.asyncDispose](): Promise<void>;
}

/** Return after authentication/subscription, or throw a native configuration/connection error. */
export function createInbox(options?: InboxOptions): Promise<Inbox>;

export const Ahrara: Readonly<{ createInbox: typeof createInbox }>;
