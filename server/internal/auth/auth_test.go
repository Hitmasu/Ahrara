package auth

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"os"
	"testing"
)

func TestSharedGoRustVector(t *testing.T) {
	data, err := os.ReadFile("../../../protocol/vectors.json")
	if err != nil {
		t.Fatal(err)
	}
	var v map[string]string
	if err = json.Unmarshal(data, &v); err != nil {
		t.Fatal(err)
	}
	public, _ := hex.DecodeString(v["public_key"])
	route := Derive(public)
	if hex.EncodeToString(route[:]) != v["route_id"] || len(route) != 16 {
		t.Fatal("route vector mismatch")
	}
	r, a, _, err := DecodeAddress(v["address"])
	if err != nil || r != route || hex.EncodeToString(a[:]) != v["alias_id"] {
		t.Fatal("address vector mismatch")
	}
}

func TestRoutesAndAddresses(t *testing.T) {
	pub, private, _ := ed25519.GenerateKey(rand.Reader)
	other, _, _ := ed25519.GenerateKey(rand.Reader)
	if Derive(pub) != Derive(pub) || Derive(pub) == Derive(other) {
		t.Fatal("route derivation")
	}
	route := Derive(pub)
	for i := 0; i < 3; i++ {
		var alias [8]byte
		rand.Read(alias[:])
		raw := append(route[:], alias[:]...)
		address := base64.RawURLEncoding.EncodeToString(raw) + "@ahrara.dev"
		got, gotAlias, canonical, err := DecodeAddress(address)
		if err != nil || got != route || gotAlias != alias || canonical != address || len(address) != 43 {
			t.Fatal("address round trip")
		}
	}
	challenge := make([]byte, 32)
	rand.Read(challenge)
	sig := ed25519.Sign(private, Preimage(pub, challenge))
	if !ed25519.Verify(pub, Preimage(pub, challenge), sig) {
		t.Fatal("signature")
	}
	challenge[0] ^= 1
	if ed25519.Verify(pub, Preimage(pub, challenge), sig) {
		t.Fatal("challenge not bound")
	}
}

func TestInvalidAddresses(t *testing.T) {
	for _, a := range []string{"bad@ahrara.dev", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA@evil.dev", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=@ahrara.dev", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA+@ahrara.dev"} {
		if _, _, _, err := DecodeAddress(a); err == nil {
			t.Fatalf("accepted %q", a)
		}
	}
}
