package protocol

import "testing"

func TestFrameBoundaries(t *testing.T) {
	for _, size := range []int{0, 20, ChunkSize + 20} {
		frame, err := Encode(Chunk, make([]byte, size))
		if err != nil {
			t.Fatal(err)
		}
		kind, payload, err := Decode(frame)
		if err != nil || kind != Chunk || len(payload) != size {
			t.Fatal("roundtrip")
		}
		frame[0] = 2
		if _, _, err := Decode(frame); err == nil {
			t.Fatal("version accepted")
		}
	}
	if _, err := Encode(Offer, make([]byte, MaxControl+1)); err == nil {
		t.Fatal("unbounded control")
	}
}
