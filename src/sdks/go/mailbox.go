package ahrara

// Credentials restores an identity and its inbox address. RecoveryKey grants
// access to that identity; store it as a secret and never log or commit it.
// Default fmt formatting is redacted. JSON serialization is an explicit export
// and includes the recovery key, so callers must protect its output.
type Credentials struct {
	// Address is the inbox recipient associated with the identity.
	Address string `json:"address"`
	// RecoveryKey is the secret needed to restore the identity.
	RecoveryKey string `json:"recovery_key"`
}

// String prevents recovery keys from appearing in default diagnostic output.
func (Credentials) String() string { return "Ahrara credentials (redacted)" }

// GoString redacts credentials in Go-syntax diagnostic formatting.
func (c Credentials) GoString() string { return c.String() }

// EmailMetadata describes a locally stored message without decoding its body.
type EmailMetadata struct {
	// ID is the local message identifier used by mailbox operations.
	ID int64 `json:"id"`
	// TransferID identifies the relay delivery.
	TransferID string `json:"transfer_id"`
	// IdentityFingerprint identifies the receiving identity without its secret.
	IdentityFingerprint string `json:"identity_fingerprint"`
	// Recipient is the envelope recipient accepted by the relay.
	Recipient string `json:"recipient"`
	// EnvelopeSender is the SMTP envelope sender.
	EnvelopeSender string `json:"envelope_sender"`
	// ReceivedAt is the reception timestamp recorded by the core.
	ReceivedAt string `json:"received_at"`
	// Subject is the decoded subject header.
	Subject string `json:"subject"`
	// From is the decoded sender header.
	From string `json:"from"`
	// To is the decoded recipient header.
	To string `json:"to"`
	// MessageID is the optional Message-ID header.
	MessageID *string `json:"message_id"`
	// SizeBytes is the size of the original RFC 5322 message.
	SizeBytes int64 `json:"size_bytes"`
}

// Email includes message metadata, decoded text and MIME attachment metadata.
type Email struct {
	// EmailMetadata contains the headers and identifiers shared with ListEmails.
	EmailMetadata
	// CC is the decoded carbon-copy header.
	CC string `json:"cc"`
	// Date is the message's Date header.
	Date string `json:"date"`
	// ForwardedTo records a forwarding destination when present.
	ForwardedTo *string `json:"forwarded_to"`
	// TextBody is the decoded plain-text body, limited by the core to 256 KiB.
	TextBody string `json:"text_body"`
	// TextBodyTruncated reports whether TextBody exceeded the decoding limit.
	TextBodyTruncated bool `json:"text_body_truncated"`
	// HTMLAvailable reports whether the message includes an HTML part.
	HTMLAvailable bool `json:"html_available"`
	// Attachments contains identifiers usable with GetAttachment and SaveAttachment.
	Attachments []Attachment `json:"attachments"`
	// ParseError reports that the core could not fully decode the message.
	ParseError bool `json:"parse_error"`
}

// Attachment describes one decoded MIME attachment.
type Attachment struct {
	// ID identifies this attachment within its message.
	ID string `json:"attachment_id"`
	// Filename is the name supplied by the MIME message.
	Filename string `json:"filename"`
	// ContentType is the MIME media type.
	ContentType string `json:"content_type"`
	// Size is the number of decoded content bytes.
	Size uint64 `json:"size"`
	// ContentDisposition is the MIME disposition, such as attachment or inline.
	ContentDisposition string `json:"content_disposition"`
}

// AttachmentData contains the attachment metadata and decoded bytes.
type AttachmentData struct {
	// Attachment identifies and describes the downloaded attachment.
	Attachment Attachment `json:"attachment"`
	// Content holds the exact decoded attachment bytes.
	Content []byte `json:"content_base64"`
}

// HTMLBody contains sanitized HTML and the number of detected remote resources.
type HTMLBody struct {
	// HTML is the sanitized HTML document.
	HTML string `json:"html"`
	// RemoteResources counts remote resources detected in the message.
	RemoteResources uint64 `json:"remote_resources"`
}

// ListOptions selects a page of messages without consuming receive events.
type ListOptions struct {
	// AfterID excludes messages at or below this cursor; zero starts the listing.
	AfterID int64
	// Limit bounds the page size to 1..100; zero uses 20.
	Limit uint32
	// Subject is an optional ASCII case-insensitive subject substring.
	Subject string
	// From is an optional ASCII case-insensitive substring of the From header or
	// envelope sender. Unlike Filter.From, it never uses regex or Unicode case folding.
	From string
}

// EmailPage contains mailbox metadata and a cursor for the next page.
type EmailPage struct {
	// Emails contains the message metadata for this page.
	Emails []EmailMetadata `json:"emails"`
	// NextCursor is nil when there is no next page.
	NextCursor *int64 `json:"next_cursor"`
}

func (i *Inbox) request(op string, fields map[string]any, result any) error {
	if i.closed.Load() {
		return ErrClosed
	}
	if fields == nil {
		fields = make(map[string]any)
	}

	fields["op"], fields["handle"] = op, i.handle
	return call(fields, result)
}

// ExportCredentials returns the secret needed to reopen the identity and address.
// Exporting credentials does not copy local history; use ProfilePath to retain it.
func (i *Inbox) ExportCredentials() (*Credentials, error) {
	var result Credentials
	err := i.request("export_credentials", nil, &result)
	if err != nil {
		return nil, err
	}

	return &result, nil
}

// ListEmails reads mailbox metadata without changing the receive cursor.
func (i *Inbox) ListEmails(options ListOptions) (*EmailPage, error) {
	limit := options.Limit
	if limit == 0 {
		limit = 20
	}

	fields := map[string]any{"after_id": options.AfterID, "limit": limit}
	if options.Subject != "" {
		fields["subject"] = options.Subject
	}
	if options.From != "" {
		fields["from"] = options.From
	}

	var result EmailPage
	err := i.request("list", fields, &result)
	if err != nil {
		return nil, err
	}

	return &result, nil
}

// GetEmail reads a message and attachment metadata without consuming it.
func (i *Inbox) GetEmail(id int64) (*Email, error) {
	var result Email
	err := i.request("get", map[string]any{"id": id}, &result)
	if err != nil {
		return nil, err
	}

	return &result, nil
}

// DeleteEmail removes a message from this inbox and returns the number deleted.
func (i *Inbox) DeleteEmail(id int64) (int64, error) {
	var result struct {
		Deleted int64 `json:"deleted"`
	}
	err := i.request("delete", map[string]any{"id": id}, &result)
	return result.Deleted, err
}

// ClearEmails deletes messages belonging to this inbox and returns their count.
func (i *Inbox) ClearEmails() (int64, error) {
	var result struct {
		Deleted int64 `json:"deleted"`
	}
	err := i.request("clear", nil, &result)
	return result.Deleted, err
}

// GetAttachment reads and decodes one attachment from the specified message.
func (i *Inbox) GetAttachment(id int64, attachmentID string) (*AttachmentData, error) {
	var result AttachmentData
	if err := i.request("attachment", map[string]any{"id": id, "attachment_id": attachmentID}, &result); err != nil {
		return nil, err
	}

	return &result, nil
}

// SaveAttachment writes an attachment to an absolute filename or directory and
// returns the saved path. The native core refuses to overwrite existing files.
func (i *Inbox) SaveAttachment(id int64, attachmentID, destination string) (string, error) {
	var result struct {
		Path string `json:"path"`
	}
	err := i.request("save_attachment", map[string]any{"id": id, "attachment_id": attachmentID, "destination": destination}, &result)
	return result.Path, err
}

// RawEmail returns the original RFC 5322 message bytes.
func (i *Inbox) RawEmail(id int64) ([]byte, error) {
	var result struct {
		Content []byte `json:"content_base64"`
	}
	err := i.request("raw", map[string]any{"id": id}, &result)
	return result.Content, err
}

// GetHTML returns sanitized HTML. Remote resources are blocked unless
// allowRemote is explicitly true; the SDK never fetches those resources itself.
func (i *Inbox) GetHTML(id int64, allowRemote bool) (*HTMLBody, error) {
	var result HTMLBody
	err := i.request("html", map[string]any{"id": id, "allow_remote": allowRemote}, &result)
	if err != nil {
		return nil, err
	}

	return &result, nil
}
