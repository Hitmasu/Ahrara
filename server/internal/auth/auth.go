package auth

import (
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"strings"
)

type Route [16]byte

func Derive(public ed25519.PublicKey) Route {
	h := sha256.New()
	h.Write([]byte("ahrara-route-v1\x00"))
	h.Write(public)
	var route Route
	copy(route[:], h.Sum(nil)[:16])
	return route
}

func Preimage(public, challenge []byte) []byte {
	b := append([]byte("ahrara-auth-v1\x00"), public...)
	return append(b, challenge...)
}

func DecodeAddress(address string) (Route, [8]byte, string, error) {
	var route Route
	var alias [8]byte
	local, domain, ok := strings.Cut(address, "@")
	if !ok || !strings.EqualFold(domain, "ahrara.dev") || len(local) != 32 {
		return route, alias, "", errors.New("invalid address")
	}
	b, err := base64.RawURLEncoding.Strict().DecodeString(local)
	if err != nil || len(b) != 24 || base64.RawURLEncoding.EncodeToString(b) != local {
		return route, alias, "", errors.New("invalid token")
	}
	copy(route[:], b[:16])
	copy(alias[:], b[16:])
	return route, alias, local + "@ahrara.dev", nil
}
