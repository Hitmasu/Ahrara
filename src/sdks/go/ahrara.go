// Package ahrara receives disposable email using the shared Rust core in process.
package ahrara

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"iter"
	"runtime"
	"sync"
	"sync/atomic"
	"time"
)

// ErrClosed indicates that an operation used an inbox after Close.
var ErrClosed = errors.New("Ahrara inbox is closed")

// Options overrides the core's production relay settings for a private relay.
type Options struct {
	// ServerURL overrides the relay's secure WebSocket endpoint.
	ServerURL string `json:"server_url,omitempty"`
	// ServerPublicKey is the verified public Noise key of the relay.
	ServerPublicKey string `json:"server_public_key,omitempty"`
	// TLSCAPath adds a PEM certificate authority from an absolute file path.
	TLSCAPath string `json:"tls_ca_path,omitempty"`
	// ConnectTimeoutMS bounds initial registration; zero uses the core default.
	ConnectTimeoutMS uint64 `json:"connect_timeout_ms,omitempty"`
	// ProfilePath preserves identity, address and encrypted history in an absolute directory.
	// An empty path creates a disposable profile removed after Close.
	ProfilePath string `json:"profile_path,omitempty"`
	// Credentials imports a previously exported identity and inbox address.
	Credentials *Credentials `json:"credentials,omitempty"`
	// RecoveryKey imports an identity without an exported Credentials value.
	RecoveryKey string `json:"recovery_key,omitempty"`
	// Address selects the inbox address when importing RecoveryKey.
	Address string `json:"address,omitempty"`
	// Filter rejects nonmatching messages before storing them in this inbox.
	Filter *Filter `json:"filter,omitempty"`
}

// String redacts configuration that may contain recovery credentials.
func (Options) String() string { return "Ahrara options (credentials redacted)" }

// GoString redacts configuration in Go-syntax diagnostic formatting.
func (o Options) GoString() string { return o.String() }

// Filter combines all populated conditions. Header/text matches are Unicode
// case-insensitive substrings; envelope fields and MessageID match the whole
// value, ignoring case. With Regex, strings are case-insensitive Rust regex
// patterns: Recipient, EnvelopeSender and MessageID must match the whole value,
// while the others match anywhere. Rejected admission messages are discarded;
// per-call nonmatches remain available for other receives.
type Filter struct {
	// Regex interprets string conditions as case-insensitive Rust regex patterns.
	// Header names remain literal; inline flags such as (?-i) override case.
	Regex bool `json:"regex,omitempty" js:"regex"`
	// Subject matches a substring of the subject header; empty matches any subject.
	Subject string `json:"subject,omitempty" js:"subject"`
	// From matches the decoded From header, including display names; empty
	// matches any sender. Unlike ListOptions.From, it follows the Filter rules.
	From string `json:"sender,omitempty" js:"sender"`
	// To matches any decoded To header.
	To string `json:"to,omitempty" js:"to"`
	// CC matches any decoded Cc header.
	CC string `json:"cc,omitempty" js:"cc"`
	// Recipient matches the whole SMTP recipient.
	Recipient string `json:"recipient,omitempty" js:"recipient"`
	// EnvelopeSender matches the whole SMTP sender.
	EnvelopeSender string `json:"envelope_sender,omitempty" js:"envelopeSender"`
	// MessageID matches the whole Message-ID header.
	MessageID string `json:"message_id,omitempty" js:"messageId"`
	// Text searches the complete decoded plain-text body, excluding attachments.
	Text string `json:"text,omitempty" js:"text"`
	// HasAttachments selects messages with or without MIME attachments when non-nil.
	HasAttachments *bool `json:"has_attachments,omitempty" js:"hasAttachments"`
	// Headers maps header names to substrings; any repeated value can match.
	Headers map[string]string `json:"headers,omitempty" js:"headers"`
}

func (f Filter) apply(request map[string]any) {
	if f.Regex || f.Subject != "" || f.From != "" || f.To != "" || f.CC != "" ||
		f.Recipient != "" || f.EnvelopeSender != "" || f.MessageID != "" ||
		f.Text != "" || f.HasAttachments != nil || len(f.Headers) != 0 {
		request["filter"] = f
	}
}

// Error preserves the stable native error code without exposing identities.
type Error struct {
	// Code is the stable native error category.
	Code string `json:"code"`
	// Message describes the failed operation.
	Message string `json:"message"`
}

// Error returns the native category and message.
func (e *Error) Error() string { return fmt.Sprintf("Ahrara %s: %s", e.Code, e.Message) }

// Inbox owns a live connection. Always Close it, normally with defer or t.Cleanup;
// an unreachable inbox is closed by a finalizer as a fallback. Multiple
// goroutines may use an Inbox. Concurrent waits queue and consume messages one
// at a time.
type Inbox struct {
	// Address is the registered recipient address. Treat it as read-only.
	Address   string
	handle    uint64
	closed    atomic.Bool
	closeOnce sync.Once
	closeErr  error
	waitGate  chan struct{}
}

func call(request any, result any) error {
	data, err := json.Marshal(request)
	if err != nil {
		return err
	}

	data, err = nativeCall(data)
	if err != nil {
		return err
	}

	var envelope struct {
		OK     bool            `json:"ok"`
		Result json.RawMessage `json:"result"`
		Error  *Error          `json:"error"`
	}
	if err := json.Unmarshal(data, &envelope); err != nil {
		return fmt.Errorf("invalid Ahrara native response: %w", err)
	}
	if !envelope.OK {
		if envelope.Error == nil {
			return errors.New("Ahrara native call failed without an error")
		}

		return envelope.Error
	}
	if result == nil {
		return nil
	}

	return json.Unmarshal(envelope.Result, result)
}

// CreateInbox returns only after the relay is ready to receive email. Canceling
// ctx returns promptly; any in-flight native creation is closed when it finishes.
func CreateInbox(ctx context.Context, options Options) (*Inbox, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if deadline, ok := ctx.Deadline(); ok {
		remaining := uint64(max(1, time.Until(deadline).Milliseconds()))
		connectTimeout := options.ConnectTimeoutMS
		if connectTimeout == 0 {
			connectTimeout = 20000 // The native core's default connection timeout.
		}
		if connectTimeout > remaining {
			options.ConnectTimeoutMS = remaining
		}
	}

	type outcome struct {
		inbox *Inbox
		err   error
	}

	done := make(chan outcome, 1)
	go func() {
		var result struct {
			Handle  uint64 `json:"handle"`
			Address string `json:"address"`
		}
		err := call(map[string]any{"op": "create", "options": options}, &result)
		if err != nil {
			done <- outcome{err: err}
			return
		}

		inbox := &Inbox{
			Address:  result.Address,
			handle:   result.Handle,
			waitGate: make(chan struct{}, 1),
		}
		// Native close can wait for the relay, so it must not block the finalizer goroutine.
		runtime.SetFinalizer(inbox, func(i *Inbox) { go i.Close() })
		done <- outcome{inbox: inbox}
	}()
	select {
	case result := <-done:
		if err := ctx.Err(); err != nil {
			if result.inbox != nil {
				_ = result.inbox.Close()
			}

			return nil, err
		}

		return result.inbox, result.err
	case <-ctx.Done():
		go func() {
			result := <-done
			if result.inbox != nil {
				_ = result.inbox.Close()
			}
		}()

		return nil, ctx.Err()
	}
}

// WaitForEmail consumes the next matching email, including one received before
// this call. There is no default deadline: a background context waits until mail,
// Close or an error. Use context.WithTimeout for an explicit deadline.
// Cancellation is observed in 100 ms native slices and leaves the inbox open.
func (i *Inbox) WaitForEmail(ctx context.Context, filter Filter) (*Email, error) {
	request := map[string]any{"op": "wait"}
	filter.apply(request)
	result, err := i.receive(ctx, request)
	if err != nil {
		return nil, err
	}
	if result.Email == nil {
		return nil, errors.New("unexpected Ahrara email response")
	}

	return result.Email, nil
}

type receiveResult struct {
	Status  string       `json:"status"`
	Email   *Email       `json:"email"`
	Matches []RegexMatch `json:"matches"`
	Values  []string     `json:"values"`
}

func (i *Inbox) receive(ctx context.Context, request map[string]any) (*receiveResult, error) {
	if i.closed.Load() {
		return nil, ErrClosed
	}
	select {
	case i.waitGate <- struct{}{}:
		defer func() { <-i.waitGate }()
	case <-ctx.Done():
		return nil, ctx.Err()
	}

	for {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		if i.closed.Load() {
			return nil, ErrClosed
		}

		timeout := int64(100)
		if deadline, ok := ctx.Deadline(); ok {
			timeout = max(1, min(timeout, time.Until(deadline).Milliseconds()))
		}

		request["handle"], request["timeout_ms"] = i.handle, timeout
		var result receiveResult
		if err := call(request, &result); err != nil {
			if i.closed.Load() {
				return nil, ErrClosed
			}

			return nil, err
		}
		if result.Status == "matched" {
			return &result, nil
		}
		if result.Status != "timeout" {
			return nil, errors.New("unexpected Ahrara wait response")
		}
	}
}

// Emails yields matching messages sequentially until ctx ends, Close is called,
// or an error occurs. The terminal error is yielded once with a nil email.
// Breaking the range loop stops immediately without consuming the next message.
// The iterator has no background goroutine or prefetch buffer and leaves the
// inbox open, so callers still own Close.
func (i *Inbox) Emails(ctx context.Context, filter Filter) iter.Seq2[*Email, error] {
	return func(yield func(*Email, error) bool) {
		for {
			email, err := i.WaitForEmail(ctx, filter)
			if !yield(email, err) || err != nil {
				return
			}
		}
	}
}

// ForEachEmail invokes callback synchronously for each matching message. It
// stops on the first callback error, context cancellation, Close, or receive
// error and returns that error unchanged. The inbox remains owned by the caller.
// A deadline on ctx bounds the whole loop, rather than restarting per message.
func (i *Inbox) ForEachEmail(ctx context.Context, filter Filter, callback func(*Email) error) error {
	if callback == nil {
		return errors.New("Ahrara email callback must not be nil")
	}
	for email, err := range i.Emails(ctx, filter) {
		if err != nil {
			return err
		}
		if err := callback(email); err != nil {
			return err
		}
	}

	return nil
}

// Close is idempotent and interrupts active waits. The inbox cannot be reused.
func (i *Inbox) Close() error {
	i.closeOnce.Do(func() {
		i.closed.Store(true)
		i.closeErr = call(map[string]any{"op": "close", "handle": i.handle}, nil)
	})

	return i.closeErr
}
