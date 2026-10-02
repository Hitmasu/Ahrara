package main

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"os"
	"os/signal"
	"sync"
	"syscall"
	"time"

	"ahrara.dev/server/internal/broker"
	"ahrara.dev/server/internal/config"
	"ahrara.dev/server/internal/protocol"
	receiver "ahrara.dev/server/internal/smtp"
	esmtp "github.com/emersion/go-smtp"
)

func main() {
	slog.SetDefault(slog.New(slog.NewJSONHandler(os.Stderr, nil)))
	if err := run(); err != nil {
		slog.Error("server_failed", "error", err.Error())
		os.Exit(1)
	}
}

func run() error {
	cfg := config.Load()
	if len(os.Args) == 2 && os.Args[1] == "healthcheck" {
		roots := x509.NewCertPool()
		pem, err := os.ReadFile(cfg.CA)
		if err != nil || !roots.AppendCertsFromPEM(pem) {
			return errors.New("invalid healthcheck CA")
		}
		client := http.Client{Timeout: 3 * time.Second, Transport: &http.Transport{TLSClientConfig: &tls.Config{RootCAs: roots, MinVersion: tls.VersionTLS12}}}
		_, port, err := net.SplitHostPort(cfg.HTTPS)
		if err != nil {
			return err
		}
		response, err := client.Get("https://localhost:" + port + "/healthz")
		if err != nil {
			return err
		}
		defer response.Body.Close()
		if response.StatusCode != http.StatusOK {
			return errors.New("unhealthy")
		}
		return nil
	}
	cert, err := tls.LoadX509KeyPair(cfg.Cert, cfg.Key)
	if err != nil {
		return err
	}
	tlsConfig := &tls.Config{Certificates: []tls.Certificate{cert}, MinVersion: tls.VersionTLS12}
	b := broker.New(cfg)
	smtp := esmtp.NewServer(&receiver.Backend{Broker: b, Config: cfg})
	smtp.Domain = "ahrara.dev"
	smtp.TLSConfig = tlsConfig
	smtp.MaxMessageBytes = protocol.MaxEmail
	// The backend limits distinct canonical recipients; repeated RCPT does not
	// consume capacity or create another delivery.
	smtp.MaxRecipients = 0
	smtp.DisableChunking = true
	smtp.AbortOnDataError = true
	smtp.ReadTimeout = 2 * time.Minute
	smtp.WriteTimeout = 30 * time.Second
	mux := http.NewServeMux()
	mux.Handle("/ws", b)
	mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		fmt.Fprintln(w, `{"status":"ok"}`)
	})
	https := &http.Server{Handler: mux, TLSConfig: tlsConfig, ReadHeaderTimeout: 10 * time.Second, IdleTimeout: 120 * time.Second, MaxHeaderBytes: 16 * 1024}
	smtpListener, err := net.Listen("tcp", cfg.SMTP)
	if err != nil {
		return err
	}
	defer smtpListener.Close()
	httpsListener, err := net.Listen("tcp", cfg.HTTPS)
	if err != nil {
		return err
	}
	defer httpsListener.Close()
	errorsCh := make(chan error, 2)
	go func() { errorsCh <- smtp.Serve(newLimitedListener(smtpListener, 128)) }()
	go func() { errorsCh <- https.ServeTLS(httpsListener, "", "") }()
	slog.Info("server_started", "smtp", cfg.SMTP, "https", cfg.HTTPS)
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	select {
	case <-ctx.Done():
	case err = <-errorsCh:
	}
	b.Close()
	_ = smtp.Close()
	shutdown, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_ = https.Shutdown(shutdown)
	if errors.Is(err, http.ErrServerClosed) || errors.Is(err, esmtp.ErrServerClosed) {
		return nil
	}
	return err
}

type limitedListener struct {
	net.Listener
	slots chan struct{}
	done  chan struct{}
}
type limitedConn struct {
	net.Conn
	release func()
}

func newLimitedListener(l net.Listener, max int) *limitedListener {
	return &limitedListener{Listener: l, slots: make(chan struct{}, max), done: make(chan struct{})}
}
func (l *limitedListener) Accept() (net.Conn, error) {
	select {
	case l.slots <- struct{}{}:
	case <-l.done:
		return nil, net.ErrClosed
	}
	c, err := l.Listener.Accept()
	if err != nil {
		<-l.slots
		return nil, err
	}
	var once sync.Once
	return &limitedConn{Conn: c, release: func() { once.Do(func() { <-l.slots }) }}, nil
}
func (l *limitedListener) Close() error {
	select {
	case <-l.done:
	default:
		close(l.done)
	}
	return l.Listener.Close()
}
func (c *limitedConn) Close() error { err := c.Conn.Close(); c.release(); return err }
