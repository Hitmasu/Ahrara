package ahrara_test

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"net"
	"net/smtp"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	ahrara "github.com/Hitmasu/Ahrara/src/sdks/go"
)

func options(t *testing.T) ahrara.Options {
	t.Helper()
	if os.Getenv("AHRARA_TEST_SERVER_URL") == "" {
		t.Skip("set AHRARA_TEST_SERVER_URL to run against a development relay")
	}

	return ahrara.Options{
		ServerURL:        os.Getenv("AHRARA_TEST_SERVER_URL"),
		ServerPublicKey:  os.Getenv("AHRARA_TEST_SERVER_PUBLIC_KEY"),
		TLSCAPath:        os.Getenv("AHRARA_TEST_TLS_CA"),
		ConnectTimeoutMS: 10000,
	}
}

func newInbox(t *testing.T) *ahrara.Inbox {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	inbox, err := ahrara.CreateInbox(ctx, options(t))
	if err != nil {
		t.Fatal(err)
	}

	t.Cleanup(func() {
		if err := inbox.Close(); err != nil {
			t.Error(err)
		}
	})

	return inbox
}

func send(address, subject, body string) error {
	message := fmt.Sprintf("From: sender@example.test\r\nTo: %s\r\nSubject: %s\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n%s\r\n", address, subject, body)
	return sendRaw(address, message)
}

func sendRaw(address, message string) error {
	endpoint := os.Getenv("AHRARA_TEST_SMTP_ADDRESS")
	if endpoint == "" {
		endpoint = "localhost:2525"
	}

	host, _, err := net.SplitHostPort(endpoint)
	if err != nil {
		return err
	}

	connection, err := net.DialTimeout("tcp", endpoint, 5*time.Second)
	if err != nil {
		return err
	}

	defer connection.Close()
	if err := connection.SetDeadline(time.Now().Add(10 * time.Second)); err != nil {
		return err
	}

	client, err := smtp.NewClient(connection, host)
	if err != nil {
		return err
	}

	defer client.Close()
	ca, err := os.ReadFile(os.Getenv("AHRARA_TEST_TLS_CA"))
	if err != nil {
		return err
	}

	roots := x509.NewCertPool()
	if !roots.AppendCertsFromPEM(ca) {
		return errors.New("invalid test CA")
	}
	if err := client.StartTLS(&tls.Config{RootCAs: roots, ServerName: host, MinVersion: tls.VersionTLS12}); err != nil {
		return err
	}
	if err := client.Mail("sender@example.test"); err != nil {
		return err
	}
	if err := client.Rcpt(address); err != nil {
		return err
	}

	writer, err := client.Data()
	if err != nil {
		return err
	}

	_, err = writer.Write([]byte(message))
	if err != nil {
		return err
	}
	if err := writer.Close(); err != nil {
		return err
	}

	return client.Quit()
}

func TestRegexQueriesApplyAdmissionThenCallFiltersAcrossEmails(t *testing.T) {
	settings := options(t)
	settings.Filter = &ahrara.Filter{From: "SENDER@EXAMPLE.TEST", Headers: map[string]string{"X-Flow": "accept"}}
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	inbox, err := ahrara.CreateInbox(ctx, settings)
	if err != nil {
		t.Fatal(err)
	}

	t.Cleanup(func() { _ = inbox.Close() })

	for _, message := range []struct{ subject, header, body string }{
		{"Drop", "reject", "999999 is your verification code"},
		{"Retain", "accept", "888888 is your verification code"},
		{"Verification", "accept", "123456 is your verification code\n234567 is your verification code"},
		{"Verification", "accept", "345678 is your verification code"},
	} {
		raw := fmt.Sprintf("From: sender@example.test\r\nTo: %s\r\nSubject: %s\r\nX-Flow: %s\r\n\r\n%s\r\n", inbox.Address, message.subject, message.header, message.body)
		if err := sendRaw(inbox.Address, raw); err != nil {
			t.Fatal(err)
		}
	}

	period, stop := context.WithTimeout(ctx, 1500*time.Millisecond)
	defer stop()
	var codes []string
	var terminal error

	for match, err := range inbox.WaitForRegexMatches(period, `(?P<code>[0-9]{6}) is your verification code`, ahrara.RegexOptions{Filter: ahrara.Filter{Subject: "verification", Headers: map[string]string{"x-flow": "ACCEPT"}}}) {
		if err != nil {
			terminal = err
			break
		}
		if len(match.Groups) != 2 || match.Groups[1].Name == nil || *match.Groups[1].Name != "code" || match.Groups[1].Value == nil {
			t.Fatal("missing named capture")
		}
		if match.Groups[0].Value == nil || *match.Groups[0].Value != match.Value {
			t.Fatal("missing entire match")
		}

		codes = append(codes, *match.Groups[1].Value)
	}
	if strings.Join(codes, ",") != "123456,234567,345678" || !errors.Is(terminal, context.DeadlineExceeded) {
		t.Fatalf("unexpected extraction: %d codes, deadline=%v", len(codes), errors.Is(terminal, context.DeadlineExceeded))
	}

	page, err := inbox.ListEmails(ahrara.ListOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if len(page.Emails) != 3 {
		t.Fatalf("admission must discard rejected mail, stored=%d", len(page.Emails))
	}

	retained, err := inbox.WaitForEmail(ctx, ahrara.Filter{Subject: "Retain", Text: "888888"})
	if err != nil || retained.Subject != "Retain" {
		t.Fatal("method filter consumed unrelated mail", err)
	}

	_, err = inbox.WaitForRegexMatch(ctx, "[", ahrara.RegexOptions{})
	var native *ahrara.Error
	if !errors.As(err, &native) || native.Code != "invalid_argument" {
		t.Fatalf("invalid regex must fail before waiting: %v", err)
	}
}

func TestXPathQueriesAndEarlyRegexExitLeaveNextEmailAvailable(t *testing.T) {
	inbox := newInbox(t)
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	for _, body := range []string{"<p><a href='/first?a=1&amp;b=2'>One</a><br><a href='/second'>Two</a>", "<p><a href='/third'>Three</a>"} {
		raw := fmt.Sprintf("From: sender@example.test\r\nTo: %s\r\nSubject: HTML\r\nContent-Type: text/html; charset=utf-8\r\n\r\n%s", inbox.Address, body)
		if err := sendRaw(inbox.Address, raw); err != nil {
			t.Fatal(err)
		}
	}

	period, stop := context.WithTimeout(ctx, 1200*time.Millisecond)
	defer stop()
	var links []string
	var terminal error

	for value, err := range inbox.WaitForXPathValues(period, "//a/@href", ahrara.Filter{Subject: "HTML"}) {
		if err != nil {
			terminal = err
			break
		}

		links = append(links, value)
	}
	if strings.Join(links, ",") != "/first?a=1&b=2,/second,/third" || !errors.Is(terminal, context.DeadlineExceeded) {
		t.Fatal("XPath did not traverse every email during the period")
	}

	for _, code := range []string{"123456", "234567"} {
		if err := send(inbox.Address, "Code", code); err != nil {
			t.Fatal(err)
		}
	}

	for match, err := range inbox.WaitForRegexMatches(ctx, `[0-9]{6}`, ahrara.RegexOptions{}) {
		if err != nil || match.Value != "123456" {
			t.Fatal("first match was not yielded", err)
		}

		break
	}

	match, err := inbox.WaitForRegexMatch(ctx, `[0-9]{6}`, ahrara.RegexOptions{})
	if err != nil || match.Value != "234567" {
		t.Fatal("early exit consumed the next email", err)
	}

	cancelled, stopCancelled := context.WithCancel(ctx)
	stopCancelled()

	for _, err := range inbox.WaitForXPathValues(cancelled, "//a", ahrara.Filter{}) {
		if !errors.Is(err, context.Canceled) {
			t.Fatal("query cancellation did not propagate")
		}
	}
}

func TestLargeRegexQueryMakesProgressAcrossNativeWaitSlices(t *testing.T) {
	inbox := newInbox(t)
	if err := send(inbox.Address, "Large full-body query", strings.Repeat(strings.Repeat("a", 76)+"\r\n", 40000)+"123456"); err != nil {
		t.Fatal(err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	match, err := inbox.WaitForRegexMatch(ctx, `[a\r\n]+(?P<code>[0-9]{6})`, ahrara.RegexOptions{})
	if err != nil {
		t.Fatal("full-body query did not make progress across receive slices:", err)
	}
	if len(match.Groups) != 2 || match.Groups[1].Value == nil || *match.Groups[1].Value != "123456" {
		t.Fatal("large full-body query lost its named capture")
	}
}

func TestPersistentProfileAndCredentialRecovery(t *testing.T) {
	settings := options(t)
	settings.ProfilePath = filepath.Join(t.TempDir(), "profile")
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	inbox, err := ahrara.CreateInbox(ctx, settings)
	if err != nil {
		t.Fatal(err)
	}

	t.Cleanup(func() { _ = inbox.Close() })
	address := inbox.Address
	credentials, err := inbox.ExportCredentials()
	if err != nil {
		t.Fatal(err)
	}
	if credentials.Address != address || credentials.RecoveryKey == "" {
		t.Fatal("missing exported credentials")
	}

	for _, diagnostic := range []string{fmt.Sprint(credentials), fmt.Sprintf("%#v", credentials), fmt.Sprint(ahrara.Options{Credentials: credentials})} {
		if strings.Contains(diagnostic, credentials.RecoveryKey) {
			t.Fatal("diagnostic formatting exposed recovery credentials")
		}
	}
	if err := send(address, "Consumed before reopen", "first"); err != nil {
		t.Fatal(err)
	}
	if _, err := inbox.WaitForEmail(ctx, ahrara.Filter{}); err != nil {
		t.Fatal(err)
	}
	if err := send(address, "Available after reopen", "second"); err != nil {
		t.Fatal(err)
	}

	// List waits only for the second delivery to be committed, without consuming it.
	for {
		page, err := inbox.ListEmails(ahrara.ListOptions{})
		if err != nil {
			t.Fatal(err)
		}
		if len(page.Emails) == 2 {
			break
		}
		if err := ctx.Err(); err != nil {
			t.Fatal(err)
		}

		time.Sleep(10 * time.Millisecond)
	}
	if err := inbox.Close(); err != nil {
		t.Fatal(err)
	}

	reopened, err := ahrara.CreateInbox(ctx, settings)
	if err != nil {
		t.Fatal(err)
	}

	t.Cleanup(func() { _ = reopened.Close() })
	if reopened.Address != address {
		t.Fatal("profile did not preserve the inbox address")
	}

	email, err := reopened.WaitForEmail(ctx, ahrara.Filter{})
	if err != nil {
		t.Fatal(err)
	}
	if email.Subject != "Available after reopen" {
		t.Fatal("profile replayed a consumed message")
	}

	page, err := reopened.ListEmails(ahrara.ListOptions{Limit: 1})
	if err != nil {
		t.Fatal(err)
	}
	if len(page.Emails) != 1 || page.NextCursor == nil {
		t.Fatal("pagination did not expose retained history")
	}

	next, err := reopened.ListEmails(ahrara.ListOptions{AfterID: *page.NextCursor, Limit: 1})
	if err != nil {
		t.Fatal(err)
	}
	if len(next.Emails) != 1 || next.Emails[0].ID == page.Emails[0].ID {
		t.Fatal("pagination repeated a message")
	}
	if err := reopened.Close(); err != nil {
		t.Fatal(err)
	}

	settings.ProfilePath, settings.Credentials = "", credentials
	restored, err := ahrara.CreateInbox(ctx, settings)
	if err != nil {
		t.Fatal(err)
	}

	t.Cleanup(func() { _ = restored.Close() })
	if restored.Address != address {
		t.Fatal("credentials did not restore the address")
	}

	empty, err := restored.ListEmails(ahrara.ListOptions{})
	if err != nil || len(empty.Emails) != 0 {
		t.Fatal("credential import should start with fresh local history")
	}
	if err := send(address, "Recovered identity", "restored"); err != nil {
		t.Fatal(err)
	}
	if _, err := restored.WaitForEmail(ctx, ahrara.Filter{}); err != nil {
		t.Fatal(err)
	}
	if err := restored.Close(); err != nil {
		t.Fatal(err)
	}

	settings.Credentials = nil
	settings.RecoveryKey, settings.Address = credentials.RecoveryKey, credentials.Address
	fromKey, err := ahrara.CreateInbox(ctx, settings)
	if err != nil {
		t.Fatal(err)
	}

	t.Cleanup(func() { _ = fromKey.Close() })
	if fromKey.Address != address {
		t.Fatal("raw recovery key did not restore the address")
	}
}

func TestMailboxCRUDAndAttachmentBytes(t *testing.T) {
	inbox := newInbox(t)
	message := fmt.Sprintf("From: sender@example.test\r\nTo: %s\r\nSubject: MIME fixture\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=ahrara-test\r\n\r\n--ahrara-test\r\nContent-Type: text/plain\r\n\r\nReadable body\r\n--ahrara-test\r\nContent-Type: text/html\r\n\r\n<p>Readable HTML</p><img src=\"https://remote.example.test/image.png\">\r\n--ahrara-test\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"sample.bin\"\r\nContent-Transfer-Encoding: base64\r\n\r\nAQIDBA==\r\n--ahrara-test--\r\n", inbox.Address)
	if err := sendRaw(inbox.Address, message); err != nil {
		t.Fatal(err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	email, err := inbox.WaitForEmail(ctx, ahrara.Filter{})
	if err != nil {
		t.Fatal(err)
	}
	if len(email.Attachments) != 1 {
		t.Fatal("missing attachment metadata")
	}

	fetched, err := inbox.GetEmail(email.ID)
	if err != nil || fetched.Subject != "MIME fixture" {
		t.Fatal("failed to fetch consumed mail")
	}

	attachment, err := inbox.GetAttachment(email.ID, email.Attachments[0].ID)
	if err != nil {
		t.Fatal(err)
	}
	if string(attachment.Content) != string([]byte{1, 2, 3, 4}) {
		t.Fatal("attachment bytes changed")
	}

	destination := t.TempDir()
	path, err := inbox.SaveAttachment(email.ID, email.Attachments[0].ID, destination)
	if err != nil {
		t.Fatal(err)
	}

	saved, err := os.ReadFile(path)
	if err != nil || string(saved) != string(attachment.Content) {
		t.Fatal("saved attachment content changed")
	}
	if _, err := inbox.SaveAttachment(email.ID, email.Attachments[0].ID, path); err == nil {
		t.Fatal("attachment save overwrote an existing file")
	}

	raw, err := inbox.RawEmail(email.ID)
	if err != nil || !strings.Contains(string(raw), "multipart/mixed") {
		t.Fatal("original message bytes were not returned")
	}

	html, err := inbox.GetHTML(email.ID, false)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(html.HTML, "Readable HTML") || html.RemoteResources != 1 || strings.Contains(html.HTML, ` src="https://remote.example.test/image.png"`) {
		t.Fatal("HTML content or remote-resource sanitization changed")
	}

	deleted, err := inbox.DeleteEmail(email.ID)
	if err != nil || deleted != 1 {
		t.Fatal("message deletion failed")
	}
	if _, err := inbox.GetEmail(email.ID); err == nil {
		t.Fatal("deleted message was still accessible")
	}
	if err := send(inbox.Address, "Clear me", "clear body"); err != nil {
		t.Fatal(err)
	}
	if _, err := inbox.WaitForEmail(ctx, ahrara.Filter{}); err != nil {
		t.Fatal(err)
	}

	cleared, err := inbox.ClearEmails()
	if err != nil || cleared != 1 {
		t.Fatal("mailbox clear failed")
	}

	page, err := inbox.ListEmails(ahrara.ListOptions{})
	if err != nil || len(page.Emails) != 0 {
		t.Fatal("mailbox still contained messages after clear")
	}
}

func TestIteratorEarlyBreakCallbackErrorsAndCancellation(t *testing.T) {
	inbox := newInbox(t)
	for _, subject := range []string{"Iterator first", "Callback second"} {
		if err := send(inbox.Address, subject, subject); err != nil {
			t.Fatal(err)
		}
	}

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	count := 0

	for email, err := range inbox.Emails(ctx, ahrara.Filter{}) {
		if err != nil {
			t.Fatal(err)
		}
		if email.Subject != "Iterator first" {
			t.Fatal("iterator returned messages out of order")
		}

		count++
		break
	}
	if count != 1 {
		t.Fatal("iterator did not yield one message")
	}

	callbackError := errors.New("synthetic callback failure")
	err := inbox.ForEachEmail(ctx, ahrara.Filter{}, func(email *ahrara.Email) error {
		if email.Subject != "Callback second" {
			t.Error("early break consumed the next message")
		}

		return callbackError
	})
	if err != callbackError {
		t.Fatal("callback error was not propagated unchanged")
	}

	wait, stop := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- inbox.ForEachEmail(wait, ahrara.Filter{}, func(*ahrara.Email) error { return nil }) }()
	time.Sleep(50 * time.Millisecond)
	stop()
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatal("stream cancellation did not propagate")
		}
	case <-time.After(time.Second):
		t.Fatal("stream left a blocked receive after cancellation")
	}
}

func TestReceivesMessagesSentBeforeAndDuringWait(t *testing.T) {
	inbox := newInbox(t)
	if err := send(inbox.Address, "Already received", "first body"); err != nil {
		t.Fatal(err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	first, err := inbox.WaitForEmail(ctx, ahrara.Filter{Subject: "Already received"})
	if err != nil {
		t.Fatal(err)
	}
	if first.Subject != "Already received" || !strings.Contains(first.TextBody, "first body") {
		t.Fatal("first email did not preserve its subject and body")
	}

	delivery := make(chan error, 1)
	go func() {
		time.Sleep(150 * time.Millisecond)
		delivery <- send(inbox.Address, "During wait", "second body")
	}()
	second, err := inbox.WaitForEmail(ctx, ahrara.Filter{From: "sender@example.test"})
	if err != nil {
		t.Fatal(err)
	}
	if err := <-delivery; err != nil {
		t.Fatal(err)
	}
	if second.Subject != "During wait" || !strings.Contains(second.TextBody, "second body") {
		t.Fatal("wait returned the wrong message")
	}
	if first.ID == second.ID {
		t.Fatal("two emails had the same ID")
	}
}

func TestTimeoutCancellationAndClose(t *testing.T) {
	inbox := newInbox(t)
	ctx, cancel := context.WithTimeout(context.Background(), 150*time.Millisecond)
	defer cancel()
	if _, err := inbox.WaitForEmail(ctx, ahrara.Filter{}); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("expected deadline exceeded, got %v", err)
	}

	canceled, stop := context.WithCancel(context.Background())
	stop()
	if _, err := inbox.WaitForEmail(canceled, ahrara.Filter{}); !errors.Is(err, context.Canceled) {
		t.Fatalf("expected cancellation, got %v", err)
	}

	done := make(chan error, 1)
	go func() { _, err := inbox.WaitForEmail(context.Background(), ahrara.Filter{}); done <- err }()
	if err := inbox.Close(); err != nil {
		t.Fatal(err)
	}
	if err := inbox.Close(); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-done:
		if !errors.Is(err, ahrara.ErrClosed) {
			t.Fatalf("expected closed inbox error, got %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("close did not cancel the waiter")
	}
	if _, err := inbox.WaitForEmail(context.Background(), ahrara.Filter{}); !errors.Is(err, ahrara.ErrClosed) {
		t.Fatalf("expected closed inbox error, got %v", err)
	}
}

func TestParallelInboxIsolation(t *testing.T) {
	first, second := newInbox(t), newInbox(t)
	if first.Address == second.Address {
		t.Fatal("isolated inboxes shared an address")
	}

	var group sync.WaitGroup

	for n, inbox := range []*ahrara.Inbox{first, second} {
		group.Add(1)
		go func(n int, inbox *ahrara.Inbox) {
			defer group.Done()
			subject := fmt.Sprintf("Isolated message %d", n)
			if err := send(inbox.Address, subject, subject); err != nil {
				t.Error(err)
				return
			}

			ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()
			email, err := inbox.WaitForEmail(ctx, ahrara.Filter{})
			if err != nil {
				t.Error(err)
				return
			}
			if email.Subject != subject {
				t.Error("inbox received another inbox's email")
			}
		}(n, inbox)
	}

	group.Wait()
}

func TestFilterPreservesUnmatchedMail(t *testing.T) {
	inbox := newInbox(t)
	if err := send(inbox.Address, "Other message", "retained"); err != nil {
		t.Fatal(err)
	}
	if err := send(inbox.Address, "Verification", "123456"); err != nil {
		t.Fatal(err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	matched, err := inbox.WaitForEmail(ctx, ahrara.Filter{Subject: "Verification"})
	if err != nil {
		t.Fatal(err)
	}
	if matched.Subject != "Verification" {
		t.Fatal("subject filter did not match")
	}

	retained, err := inbox.WaitForEmail(ctx, ahrara.Filter{})
	if err != nil {
		t.Fatal(err)
	}
	if retained.Subject != "Other message" {
		t.Fatal("filter discarded unmatched mail")
	}
}

func TestCanceledCreationDoesNotConnect(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := ahrara.CreateInbox(ctx, ahrara.Options{}); !errors.Is(err, context.Canceled) {
		t.Fatalf("expected cancellation, got %v", err)
	}
}

func TestCancellationKeepsInboxUsable(t *testing.T) {
	inbox := newInbox(t)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan error, 1)
	go func() {
		_, err := inbox.WaitForEmail(ctx, ahrara.Filter{})
		done <- err
	}()
	time.Sleep(50 * time.Millisecond)
	cancel()
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("expected canceled active wait, got %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("active wait ignored context cancellation")
	}
	if err := send(inbox.Address, "After cancellation", "still receiving"); err != nil {
		t.Fatal(err)
	}

	wait, stop := context.WithTimeout(context.Background(), 10*time.Second)
	defer stop()
	email, err := inbox.WaitForEmail(wait, ahrara.Filter{})
	if err != nil {
		t.Fatal(err)
	}
	if email.Subject != "After cancellation" {
		t.Fatal("canceling a wait damaged the inbox")
	}
}
