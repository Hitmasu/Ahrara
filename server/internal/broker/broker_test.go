package broker

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"net/http/httptest"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"ahrara.dev/server/internal/auth"
	"ahrara.dev/server/internal/config"
	"ahrara.dev/server/internal/protocol"
	"github.com/coder/websocket"
)

func TestSlowClientWriteDeadline(t *testing.T) {
	b, dial := testBroker(t)
	pub, private, _ := ed25519.GenerateKey(rand.Reader)
	ws := dial()
	authenticateTest(t, ws, pub, private)
	p := b.Lookup(auth.Derive(pub))
	frame, err := protocol.Encode(protocol.Chunk, make([]byte, 20+protocol.ChunkSize))
	if err != nil {
		t.Fatal(err)
	}
	// Stop reading after auth. Only one bounded frame is allocated/reused while
	// socket buffers fill; a slow consumer must eventually stop the producer.
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	for range 1024 {
		if err = p.SendFrame(ctx, frame); err != nil {
			return
		}
	}
	t.Fatal("slow peer did not apply backpressure before 64 MiB")
}

func TestHeartbeatCancellationWithVirtualClock(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		p := &Peer{ctx: ctx, cancel: cancel}
		done := make(chan struct{})
		go func() { defer close(done); p.heartbeat(90 * time.Second) }()
		synctest.Wait()
		time.Sleep(60 * time.Second) // Before the earliest jittered ping (72s).
		select {
		case <-done:
			t.Fatal("heartbeat stopped an idle session")
		default:
		}
		cancel()
		synctest.Wait()
		select {
		case <-done:
		default:
			t.Fatal("heartbeat leaked after cancellation")
		}
	})
}

func testBroker(t *testing.T) (*Broker, func() *websocket.Conn) {
	t.Helper()
	cfg := config.Load()
	cfg.AuthTimeout = 300 * time.Millisecond
	cfg.Heartbeat = 20 * time.Millisecond
	cfg.WriteTimeout = 200 * time.Millisecond
	b := New(cfg)
	server := httptest.NewTLSServer(b)
	t.Cleanup(func() { b.Close(); server.Close() })
	return b, func() *websocket.Conn {
		t.Helper()
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		ws, _, err := websocket.Dial(ctx, "wss"+strings.TrimPrefix(server.URL, "https"), &websocket.DialOptions{HTTPClient: server.Client()})
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { ws.CloseNow() })
		return ws
	}
}
func sendTest(t *testing.T, ws *websocket.Conn, kind byte, value any) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if err := write(ctx, ws, kind, value); err != nil {
		t.Fatal(err)
	}
}
func receiveTest(t *testing.T, ws *websocket.Conn) (byte, map[string]string) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	_, raw, err := ws.Read(ctx)
	if err != nil {
		t.Fatal(err)
	}
	kind, payload, err := protocol.Decode(raw)
	if err != nil {
		t.Fatal(err)
	}
	var v map[string]string
	if err = json.Unmarshal(payload, &v); err != nil {
		t.Fatal(err)
	}
	return kind, v
}
func challengeTest(t *testing.T, ws *websocket.Conn, pub ed25519.PublicKey) []byte {
	t.Helper()
	sendTest(t, ws, protocol.Hello, map[string]string{"public_key": base64.RawURLEncoding.EncodeToString(pub)})
	kind, v := receiveTest(t, ws)
	if kind != protocol.Challenge {
		t.Fatal("no challenge")
	}
	challenge, err := base64.RawURLEncoding.DecodeString(v["challenge"])
	if err != nil || len(challenge) != 32 {
		t.Fatal("invalid challenge")
	}
	return challenge
}
func authenticateTest(t *testing.T, ws *websocket.Conn, pub ed25519.PublicKey, private ed25519.PrivateKey) (byte, map[string]string) {
	t.Helper()
	c := challengeTest(t, ws, pub)
	sendTest(t, ws, protocol.Authenticate, map[string]string{"signature": base64.RawURLEncoding.EncodeToString(ed25519.Sign(private, auth.Preimage(pub, c)))})
	return receiveTest(t, ws)
}

func TestProofOfPossessionDuplicateAndRoute(t *testing.T) {
	b, dial := testBroker(t)
	pub, private, _ := ed25519.GenerateKey(rand.Reader)
	first := dial()
	kind, v := authenticateTest(t, first, pub, private)
	route := auth.Derive(pub)
	if kind != protocol.Ready || v["route_id"] != hex.EncodeToString(route[:]) || b.Count() != 1 {
		t.Fatal("authenticated route not registered")
	}
	second := dial()
	kind, v = authenticateTest(t, second, pub, private)
	if kind != protocol.Failure || v["code"] != "identity_in_use" || b.Count() != 1 {
		t.Fatal("second connection took over")
	}
	third := dial()
	sendTest(t, third, protocol.Hello, map[string]string{"public_key": base64.RawURLEncoding.EncodeToString(pub), "route_id": hex.EncodeToString(route[:])})
	kind, _ = receiveTest(t, third)
	if kind != protocol.Failure {
		t.Fatal("arbitrary route accepted")
	}
	fourth := dial()
	challengeTest(t, fourth, pub)
	sendTest(t, fourth, protocol.Authenticate, map[string]string{"signature": base64.RawURLEncoding.EncodeToString(make([]byte, 64))})
	kind, _ = receiveTest(t, fourth)
	if kind != protocol.Failure || b.Count() != 1 {
		t.Fatal("public key alone authenticated")
	}
	// Read frames while heartbeat pings are answered by the WebSocket library.
	readCtx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() { defer close(done); first.Read(readCtx) }()
	time.Sleep(80 * time.Millisecond)
	if b.Count() != 1 {
		t.Fatal("idle identity disconnected")
	}
	cancel()
	<-done
	first.CloseNow()
	until := time.Now().Add(time.Second)
	for b.Count() != 0 && time.Now().Before(until) {
		time.Sleep(time.Millisecond)
	}
	if b.Count() != 0 {
		t.Fatal("disconnected identity leaked")
	}
	replacement := dial()
	kind, _ = authenticateTest(t, replacement, pub, private)
	if kind != protocol.Ready {
		t.Fatal("cannot reconnect")
	}
}

func TestChallengeExpiryAndReplay(t *testing.T) {
	b, dial := testBroker(t)
	pub, private, _ := ed25519.GenerateKey(rand.Reader)
	expired := dial()
	old := challengeTest(t, expired, pub)
	time.Sleep(350 * time.Millisecond)
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	_, _, err := expired.Read(ctx)
	if err == nil || b.Count() != 0 {
		t.Fatal("expired challenge accepted")
	}
	replay := dial()
	challengeTest(t, replay, pub)
	sendTest(t, replay, protocol.Authenticate, map[string]string{"signature": base64.RawURLEncoding.EncodeToString(ed25519.Sign(private, auth.Preimage(pub, old)))})
	kind, _ := receiveTest(t, replay)
	if kind != protocol.Failure || b.Count() != 0 {
		t.Fatal("challenge replay accepted")
	}
}

func TestReservationAndLateReplies(t *testing.T) {
	b, dial := testBroker(t)
	pub, private, _ := ed25519.GenerateKey(rand.Reader)
	ws := dial()
	authenticateTest(t, ws, pub, private)
	p := b.Lookup(auth.Derive(pub))
	if !p.Reserve("one") || !p.Reserve("one") || p.Reserve("two") {
		t.Fatal("reservation semantics")
	}
	p.Release("two")
	if p.Reserve("two") {
		t.Fatal("wrong owner released reservation")
	}
	p.Release("one")
	if !p.Reserve("two") {
		t.Fatal("release failed")
	}
	// A response without a pending offer cannot create authorization/state.
	sendTest(t, ws, protocol.Accept, Reply{TransferID: "expired", Recipient: "unknown@ahrara.dev"})
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	done := make(chan error, 1)
	go func() {
		_, err := p.Ask(ctx, protocol.Offer, map[string]string{"transfer_id": "new", "recipient": "local"}, "new", "local")
		done <- err
	}()
	kind, _ := receiveTest(t, ws)
	if kind != protocol.Offer {
		t.Fatal("missing offer")
	}
	if <-done == nil {
		t.Fatal("offer without matching reply accepted")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if len(p.pending) != 0 {
		t.Fatal("pending offer leaked")
	}
}
