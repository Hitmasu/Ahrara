package smtp

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"io"
	"log/slog"
	"sync"
	"time"

	"ahrara.dev/server/internal/auth"
	"ahrara.dev/server/internal/broker"
	"ahrara.dev/server/internal/config"
	"ahrara.dev/server/internal/protocol"
	esmtp "github.com/emersion/go-smtp"
)

type Backend struct {
	Broker *broker.Broker
	Config config.Config
}

type session struct {
	mu         sync.Mutex // go-smtp may invoke Logout concurrently during server shutdown.
	backend    *Backend
	conn       *esmtp.Conn
	transfer   [16]byte
	id, sender string
	size       *int64
	peers      map[*broker.Peer][]string
	recipients map[string]bool
	offered    map[string]bool
	ctx        context.Context
	cancel     context.CancelFunc
}

func smtpError(code int, enhanced esmtp.EnhancedCode, message string) error {
	return &esmtp.SMTPError{Code: code, EnhancedCode: enhanced, Message: message}
}

func (b *Backend) NewSession(c *esmtp.Conn) (esmtp.Session, error) {
	return &session{backend: b, conn: c}, nil
}
func (s *session) Logout() error { s.Reset(); return nil }
func (s *session) Reset() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.resetLocked()
}
func (s *session) resetLocked() {
	if s.cancel != nil {
		s.cancel()
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	for peer := range s.peers {
		_ = peer.Send(ctx, protocol.Abort, map[string]string{"transfer_id": s.id})
		peer.Release(s.id)
	}
	s.peers = nil
	s.recipients = nil
	s.offered = nil
	s.id = ""
	s.cancel = nil
}

func (s *session) requireTLS() error {
	if _, ok := s.conn.TLSConnectionState(); !ok {
		return smtpError(530, esmtp.EnhancedCode{5, 7, 0}, "Must issue STARTTLS first")
	}
	return nil
}

func (s *session) Mail(from string, opts *esmtp.MailOptions) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.requireTLS(); err != nil {
		return err
	}
	s.resetLocked()
	if opts.Size > protocol.MaxEmail {
		return esmtp.ErrDataTooLarge
	}
	if _, err := rand.Read(s.transfer[:]); err != nil {
		return smtpError(451, esmtp.EnhancedCode{4, 3, 0}, "Temporary failure")
	}
	s.id = hex.EncodeToString(s.transfer[:])
	s.sender = from
	s.size = nil
	if opts.Size > 0 {
		value := opts.Size
		s.size = &value
	}
	s.peers = make(map[*broker.Peer][]string)
	s.recipients = make(map[string]bool)
	s.offered = make(map[string]bool)
	s.ctx, s.cancel = context.WithTimeout(context.Background(), 5*time.Minute)
	return nil
}

func (s *session) Rcpt(to string, _ *esmtp.RcptOptions) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.requireTLS(); err != nil {
		return err
	}
	route, alias, recipient, err := auth.DecodeAddress(to)
	unavailable := func() error { return smtpError(550, esmtp.EnhancedCode{5, 1, 1}, "Mailbox unavailable") }
	if err != nil {
		return unavailable()
	}
	if s.recipients[recipient] {
		return nil
	}
	// Never reuse an expired/rejected offer's correlation key. The sender may
	// retry after RSET + MAIL, which creates a fresh transfer ID.
	if s.offered[recipient] {
		return smtpError(451, esmtp.EnhancedCode{4, 4, 2}, "Start a new transaction to retry this recipient")
	}
	if len(s.offered) >= 100 {
		return smtpError(452, esmtp.EnhancedCode{4, 5, 3}, "Too many distinct recipients")
	}
	peer := s.backend.Broker.Lookup(route)
	if peer == nil {
		return unavailable()
	}
	if !peer.Reserve(s.id) {
		return smtpError(450, esmtp.EnhancedCode{4, 2, 1}, "Identity busy")
	}
	s.offered[recipient] = true
	ctx, cancel := context.WithTimeout(s.ctx, s.backend.Config.OfferTimeout)
	defer cancel()
	reply, err := peer.Ask(ctx, protocol.Offer, map[string]any{
		"transfer_id": s.id, "recipient": recipient, "alias_id": hex.EncodeToString(alias[:]),
		"envelope_sender": s.sender, "declared_size": s.size,
	}, s.id, recipient)
	if err != nil || reply.Kind != protocol.Accept {
		if len(s.peers[peer]) == 0 {
			abortCtx, abortCancel := context.WithTimeout(context.Background(), time.Second)
			_ = peer.Send(abortCtx, protocol.Abort, map[string]string{"transfer_id": s.id})
			abortCancel()
			peer.Release(s.id)
		}
		if err != nil {
			return smtpError(451, esmtp.EnhancedCode{4, 4, 2}, "Delivery offer unavailable")
		}
		return unavailable()
	}
	s.peers[peer] = append(s.peers[peer], recipient)
	s.recipients[recipient] = true
	return nil
}

func (s *session) Data(reader io.Reader) error {
	if err := s.requireTLS(); err != nil {
		return err
	}
	// Retain immutable transaction state while Logout can cancel it. Never hold
	// the session mutex during a blocking DATA read; Close calls Logout first.
	s.mu.Lock()
	txnCtx, transfer, id, sender, size := s.ctx, s.transfer, s.id, s.sender, s.size
	peers, recipientCount := s.peers, len(s.recipients)
	s.mu.Unlock()
	defer s.Reset() // Release before go-smtp writes the final SMTP response.
	failure := func() error { return smtpError(451, esmtp.EnhancedCode{4, 4, 2}, "Local delivery not confirmed") }
	for peer, recipients := range peers {
		if err := peer.Send(txnCtx, protocol.Start, map[string]any{"transfer_id": id, "recipients": recipients, "envelope_sender": sender, "declared_size": size}); err != nil {
			return failure()
		}
	}
	// One reusable frame for the transaction, regardless of EML size or aliases.
	frame := make([]byte, protocol.MaxFrame)
	frame[0], frame[1] = protocol.Version, protocol.Chunk
	copy(frame[6:22], transfer[:])
	digest := sha256.New()
	var total int64
	var sequence uint32
	for {
		if txnCtx.Err() != nil {
			return failure()
		}
		n, err := reader.Read(frame[26:])
		if n > 0 {
			total += int64(n)
			if total > protocol.MaxEmail {
				return esmtp.ErrDataTooLarge
			}
			digest.Write(frame[26 : 26+n])
			binary.BigEndian.PutUint32(frame[2:6], uint32(20+n))
			binary.BigEndian.PutUint32(frame[22:26], sequence)
			ctx, cancel := context.WithTimeout(txnCtx, s.backend.Config.WriteTimeout)
			for peer := range peers {
				if sendErr := peer.SendFrame(ctx, frame[:26+n]); sendErr != nil {
					cancel()
					return failure()
				}
			}
			cancel()
			sequence++
		}
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			if errors.Is(err, esmtp.ErrDataTooLarge) {
				return esmtp.ErrDataTooLarge
			}
			return failure()
		}
	}
	end := map[string]any{"transfer_id": id, "total_bytes": total, "chunks": sequence, "sha256": hex.EncodeToString(digest.Sum(nil))}
	// Start all bounded ACK waiters together; one slow client cannot multiply the deadline.
	ctx, cancel := context.WithTimeout(txnCtx, s.backend.Config.WriteTimeout)
	defer cancel()
	results := make(chan bool, len(peers))
	for peer := range peers {
		go func(p *broker.Peer) {
			reply, err := p.Ask(ctx, protocol.End, end, id, "")
			results <- err == nil && reply.Kind == protocol.Ack
		}(peer)
	}
	for range peers {
		if !<-results {
			return failure()
		}
	}
	slog.Info("delivery_committed", "bytes", total, "recipients", recipientCount, "connections", len(peers))
	return nil
}
