package config

import (
	"os"
	"time"
)

type Config struct {
	SMTP, HTTPS, Cert, Key, CA                         string
	OfferTimeout, WriteTimeout, AuthTimeout, Heartbeat time.Duration
}

func Load() Config {
	get := func(k, d string) string {
		if v := os.Getenv(k); v != "" {
			return v
		}
		return d
	}
	return Config{
		SMTP: get("AHRARA_SMTP_BIND", ":2525"), HTTPS: get("AHRARA_HTTPS_BIND", ":8443"),
		Cert: get("AHRARA_TLS_CERT", "/certs/server.pem"), Key: get("AHRARA_TLS_KEY", "/certs/server-key.pem"),
		CA:           get("AHRARA_TLS_CA", "/certs/ca.pem"),
		OfferTimeout: 5 * time.Second, WriteTimeout: 30 * time.Second, AuthTimeout: 15 * time.Second, Heartbeat: 90 * time.Second,
	}
}
