package ahrara

import (
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync/atomic"
	"testing"
)

func publishNative(t *testing.T, library []byte, pinned string) (path string, requests *atomic.Int32) {
	t.Helper()
	asset, name := nativeAsset(runtime.GOOS, runtime.GOARCH)
	requests = new(atomic.Int32)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests.Add(1)
		if r.URL.Path != "/v1.2.3/"+asset {
			http.NotFound(w, r)
			return
		}
		_, _ = w.Write(library)
	}))
	t.Cleanup(server.Close)

	cache := t.TempDir()
	t.Setenv("AHRARA_NATIVE_LIBRARY", "")
	t.Setenv("XDG_CACHE_HOME", cache)
	t.Setenv("HOME", cache)
	t.Setenv("LocalAppData", cache)
	url, version, sums := nativeReleaseURL, nativeVersion, nativeSHA256
	nativeReleaseURL, nativeVersion, nativeSHA256 = server.URL, "1.2.3", map[string]string{asset: pinned}
	t.Cleanup(func() { nativeReleaseURL, nativeVersion, nativeSHA256 = url, version, sums })

	root, err := os.UserCacheDir()
	if err != nil {
		t.Fatal(err)
	}
	return filepath.Join(root, "ahrara", "native", pinned, name), requests
}

func TestNativeLibraryDownloadsOnceAndRechecksTheCache(t *testing.T) {
	library := []byte("native library")
	want, requests := publishNative(t, library, nativeDigest(library))

	for range 2 {
		path, err := nativeLibrary()
		if err != nil || path != want {
			t.Fatalf("nativeLibrary() = %q, %v; want %q", path, err, want)
		}
	}
	if got, _ := os.ReadFile(want); string(got) != string(library) || requests.Load() != 1 {
		t.Fatalf("cached %q after %d requests", got, requests.Load())
	}

	// A tampered cache entry is downloaded again.
	if err := os.WriteFile(want, []byte("tampered"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := nativeLibrary(); err != nil || requests.Load() != 2 {
		t.Fatalf("after tampering: %v, %d requests", err, requests.Load())
	}
}

func TestNativeLibraryRejectsAChecksumMismatch(t *testing.T) {
	path, _ := publishNative(t, []byte("native library"), nativeDigest([]byte("other bytes")))

	if _, err := nativeLibrary(); err == nil || !strings.Contains(err.Error(), "SHA-256") {
		t.Fatalf("nativeLibrary() error = %v", err)
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatalf("a mismatched library was cached: %v", err)
	}
}
