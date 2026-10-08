package ahrara

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"time"
)

// nativeReleaseURL serves the native libraries attached to each GitHub
// Release. Tests point it at a local server.
var nativeReleaseURL = "https://github.com/Hitmasu/Ahrara/releases/download"

const nativeMaxSize = 128 << 20

// nativeLibrary returns the native core to load: AHRARA_NATIVE_LIBRARY, or
// this platform's release asset, downloaded once into the user cache and
// checked against the SHA-256 pinned in native_checksums.go on every load.
func nativeLibrary() (string, error) {
	if path := os.Getenv("AHRARA_NATIVE_LIBRARY"); path != "" {
		return path, nil
	}

	asset, name := nativeAsset(runtime.GOOS, runtime.GOARCH)
	sum := nativeSHA256[asset]
	if nativeVersion == "" || sum == "" {
		return "", errors.New("no published Ahrara native library for " + runtime.GOOS + "/" + runtime.GOARCH + "; set AHRARA_NATIVE_LIBRARY to its absolute path")
	}

	cache, err := os.UserCacheDir()
	if err != nil {
		return "", fmt.Errorf("locate the cache for the Ahrara native library: %w", err)
	}
	path := filepath.Join(cache, "ahrara", "native", sum, name)
	if nativeVerified(path, sum) {
		return path, nil
	}

	data, err := nativeDownload(nativeReleaseURL + "/v" + nativeVersion + "/" + asset)
	if err != nil {
		return "", err
	}
	if nativeDigest(data) != sum {
		return "", errors.New("the downloaded Ahrara native library does not match its pinned SHA-256")
	}
	// Another process may have stored it first; Windows cannot replace a loaded DLL.
	if err := nativeStore(path, data); err != nil && !nativeVerified(path, sum) {
		return "", fmt.Errorf("cache the Ahrara native library: %w", err)
	}
	return path, nil
}

func nativeAsset(goos, goarch string) (asset, name string) {
	arch := map[string]string{"amd64": "x86_64", "arm64": "aarch64"}[goarch]
	extension := map[string]string{"linux": "so", "darwin": "dylib", "windows": "dll"}[goos]
	name = "libahrara_native." + extension
	if goos == "windows" {
		name = "ahrara_native.dll"
	}
	return "ahrara-native-" + goos + "-" + arch + "." + extension, name
}

func nativeVerified(path, sum string) bool {
	data, err := os.ReadFile(path)
	return err == nil && nativeDigest(data) == sum
}

func nativeDigest(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

func nativeDownload(url string) ([]byte, error) {
	client := &http.Client{Timeout: 5 * time.Minute}
	response, err := client.Get(url)
	if err != nil {
		return nil, fmt.Errorf("download the Ahrara native library: %w", err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("download the Ahrara native library: %s", response.Status)
	}

	data, err := io.ReadAll(io.LimitReader(response.Body, nativeMaxSize+1))
	if err != nil {
		return nil, fmt.Errorf("download the Ahrara native library: %w", err)
	}
	if len(data) > nativeMaxSize {
		return nil, errors.New("the Ahrara native library download is too large")
	}
	return data, nil
}

func nativeStore(path string, data []byte) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	file, err := os.CreateTemp(filepath.Dir(path), ".download-*")
	if err != nil {
		return err
	}
	defer os.Remove(file.Name())

	if _, err := file.Write(data); err != nil {
		file.Close()
		return err
	}
	if err := file.Close(); err != nil {
		return err
	}
	return os.Rename(file.Name(), path)
}
