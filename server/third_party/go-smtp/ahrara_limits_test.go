package smtp

import (
	"bufio"
	"io"
	"strings"
	"testing"
)

func TestAhraraInclusiveDataLimit(t *testing.T) {
	for _, body := range []string{"x\r\n", "..\r\n"} {
		r := &dataReader{r:bufio.NewReader(strings.NewReader(body+".\r\n")), limited:true, n:3}
		got, err := io.ReadAll(r)
		if err != nil { t.Fatalf("exactly 3 bytes must be accepted: %v", err) }
		if len(got) != 3 { t.Fatalf("got %d decoded bytes", len(got)) }
	}
}

func TestAhraraDataLimitRejectsExtraByte(t *testing.T) {
	r := &dataReader{r:bufio.NewReader(strings.NewReader("xx\r\n.\r\n")), limited:true, n:3}
	if _, err := io.ReadAll(r); err != ErrDataTooLarge { t.Fatalf("want size error, got %v",err) }
}
