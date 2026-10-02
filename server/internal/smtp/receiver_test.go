package smtp

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"ahrara.dev/server/internal/auth"
	"ahrara.dev/server/internal/broker"
	"ahrara.dev/server/internal/config"
	"ahrara.dev/server/internal/protocol"
	"github.com/coder/websocket"
	esmtp "github.com/emersion/go-smtp"
)

type recorded struct {
	mu          sync.Mutex
	starts      int
	recipients  []string
	raw         []byte
	badChecksum bool
	offers      map[string]int
}

func TestSMTPRoutingOffersAndPartialFailure(t *testing.T) {
	cfg := config.Load()
	cfg.OfferTimeout = 100 * time.Millisecond
	cfg.WriteTimeout = 300 * time.Millisecond
	b := broker.New(cfg)
	https := httptest.NewTLSServer(b)
	defer https.Close()
	defer b.Close()
	roots := x509.NewCertPool()
	roots.AddCert(https.Certificate())
	server := esmtp.NewServer(&Backend{Broker: b, Config: cfg})
	server.Domain = "ahrara.dev"
	server.TLSConfig = https.TLS
	server.MaxMessageBytes = protocol.MaxEmail
	server.AbortOnDataError = true
	server.DisableChunking = true
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer server.Close()
	go server.Serve(listener)
	smtpClient := func(tlsEnabled bool) *esmtp.Client {
		t.Helper()
		conn, err := net.Dial("tcp", listener.Addr().String())
		if err != nil {
			t.Fatal(err)
		}
		conn.SetDeadline(time.Now().Add(5 * time.Second))
		var c *esmtp.Client
		if tlsEnabled {
			c, err = esmtp.NewClientStartTLS(conn, &tls.Config{RootCAs: roots, ServerName: "example.com", MinVersion: tls.VersionTLS12})
		} else {
			c = esmtp.NewClient(conn)
		}
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { c.Close() })
		return c
	}
	status := func(err error, code int) {
		t.Helper()
		var smtpErr *esmtp.SMTPError
		if !errors.As(err, &smtpErr) || smtpErr.Code != code {
			t.Fatalf("want SMTP %d, got %v", code, err)
		}
	}
	plain := smtpClient(false)
	status(plain.Mail("sender@example.test", nil), 530)
	connect := func(nack bool) ([]string, *recorded, *websocket.Conn) {
		t.Helper()
		ctx := context.Background()
		ws, _, err := websocket.Dial(ctx, "wss"+strings.TrimPrefix(https.URL, "https"), &websocket.DialOptions{HTTPClient: https.Client()})
		if err != nil {
			t.Fatal(err)
		}
		ws.SetReadLimit(protocol.MaxFrame)
		t.Cleanup(func() { ws.CloseNow() })
		send := func(kind byte, v any) error {
			raw, e := protocol.JSON(kind, v)
			if e != nil {
				return e
			}
			return ws.Write(ctx, websocket.MessageBinary, raw)
		}
		pub, private, _ := ed25519.GenerateKey(rand.Reader)
		send(protocol.Hello, map[string]string{"public_key": base64.RawURLEncoding.EncodeToString(pub)})
		_, frame, err := ws.Read(ctx)
		if err != nil {
			t.Fatal(err)
		}
		_, payload, _ := protocol.Decode(frame)
		var challenge map[string]string
		json.Unmarshal(payload, &challenge)
		ch, _ := base64.RawURLEncoding.DecodeString(challenge["challenge"])
		send(protocol.Authenticate, map[string]string{"signature": base64.RawURLEncoding.EncodeToString(ed25519.Sign(private, auth.Preimage(pub, ch)))})
		_, frame, err = ws.Read(ctx)
		if err != nil {
			t.Fatal(err)
		}
		kind, _, _ := protocol.Decode(frame)
		if kind != protocol.Ready {
			t.Fatal("auth failed")
		}
		route := auth.Derive(pub)
		addresses := make([]string, 5)
		for i := range addresses {
			alias := make([]byte, 8)
			alias[0] = byte(i)
			addresses[i] = base64.RawURLEncoding.EncodeToString(append(route[:], alias...)) + "@ahrara.dev"
		}
		rec := &recorded{offers: make(map[string]int)}
		go func() {
			for {
				_, frame, err := ws.Read(ctx)
				if err != nil {
					return
				}
				kind, payload, err := protocol.Decode(frame)
				if err != nil {
					return
				}
				var v struct {
					TransferID string   `json:"transfer_id"`
					Recipient  string   `json:"recipient"`
					Recipients []string `json:"recipients"`
					SHA256     string   `json:"sha256"`
				}
				if kind != protocol.Chunk {
					json.Unmarshal(payload, &v)
				}
				switch kind {
				case protocol.Offer:
					rec.mu.Lock()
					rec.offers[v.Recipient]++
					rec.mu.Unlock()
					if v.Recipient == addresses[3] {
						continue
					}
					if v.Recipient == addresses[4] {
						ws.CloseNow()
						return
					}
					reply := protocol.Reject
					if v.Recipient == addresses[0] || v.Recipient == addresses[1] {
						reply = protocol.Accept
					}
					send(reply, broker.Reply{TransferID: v.TransferID, Recipient: v.Recipient})
				case protocol.Start:
					rec.mu.Lock()
					rec.starts++
					rec.recipients = v.Recipients
					rec.raw = nil
					rec.mu.Unlock()
				case protocol.Chunk:
					rec.mu.Lock()
					rec.raw = append(rec.raw, payload[20:]...)
					rec.mu.Unlock()
				case protocol.End:
					rec.mu.Lock()
					hash := sha256.Sum256(rec.raw)
					rec.badChecksum = hex.EncodeToString(hash[:]) != v.SHA256
					rec.mu.Unlock()
					reply := protocol.Ack
					if nack {
						reply = protocol.Nack
					}
					send(reply, broker.Reply{TransferID: v.TransferID})
				}
			}
		}()
		return addresses, rec, ws
	}
	a, one, _ := connect(false)
	other, two, _ := connect(false)
	c := smtpClient(true)
	if err = c.Mail("sender@example.test", nil); err != nil {
		t.Fatal(err)
	}
	status(c.Rcpt("invalid@ahrara.dev", nil), 550)
	status(c.Rcpt(a[2], nil), 550)
	status(c.Rcpt(a[3], nil), 451) // Route known, offer timed out.
	status(c.Rcpt(a[3], nil), 451)
	one.mu.Lock()
	attempts := one.offers[a[3]]
	one.mu.Unlock()
	if attempts != 1 {
		t.Fatal("failed offer correlation key was reused in the same transaction")
	}
	for _, to := range []string{a[0], a[1], a[0], other[0]} {
		if err = c.Rcpt(to, nil); err != nil {
			t.Fatal(err)
		}
	}
	competing := smtpClient(true)
	competing.Mail("other@example.test", nil)
	status(competing.Rcpt(a[1], nil), 450)
	raw := []byte("From: sender@example.test\r\nTo: undisclosed-recipients:;\r\nSubject: BCC\r\n\r\n.first\r\n..second\r\n")
	w, err := c.Data()
	if err != nil {
		t.Fatal(err)
	}
	if _, err = w.Write(raw); err != nil {
		t.Fatal(err)
	}
	if err = w.Close(); err != nil {
		t.Fatal(err)
	}
	if b.Count() != 2 {
		t.Fatal("expected one WSS per Identity")
	}
	for i, rec := range []*recorded{one, two} {
		rec.mu.Lock()
		if rec.starts != 1 || !bytes.Equal(rec.raw, raw) || rec.badChecksum {
			t.Error("stream grouping or bytes changed")
		}
		expected := 2 - i
		if len(rec.recipients) != expected {
			t.Error("recipients not grouped/deduplicated")
		}
		for _, recipient := range rec.recipients {
			route, _, _, _ := auth.DecodeAddress(recipient)
			expectedRoute, _, _, _ := auth.DecodeAddress([]string{a[0], other[0]}[i])
			if route != expectedRoute {
				t.Error("BCC leaked across identities")
			}
		}
		rec.mu.Unlock()
	}
	// RSET frees the Identity and cancels offers before DATA.
	if err = competing.Rcpt(a[0], nil); err != nil {
		t.Fatal(err)
	}
	if err = competing.Reset(); err != nil {
		t.Fatal(err)
	}
	c.Mail("sender@example.test", nil)
	status(c.Rcpt(a[4], nil), 451)
	bad, _, _ := connect(true)
	failure := smtpClient(true)
	failure.Mail("sender@example.test", nil)
	failure.Rcpt(other[0], nil)
	failure.Rcpt(bad[0], nil)
	writer, _ := failure.Data()
	writer.Write(raw)
	status(writer.Close(), 451)
	// Closing the server during an ordinary, incomplete DATA transfer must
	// release the session safely while its receive goroutine is still active.
	interrupted := smtpClient(true)
	if err = interrupted.Mail("sender@example.test", nil); err != nil {
		t.Fatal(err)
	}
	if err = interrupted.Rcpt(other[0], nil); err != nil {
		t.Fatal(err)
	}
	partial, err := interrupted.Data()
	if err != nil {
		t.Fatal(err)
	}
	writing := make(chan struct{})
	go func() {
		defer close(writing)
		partial.Write(bytes.Repeat([]byte("body\r\n"), 1500000))
		partial.Close()
	}()
	until := time.Now().Add(time.Second)
	for {
		two.mu.Lock()
		received := len(two.raw)
		two.mu.Unlock()
		if received > 65536 {
			break
		}
		if time.Now().After(until) {
			t.Fatal("DATA did not begin")
		}
		time.Sleep(time.Millisecond)
	}
	if err = server.Close(); err != nil {
		t.Fatal(err)
	}
	select {
	case <-writing:
	case <-time.After(2 * time.Second):
		t.Fatal("writer did not unblock on shutdown")
	}
}
