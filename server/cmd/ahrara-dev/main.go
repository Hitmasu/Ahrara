// Development-only certificates and SMTP sender; not part of the end-user CLI.
package main

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"flag"
	"fmt"
	"math/big"
	"net"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/emersion/go-smtp"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
func run() error {
	if len(os.Args) < 2 {
		return fmt.Errorf("usage: ahrara-dev certs|send")
	}
	switch os.Args[1] {
	case "certs":
		f := flag.NewFlagSet("certs", flag.ExitOnError)
		dir := f.String("dir", ".dev/certs", "certificate directory")
		port := f.Int("port", 8443, "local WSS port")
		profile := f.String("profile", ".dev/profile", "local profile")
		f.Parse(os.Args[2:])
		if err := certificates(*dir); err != nil {
			return err
		}
		abs, err := filepath.Abs(*dir)
		if err != nil {
			return err
		}
		p, err := filepath.Abs(*profile)
		if err != nil {
			return err
		}
		if err = os.MkdirAll(p, 0700); err != nil {
			return err
		}
		configPath := filepath.Join(p, "config.toml")
		if _, err = os.Stat(configPath); os.IsNotExist(err) {
			text := fmt.Sprintf("server_url = %q\nsmtp_dev_address = %q\ndomain = %q\ntls_ca_path = %q\ndatabase_path = %q\nidentity_path = %q\napi_bind = %q\napi_tls_cert_path = %q\napi_tls_key_path = %q\napi_token_path = %q\n", fmt.Sprintf("wss://localhost:%d/ws", *port), "localhost:2525", "ahrara.dev", filepath.Join(abs, "ca.pem"), filepath.Join(p, "ahrara.db"), filepath.Join(p, "identity.key"), "127.0.0.1:8787", filepath.Join(abs, "server.pem"), filepath.Join(abs, "server-key.pem"), filepath.Join(p, "api.token"))
			if err = os.WriteFile(configPath, []byte(text), 0600); err != nil {
				return err
			}
		}
		fmt.Println("Development certificates and local configuration ready.")
		return nil
	case "send":
		f := flag.NewFlagSet("send", flag.ExitOnError)
		addr := f.String("addr", "localhost:2525", "SMTP endpoint")
		ca := f.String("ca", ".dev/certs/ca.pem", "CA file")
		from := f.String("from", "sender@example.test", "envelope sender")
		to := f.String("to", "", "comma-separated recipients")
		file := f.String("file", "", "EML file")
		declared := f.Int64("size", 0, "declared SIZE (0 omits)")
		f.Parse(os.Args[2:])
		roots := x509.NewCertPool()
		bytes, err := os.ReadFile(*ca)
		if err != nil {
			return err
		}
		if !roots.AppendCertsFromPEM(bytes) {
			return fmt.Errorf("invalid CA")
		}
		host, _, err := net.SplitHostPort(*addr)
		if err != nil {
			return err
		}
		conn, err := net.DialTimeout("tcp", *addr, 10*time.Second)
		if err != nil {
			return err
		}
		defer conn.Close()
		conn.SetDeadline(time.Now().Add(90 * time.Second))
		client, err := smtp.NewClientStartTLS(conn, &tls.Config{RootCAs: roots, ServerName: host, MinVersion: tls.VersionTLS12})
		if err != nil {
			return err
		}
		defer client.Close()
		if *to == "" || *file == "" {
			return fmt.Errorf("--to and --file are required")
		}
		if err = client.Mail(*from, &smtp.MailOptions{Size: *declared}); err != nil {
			return err
		}
		for _, recipient := range strings.Split(*to, ",") {
			if err = client.Rcpt(recipient, nil); err != nil {
				return err
			}
		}
		writer, err := client.Data()
		if err != nil {
			return err
		}
		raw, err := os.ReadFile(*file)
		if err != nil {
			return err
		}
		if _, err = writer.Write(raw); err != nil {
			return err
		}
		if err = writer.Close(); err != nil {
			return err
		}
		return client.Quit()
	default:
		return fmt.Errorf("unknown development command")
	}
}

func certificates(dir string) error {
	if err := os.MkdirAll(dir, 0700); err != nil {
		return err
	}
	files := []string{"ca.pem", "ca-key.pem", "server.pem", "server-key.pem"}
	existing := 0
	for _, name := range files {
		if _, err := os.Stat(filepath.Join(dir, name)); err == nil {
			existing++
		}
	}
	if existing == len(files) {
		return nil
	}
	if existing != 0 {
		return fmt.Errorf("partial certificate set; refusing to replace keys")
	}
	caKey, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return err
	}
	serverKey, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return err
	}
	serial := func() *big.Int { n, _ := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 128)); return n }
	ca := &x509.Certificate{SerialNumber: serial(), Subject: pkix.Name{CommonName: "Ahrara development CA"}, NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().AddDate(2, 0, 0), IsCA: true, BasicConstraintsValid: true, KeyUsage: x509.KeyUsageCertSign | x509.KeyUsageCRLSign}
	server := &x509.Certificate{SerialNumber: serial(), Subject: pkix.Name{CommonName: "localhost"}, NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().AddDate(1, 0, 0), DNSNames: []string{"localhost"}, IPAddresses: []net.IP{net.ParseIP("127.0.0.1"), net.ParseIP("::1")}, KeyUsage: x509.KeyUsageDigitalSignature, ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth}}
	caDER, err := x509.CreateCertificate(rand.Reader, ca, ca, &caKey.PublicKey, caKey)
	if err != nil {
		return err
	}
	serverDER, err := x509.CreateCertificate(rand.Reader, server, ca, &serverKey.PublicKey, caKey)
	if err != nil {
		return err
	}
	caPrivate, err := x509.MarshalPKCS8PrivateKey(caKey)
	if err != nil {
		return err
	}
	serverPrivate, err := x509.MarshalPKCS8PrivateKey(serverKey)
	if err != nil {
		return err
	}
	for _, item := range []struct {
		name, kind string
		bytes      []byte
		mode       os.FileMode
	}{{"ca.pem", "CERTIFICATE", caDER, 0644}, {"server.pem", "CERTIFICATE", serverDER, 0644}, {"ca-key.pem", "PRIVATE KEY", caPrivate, 0600}, {"server-key.pem", "PRIVATE KEY", serverPrivate, 0600}} {
		f, err := os.OpenFile(filepath.Join(dir, item.name), os.O_CREATE|os.O_EXCL|os.O_WRONLY, item.mode)
		if err != nil {
			return err
		}
		err = pem.Encode(f, &pem.Block{Type: item.kind, Bytes: item.bytes})
		closeErr := f.Close()
		if err != nil {
			return err
		}
		if closeErr != nil {
			return closeErr
		}
	}
	return nil
}
