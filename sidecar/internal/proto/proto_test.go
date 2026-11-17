package proto

import (
	"bytes"
	"errors"
	"io"
	"reflect"
	"testing"
)

func TestFrameLayout(t *testing.T) {
	got := Encode(TypeCredit, 0x01020304, []byte{9, 8})
	want := []byte{7, 0, 0, 0, 3, 4, 3, 2, 1, 9, 8} // len=5+2, type, qid LE, payload
	if !bytes.Equal(got, want) {
		t.Fatalf("got % x want % x", got, want)
	}
}

func TestFrameRoundTrip(t *testing.T) {
	var buf bytes.Buffer
	buf.Write(Encode(TypeQuery, 1, EncodeQuery(2500, 100, "SELECT 'é'")))
	buf.Write(Encode(TypeCancel, 1, nil))
	buf.Write(Encode(TypeStarted, 7, EncodeU64(1<<40+5)))
	for _, want := range []struct {
		typ byte
		qid uint32
		n   int
	}{{TypeQuery, 1, 8 + len("SELECT 'é'")}, {TypeCancel, 1, 0}, {TypeStarted, 7, 8}} {
		f, err := ReadFrame(&buf, MaxInbound)
		if err != nil {
			t.Fatal(err)
		}
		if f.Type != want.typ || f.QID != want.qid || len(f.Payload) != want.n {
			t.Fatalf("got %+v want %+v", f, want)
		}
	}
	if _, err := ReadFrame(&buf, MaxInbound); err != io.EOF {
		t.Fatalf("want EOF between frames, got %v", err)
	}
}

func TestReadFrameErrors(t *testing.T) {
	full := Encode(TypeBatch, 3, bytes.Repeat([]byte{1}, 20))
	if _, err := ReadFrame(bytes.NewReader(full[:5]), MaxInbound); err != io.ErrUnexpectedEOF {
		t.Errorf("cut header: %v", err)
	}
	if _, err := ReadFrame(bytes.NewReader(full[:15]), MaxInbound); err != io.ErrUnexpectedEOF {
		t.Errorf("cut payload: %v", err)
	}
	if _, err := ReadFrame(bytes.NewReader(full), 10); !errors.Is(err, ErrFrameTooLarge) {
		t.Errorf("oversize: %v", err)
	}
	if _, err := ReadFrame(bytes.NewReader([]byte{4, 0, 0, 0, 1, 0, 0, 0, 0}), MaxInbound); err == nil {
		t.Error("length below the header size must fail")
	}
}

func TestQuery(t *testing.T) {
	to, rows, sql, err := DecodeQuery(EncodeQuery(30000, 500, "SELECT 1"))
	if err != nil || to != 30000 || rows != 500 || sql != "SELECT 1" {
		t.Fatalf("%v %v %q %v", to, rows, sql, err)
	}
	if _, _, _, err := DecodeQuery([]byte{1, 2, 3}); !errors.Is(err, ErrShort) {
		t.Fatal("short query payload must fail")
	}
}

func TestColumns(t *testing.T) {
	want := []Column{
		{"id", "INT", NotNull},
		{"姓名", "VARCHAR", Nullable},
		{"", "DECIMAL", NullUnknown},
	}
	got, err := DecodeColumns(EncodeColumns(want))
	if err != nil || !reflect.DeepEqual(got, want) {
		t.Fatalf("%+v %v", got, err)
	}
	enc := EncodeColumns(want)
	for n := 0; n < len(enc); n++ {
		if _, err := DecodeColumns(enc[:n]); err == nil {
			t.Fatalf("truncated at %d decoded without error", n)
		}
	}
}

func TestBatchNullEmptyAndBinary(t *testing.T) {
	b := NewBatch(0)
	b.AddRow([][]byte{[]byte("a"), nil, {}})
	b.AddRow([][]byte{{0xff, 0, 0xfe}, []byte("終"), nil})
	if b.Rows() != 2 {
		t.Fatal(b.Rows())
	}
	p := b.Bytes()
	rows, err := DecodeBatch(p, 3)
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != 2 {
		t.Fatalf("rows=%d", len(rows))
	}
	if string(rows[0][0]) != "a" || rows[0][1] != nil || rows[0][2] == nil || len(rows[0][2]) != 0 {
		t.Errorf("row 0: %q", rows[0])
	}
	if !bytes.Equal(rows[1][0], []byte{0xff, 0, 0xfe}) || string(rows[1][1]) != "終" || rows[1][2] != nil {
		t.Errorf("row 1: %q", rows[1])
	}
}

func TestBatchRejectsDamage(t *testing.T) {
	b := NewBatch(0)
	b.AddRow([][]byte{[]byte("abc")})
	p := b.Bytes()
	if _, err := DecodeBatch(p[:len(p)-1], 1); err == nil {
		t.Error("truncated cell must fail")
	}
	if _, err := DecodeBatch(append(p[:len(p):len(p)], 0), 1); err == nil {
		t.Error("trailing byte must fail")
	}
	if _, err := DecodeBatch(p, 2); err == nil {
		t.Error("wrong column count must fail")
	}
	empty, err := DecodeBatch(NewBatch(0).Bytes(), 4)
	if err != nil || len(empty) != 0 {
		t.Errorf("empty batch: %v %v", empty, err)
	}
}

func TestErrorAndReady(t *testing.T) {
	k, c, m, err := DecodeError(EncodeError(KindAuth, 1045, "Access denied"))
	if err != nil || k != KindAuth || c != 1045 || m != "Access denied" {
		t.Fatalf("%v %v %q %v", k, c, m, err)
	}
	v, d, s, err := DecodeReady(EncodeReady("mysql", "11.8.5-MariaDB"))
	if err != nil || v != Version || d != "mysql" || s != "11.8.5-MariaDB" {
		t.Fatalf("%v %q %q %v", v, d, s, err)
	}
	if _, _, _, err := DecodeError([]byte{1}); err == nil {
		t.Error("short error frame must fail")
	}
}

// Same bytes as run_frame_layout_matches_go_side in crates/db-sidecar-client.
func TestRunLayout(t *testing.T) {
	want := []byte{3, 0, 0, 0, 9, 0, 0, 0, 7, 0, 0, 0, 3, 0, 1, 1, 0, 0, 0, 'x', 0, 2, 2, 0, 0, 0, 0xff, 0, 'q'}
	got := EncodeRun(Run{Session: 3, TimeoutMS: 9, BatchRows: 7, SQL: "q", Params: []Param{
		{Kind: ParamText, Data: []byte("x")}, {Kind: ParamNull}, {Kind: ParamBytes, Data: []byte{0xff, 0}}}})
	if string(got) != string(want) {
		t.Fatalf("layout\n got %v\nwant %v", got, want)
	}
	r, err := DecodeRun(want)
	if err != nil || r.Session != 3 || len(r.Params) != 3 || r.SQL != "q" {
		t.Fatalf("decode: %+v %v", r, err)
	}
}
