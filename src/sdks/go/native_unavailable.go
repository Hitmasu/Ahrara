//go:build !cgo || (!linux && !darwin && !windows)

package ahrara

import "errors"

func nativeCall([]byte) ([]byte, error) {
	return nil, errors.New("Ahrara requires CGO_ENABLED=1 on Linux, macOS, or Windows")
}
