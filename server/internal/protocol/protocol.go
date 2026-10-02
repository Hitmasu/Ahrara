// Package protocol implements the bounded Ahrara v1 WebSocket envelope.
package protocol

import (
	"encoding/binary"
	"encoding/json"
	"errors"
)

const (
	Version           = 1
	ChunkSize         = 64 * 1024
	MaxEmail          = 10 * 1024 * 1024
	MaxControl        = 16 * 1024
	MaxFrame          = 6 + 16 + 4 + ChunkSize
	Hello        byte = 1
	Challenge    byte = 2
	Authenticate byte = 3
	Ready        byte = 4
	Failure      byte = 5
	Offer        byte = 10
	Accept       byte = 11
	Reject       byte = 12
	Start        byte = 20
	Chunk        byte = 21
	End          byte = 22
	Abort        byte = 23
	Ack          byte = 24
	Nack         byte = 25
)

func Encode(kind byte, payload []byte) ([]byte, error) {
	limit := MaxControl
	if kind == Chunk {
		limit = 20 + ChunkSize
	}
	if len(payload) > limit {
		return nil, errors.New("frame too large")
	}
	b := make([]byte, 6+len(payload))
	b[0], b[1] = Version, kind
	binary.BigEndian.PutUint32(b[2:6], uint32(len(payload)))
	copy(b[6:], payload)
	return b, nil
}

func JSON(kind byte, value any) ([]byte, error) {
	b, err := json.Marshal(value)
	if err != nil {
		return nil, err
	}
	return Encode(kind, b)
}

func Decode(b []byte) (byte, []byte, error) {
	if len(b) < 6 || b[0] != Version || int(binary.BigEndian.Uint32(b[2:6])) != len(b)-6 {
		return 0, nil, errors.New("invalid frame")
	}
	limit := MaxControl
	if b[1] == Chunk {
		limit = 20 + ChunkSize
	}
	if len(b)-6 > limit {
		return 0, nil, errors.New("frame too large")
	}
	return b[1], b[6:], nil
}
