// Package broker owns authenticated connections, never an alias directory.
package broker

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"math/big"
	"net/http"
	"sync"
	"time"

	"ahrara.dev/server/internal/auth"
	"ahrara.dev/server/internal/config"
	"ahrara.dev/server/internal/protocol"
	"github.com/coder/websocket"
)

type Broker struct {
	mu          sync.RWMutex
	connections map[auth.Route]*Peer
	cfg         config.Config
	slots       chan struct{}
}

type Reply struct {
	TransferID string `json:"transfer_id"`
	Recipient  string `json:"recipient,omitempty"`
	Code       string `json:"code,omitempty"`
	Kind       byte   `json:"-"`
}

type Peer struct {
	ws           *websocket.Conn
	ctx          context.Context
	cancel       context.CancelFunc
	mu           sync.Mutex
	reservation  string
	pending      map[string]chan Reply
	writeTimeout time.Duration
}

func New(cfg config.Config) *Broker {
	return &Broker{connections: make(map[auth.Route]*Peer), cfg: cfg, slots: make(chan struct{}, 4096)}
}

func (b *Broker) Lookup(route auth.Route) *Peer {
	b.mu.RLock()
	defer b.mu.RUnlock()
	return b.connections[route]
}

func (b *Broker) Count() int { b.mu.RLock(); defer b.mu.RUnlock(); return len(b.connections) }

func decodeJSON(payload []byte, value any) error {
	d := json.NewDecoder(bytes.NewReader(payload))
	d.DisallowUnknownFields()
	if err := d.Decode(value); err != nil {
		return err
	}
	if err := d.Decode(new(any)); err != io.EOF {
		return errors.New("trailing JSON")
	}
	return nil
}

func read(ctx context.Context, ws *websocket.Conn, expected byte, value any) error {
	k, raw, err := ws.Read(ctx)
	if err != nil {
		return err
	}
	if k != websocket.MessageBinary {
		return errors.New("binary frame required")
	}
	kind, payload, err := protocol.Decode(raw)
	if err != nil || kind != expected {
		return errors.New("unexpected protocol frame")
	}
	return decodeJSON(payload, value)
}

func write(ctx context.Context, ws *websocket.Conn, kind byte, value any) error {
	frame, err := protocol.JSON(kind, value)
	if err != nil {
		return err
	}
	return ws.Write(ctx, websocket.MessageBinary, frame)
}

func (b *Broker) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	select {
	case b.slots <- struct{}{}:
		defer func() { <-b.slots }()
	default:
		http.Error(w, "capacity reached", http.StatusServiceUnavailable)
		return
	}
	// This is a native-client endpoint. Browser origins are not part of v1.
	if r.Header.Get("Origin") != "" {
		http.Error(w, "origin not allowed", http.StatusForbidden)
		return
	}
	ws, err := websocket.Accept(w, r, &websocket.AcceptOptions{CompressionMode: websocket.CompressionDisabled})
	if err != nil {
		return
	}
	defer ws.CloseNow()
	ws.SetReadLimit(protocol.MaxControl + 6)
	ctx, cancel := context.WithTimeout(r.Context(), b.cfg.AuthTimeout)
	defer cancel()
	fail := func(code string) { _ = write(ctx, ws, protocol.Failure, map[string]string{"code": code}) }
	var hello struct {
		PublicKey string `json:"public_key"`
	}
	if read(ctx, ws, protocol.Hello, &hello) != nil {
		fail("invalid_hello")
		return
	}
	public, err := base64.RawURLEncoding.Strict().DecodeString(hello.PublicKey)
	if err != nil || len(public) != ed25519.PublicKeySize {
		fail("invalid_public_key")
		return
	}
	challenge := make([]byte, 32)
	if _, err := rand.Read(challenge); err != nil {
		return
	}
	if write(ctx, ws, protocol.Challenge, map[string]string{"challenge": base64.RawURLEncoding.EncodeToString(challenge)}) != nil {
		return
	}
	var signed struct {
		Signature string `json:"signature"`
	}
	if read(ctx, ws, protocol.Authenticate, &signed) != nil {
		fail("authentication_failed")
		return
	}
	signature, err := base64.RawURLEncoding.Strict().DecodeString(signed.Signature)
	if err != nil || !ed25519.Verify(public, auth.Preimage(public, challenge), signature) {
		fail("authentication_failed")
		return
	}
	// Derive the registry key only after proof of possession succeeds.
	route := auth.Derive(public)
	peerCtx, peerCancel := context.WithCancel(r.Context())
	p := &Peer{ws: ws, ctx: peerCtx, cancel: peerCancel, pending: make(map[string]chan Reply), writeTimeout: b.cfg.WriteTimeout}
	defer peerCancel()
	b.mu.Lock()
	if b.connections[route] != nil {
		b.mu.Unlock()
		fail("identity_in_use")
		return
	}
	b.connections[route] = p
	b.mu.Unlock()
	defer func() {
		b.mu.Lock()
		if b.connections[route] == p {
			delete(b.connections, route)
		}
		b.mu.Unlock()
	}()
	fingerprint := sha256.Sum256(public)
	if write(ctx, ws, protocol.Ready, map[string]string{"route_id": hex.EncodeToString(route[:]), "fingerprint": hex.EncodeToString(fingerprint[:])}) != nil {
		return
	}
	cancel()
	slog.Info("identity_connected")
	defer slog.Info("identity_disconnected")
	go p.heartbeat(b.cfg.Heartbeat)
	p.readLoop()
}

func (p *Peer) Reserve(id string) bool {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ctx.Err() != nil || (p.reservation != "" && p.reservation != id) {
		return false
	}
	p.reservation = id
	return true
}

func (p *Peer) Release(id string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.reservation == id {
		p.reservation = ""
	}
}

func (p *Peer) Send(ctx context.Context, kind byte, value any) error {
	ctx, cancel := context.WithTimeout(ctx, p.writeTimeout)
	defer cancel()
	return write(ctx, p.ws, kind, value)
}

func (p *Peer) SendFrame(ctx context.Context, frame []byte) error {
	ctx, cancel := context.WithTimeout(ctx, p.writeTimeout)
	defer cancel()
	return p.ws.Write(ctx, websocket.MessageBinary, frame)
}

// Ask registers the waiter before writing, so an immediate response cannot be lost.
func (p *Peer) Ask(ctx context.Context, kind byte, value any, transfer, recipient string) (Reply, error) {
	key := transfer + ":" + recipient
	ch := make(chan Reply, 1)
	p.mu.Lock()
	p.pending[key] = ch
	p.mu.Unlock()
	defer func() { p.mu.Lock(); delete(p.pending, key); p.mu.Unlock() }()
	if err := p.Send(ctx, kind, value); err != nil {
		return Reply{}, err
	}
	select {
	case reply := <-ch:
		if err := ctx.Err(); err != nil {
			return Reply{}, err
		}
		if err := p.ctx.Err(); err != nil {
			return Reply{}, err
		}
		return reply, nil
	case <-ctx.Done():
		return Reply{}, ctx.Err()
	case <-p.ctx.Done():
		return Reply{}, errors.New("client disconnected")
	}
}

func (p *Peer) readLoop() {
	for {
		t, raw, err := p.ws.Read(p.ctx)
		if err != nil || t != websocket.MessageBinary {
			return
		}
		kind, payload, err := protocol.Decode(raw)
		if err != nil || (kind != protocol.Accept && kind != protocol.Reject && kind != protocol.Ack && kind != protocol.Nack) {
			return
		}
		var reply Reply
		if decodeJSON(payload, &reply) != nil {
			return
		}
		reply.Kind = kind
		p.mu.Lock()
		ch := p.pending[reply.TransferID+":"+reply.Recipient]
		if ch != nil {
			select {
			case ch <- reply:
			default:
			}
		}
		p.mu.Unlock()
	}
}

func (p *Peer) heartbeat(interval time.Duration) {
	for {
		n, err := rand.Int(rand.Reader, big.NewInt(401))
		if err != nil {
			p.cancel()
			return
		}
		delay := interval * time.Duration(800+n.Int64()) / 1000
		timer := time.NewTimer(delay)
		select {
		case <-p.ctx.Done():
			timer.Stop()
			return
		case <-timer.C:
		}
		ctx, cancel := context.WithTimeout(p.ctx, p.writeTimeout)
		err = p.ws.Ping(ctx)
		cancel()
		if err != nil {
			p.cancel()
			return
		}
	}
}

func (b *Broker) Close() {
	b.mu.RLock()
	defer b.mu.RUnlock()
	for _, p := range b.connections {
		p.cancel()
		_ = p.ws.CloseNow()
	}
}
