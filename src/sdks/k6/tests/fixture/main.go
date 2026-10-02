// Local test application: HTTP requests trigger synthetic mail over STARTTLS.
// This process is test infrastructure, never part of the SDK or its transport.
package main

import (
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"net"
	"net/http"
	"net/smtp"
	"os"
	"strings"
	"time"
)

type message struct {
	To      string `json:"to"`
	Subject string `json:"subject"`
	Body    string `json:"body"`
	HTML    string `json:"html"`
}

func send(m message) error {
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
		return errors.New("invalid development CA")
	}
	if err := client.StartTLS(&tls.Config{RootCAs: roots, ServerName: host, MinVersion: tls.VersionTLS12}); err != nil {
		return err
	}
	if err := client.Mail("sender@example.test"); err != nil {
		return err
	}
	if err := client.Rcpt(m.To); err != nil {
		return err
	}

	writer, err := client.Data()
	if err != nil {
		return err
	}
	if m.HTML == "" {
		m.HTML = "<p>Fixture HTML</p><img src=\"https://remote.example.test/image.png\">"
	}

	_, err = fmt.Fprintf(writer, "From: sender@example.test\r\nTo: %s\r\nSubject: %s\r\nX-Flow: synthetic\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=k6-ahrara\r\n\r\n--k6-ahrara\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n%s\r\n--k6-ahrara\r\nContent-Type: text/html\r\n\r\n%s\r\n--k6-ahrara\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"fixture.bin\"\r\nContent-Transfer-Encoding: base64\r\n\r\nAQIDBA==\r\n--k6-ahrara--\r\n", m.To, m.Subject, m.Body, m.HTML)
	if err != nil {
		return err
	}
	if err := writer.Close(); err != nil {
		return err
	}

	return client.Quit()
}

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /health", func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusOK) })
	mux.HandleFunc("POST /send", func(w http.ResponseWriter, r *http.Request) {
		var m message
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 65536)).Decode(&m); err != nil {
			http.Error(w, "invalid message", http.StatusBadRequest)
			return
		}
		if !strings.HasSuffix(m.To, "@ahrara.dev") || strings.ContainsAny(m.To+m.Subject, "\r\n") {
			http.Error(w, "only synthetic development mail is accepted", http.StatusBadRequest)
			return
		}
		if err := send(m); err != nil {
			http.Error(w, "development SMTP delivery failed", http.StatusBadGateway)
			return
		}

		w.WriteHeader(http.StatusAccepted)
	})
	endpoint := os.Getenv("AHRARA_TEST_FIXTURE_ADDRESS")
	if endpoint == "" {
		endpoint = "127.0.0.1:18787"
	}

	host, _, err := net.SplitHostPort(endpoint)
	if err != nil || (host != "127.0.0.1" && host != "::1") {
		log.Fatal("fixture must bind a loopback address")
	}

	server := &http.Server{Addr: endpoint, Handler: mux, ReadHeaderTimeout: 5 * time.Second}
	log.Fatal(server.ListenAndServe())
}
