// Package ahrara registers k6/x/ahrara using the in-process Rust core via Go.
package ahrara

import (
	"context"
	"errors"
	"time"

	sdk "github.com/Hitmasu/Ahrara/src/sdks/go"
	"github.com/grafana/sobek"
	"go.k6.io/k6/v2/js/modules"
)

func init() { modules.Register("k6/x/ahrara", &RootModule{}) }

// RootModule creates an Ahrara module for each k6 virtual user.
type RootModule struct{}

// Module connects SDK operations to one virtual user's lifecycle.
type Module struct{ vu modules.VU }

var _ modules.Module = (*RootModule)(nil)
var _ modules.Instance = (*Module)(nil)

// NewModuleInstance binds a module to vu.
func (*RootModule) NewModuleInstance(vu modules.VU) modules.Instance {
	return &Module{vu: vu}
}

// Exports exposes the JavaScript module's default export.
func (m *Module) Exports() modules.Exports {
	return modules.Exports{Default: m}
}

// Credentials restores an inbox identity. Read RecoveryKey only when explicitly
// storing or importing the secret; default string and JSON displays are redacted.
type Credentials struct {
	// Address is the inbox recipient to restore.
	Address string `js:"address"`
	// RecoveryKey is secret and grants access to the identity.
	RecoveryKey string `js:"recoveryKey"`
}

// String redacts credentials in Go diagnostics.
func (Credentials) String() string { return "Ahrara credentials (redacted)" }

// ToString redacts JavaScript String(credentials).
func (c Credentials) ToString() string { return c.String() }

// ToJSON prevents accidental secret disclosure through JSON.stringify.
func (c Credentials) ToJSON() map[string]any {
	return map[string]any{"address": c.Address, "recoveryKey": "[redacted]"}
}

// Options selects the relay and optional persistent or restored identity.
type Options struct {
	// ServerURL overrides the relay's secure WebSocket URL.
	ServerURL string `js:"serverUrl"`
	// ServerPublicKey is the relay's verified public Noise key.
	ServerPublicKey string `js:"serverPublicKey"`
	// TLSCAPath is an optional absolute path to a trusted PEM CA.
	TLSCAPath string `js:"tlsCaPath"`
	// ConnectTimeoutMS bounds registration, with zero using the core default.
	ConnectTimeoutMS uint64 `js:"connectTimeoutMs"`
	// ProfilePath retains identity and encrypted history at an absolute directory.
	ProfilePath string `js:"profilePath"`
	// Credentials imports a previously exported identity and address.
	Credentials *Credentials `js:"credentials"`
	// RecoveryKey imports a secret identity without a Credentials object.
	RecoveryKey string `js:"recoveryKey"`
	// Address selects an address when restoring RecoveryKey.
	Address string `js:"address"`
	// Filter discards nonmatching messages before local inbox storage.
	Filter *sdk.Filter `js:"filter"`
}

// ReceiveOptions selects matching messages. Receives have no timeout.
type ReceiveOptions struct {
	// Subject matches a substring of the subject header.
	Subject string `js:"subject"`
	// From matches a substring of the sender header.
	From string `js:"from"`
	// Filter combines sender, subject, headers, body and envelope conditions.
	Filter *sdk.Filter `js:"filter"`
}

// ForEachOptions selects messages and an optional deadline for the whole loop.
type ForEachOptions struct {
	// Subject matches a substring of the subject header.
	Subject string `js:"subject"`
	// From matches a substring of the sender header.
	From string `js:"from"`
	// Filter runs after admission and leaves nonmatches available.
	Filter *sdk.Filter `js:"filter"`
	// DurationMS bounds the whole callback loop. Zero runs until cancellation,
	// close, callback failure, or a callback returning false.
	DurationMS int64 `js:"durationMs"`
}

// ListOptions selects a non-consuming page of stored message metadata.
type ListOptions struct {
	// AfterID is the previous page cursor; zero starts the listing.
	AfterID int64 `js:"afterId"`
	// Limit is 1..100, with zero using 20.
	Limit uint32 `js:"limit"`
	// Subject matches a substring of the subject header.
	Subject string `js:"subject"`
	// From matches a substring of the sender header.
	From string `js:"from"`
}

// Inbox owns a live inbox within one virtual user's execution context.
type Inbox struct {
	// Address is the recipient registered by the core.
	Address     string `js:"address"`
	inner       *sdk.Inbox
	vu          modules.VU
	ctx         context.Context
	stopCleanup func() bool
}

// CreateInbox waits for registration and must be called during a VU iteration.
// Context cancellation also closes the inbox if explicit cleanup is interrupted.
func (m *Module) CreateInbox(options Options) (*Inbox, error) {
	if m.vu.State() == nil {
		return nil, errors.New("createInbox must run during a VU iteration")
	}

	settings := sdk.Options{
		ServerURL: options.ServerURL, ServerPublicKey: options.ServerPublicKey,
		TLSCAPath: options.TLSCAPath, ConnectTimeoutMS: options.ConnectTimeoutMS,
		ProfilePath: options.ProfilePath, RecoveryKey: options.RecoveryKey, Address: options.Address,
		Filter: options.Filter,
	}
	if options.Credentials != nil {
		settings.Credentials = &sdk.Credentials{Address: options.Credentials.Address, RecoveryKey: options.Credentials.RecoveryKey}
	}

	inner, err := sdk.CreateInbox(m.vu.Context(), settings)
	if err != nil {
		return nil, err
	}

	inbox := &Inbox{Address: inner.Address, inner: inner, vu: m.vu, ctx: m.vu.Context()}
	inbox.stopCleanup = context.AfterFunc(inbox.ctx, func() { _ = inner.Close() })
	return inbox, nil
}

// WaitForEmail waits without a receive deadline until a message, VU cancellation,
// Close, or an error. The synchronous call blocks only this virtual user.
func (i *Inbox) WaitForEmail(options ReceiveOptions) (map[string]any, error) {
	filter, err := receiveFilter(options.Subject, options.From, options.Filter)
	if err != nil {
		return nil, err
	}

	email, err := i.inner.WaitForEmail(i.ctx, filter)
	if err != nil {
		return nil, err
	}

	return emailValue(email), nil
}

// ForEachEmail invokes a synchronous JavaScript callback for successive messages.
// Returning false stops immediately; throwing propagates the callback error.
// No messages are prefetched. DurationMS is a total loop duration, checked between
// callbacks and during waits; an executing callback must return cooperatively.
// The return value is the number of callbacks invoked.
func (i *Inbox) ForEachEmail(callback sobek.Value, options ForEachOptions) (int64, error) {
	fn, ok := sobek.AssertFunction(callback)
	if !ok {
		return 0, errors.New("email callback must be a function")
	}

	filter, err := receiveFilter(options.Subject, options.From, options.Filter)
	if err != nil {
		return 0, err
	}
	if options.DurationMS < 0 || options.DurationMS > int64((1<<63-1)/time.Millisecond) {
		return 0, errors.New("durationMs must be a non-negative duration")
	}

	ctx := i.ctx
	if options.DurationMS != 0 {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, time.Duration(options.DurationMS)*time.Millisecond)
		defer cancel()
	}

	var count int64

	for {
		email, err := i.inner.WaitForEmail(ctx, filter)
		if errors.Is(err, context.DeadlineExceeded) && i.ctx.Err() == nil {
			return count, nil
		}
		if err != nil {
			return count, err
		}

		count++
		result, err := fn(sobek.Undefined(), i.vu.Runtime().ToValue(emailValue(email)))
		if err != nil {
			return count, err
		}
		if _, asynchronous := result.Export().(*sobek.Promise); asynchronous {
			return count, errors.New("forEachEmail requires a synchronous callback")
		}
		if value, isBoolean := result.Export().(bool); isBoolean && !value {
			return count, nil
		}
	}
}

// ExportCredentials returns explicit access to the secret needed to restore this identity.
func (i *Inbox) ExportCredentials() (*Credentials, error) {
	credentials, err := i.inner.ExportCredentials()
	if err != nil {
		return nil, err
	}

	return &Credentials{Address: credentials.Address, RecoveryKey: credentials.RecoveryKey}, nil
}

// ListEmails returns metadata without changing the receive cursor.
func (i *Inbox) ListEmails(options ListOptions) (map[string]any, error) {
	page, err := i.inner.ListEmails(sdk.ListOptions{AfterID: options.AfterID, Limit: options.Limit, Subject: options.Subject, From: options.From})
	if err != nil {
		return nil, err
	}

	emails := make([]map[string]any, len(page.Emails))

	for index, email := range page.Emails {
		emails[index] = metadataValue(email)
	}

	return map[string]any{"emails": emails, "nextCursor": page.NextCursor}, nil
}

// GetEmail reads a message without consuming it.
func (i *Inbox) GetEmail(id int64) (map[string]any, error) {
	email, err := i.inner.GetEmail(id)
	if err != nil {
		return nil, err
	}

	return emailValue(email), nil
}

// DeleteEmail removes one message and returns its deletion count.
func (i *Inbox) DeleteEmail(id int64) (int64, error) { return i.inner.DeleteEmail(id) }

// ClearEmails removes stored messages belonging to this inbox.
func (i *Inbox) ClearEmails() (int64, error) { return i.inner.ClearEmails() }

// GetAttachment returns attachment metadata and its bytes as an ArrayBuffer.
func (i *Inbox) GetAttachment(id int64, attachmentID string) (map[string]any, error) {
	result, err := i.inner.GetAttachment(id, attachmentID)
	if err != nil {
		return nil, err
	}

	return map[string]any{"attachment": attachmentValue(result.Attachment), "content": i.vu.Runtime().NewArrayBuffer(result.Content)}, nil
}

// SaveAttachment writes to an absolute destination without overwriting an existing file.
func (i *Inbox) SaveAttachment(id int64, attachmentID, destination string) (string, error) {
	return i.inner.SaveAttachment(id, attachmentID, destination)
}

// RawEmail returns original RFC 5322 bytes as an ArrayBuffer.
func (i *Inbox) RawEmail(id int64) (sobek.ArrayBuffer, error) {
	content, err := i.inner.RawEmail(id)
	if err != nil {
		return sobek.ArrayBuffer{}, err
	}

	return i.vu.Runtime().NewArrayBuffer(content), nil
}

// GetHTML returns sanitized HTML; remote content is blocked unless allowRemote is true.
func (i *Inbox) GetHTML(id int64, allowRemote bool) (map[string]any, error) {
	html, err := i.inner.GetHTML(id, allowRemote)
	if err != nil {
		return nil, err
	}

	return map[string]any{"html": html.HTML, "remoteResources": html.RemoteResources}, nil
}

// Close releases native resources and stops automatic VU cleanup. It is idempotent.
func (i *Inbox) Close() error {
	i.stopCleanup()
	return i.inner.Close()
}

func metadataValue(email sdk.EmailMetadata) map[string]any {
	return map[string]any{
		"id": email.ID, "transferId": email.TransferID, "identityFingerprint": email.IdentityFingerprint,
		"recipient": email.Recipient, "envelopeSender": email.EnvelopeSender, "receivedAt": email.ReceivedAt,
		"subject": email.Subject, "from": email.From, "to": email.To, "messageId": email.MessageID, "sizeBytes": email.SizeBytes,
	}
}

func attachmentValue(attachment sdk.Attachment) map[string]any {
	return map[string]any{"id": attachment.ID, "filename": attachment.Filename, "contentType": attachment.ContentType,
		"size": attachment.Size, "contentDisposition": attachment.ContentDisposition}
}

func emailValue(email *sdk.Email) map[string]any {
	value := metadataValue(email.EmailMetadata)
	value["cc"], value["date"], value["forwardedTo"] = email.CC, email.Date, email.ForwardedTo
	value["textBody"], value["htmlAvailable"], value["textBodyTruncated"] = email.TextBody, email.HTMLAvailable, email.TextBodyTruncated
	value["parseError"] = email.ParseError
	attachments := make([]map[string]any, len(email.Attachments))

	for index, attachment := range email.Attachments {
		attachments[index] = attachmentValue(attachment)
	}

	value["attachments"] = attachments
	return value
}
