// Package proto is the sidecar wire format. See sidecar/PROTOCOL.md.
//
// Every frame is [u32 len][u8 type][u32 qid][payload], little endian,
// len = 5 + len(payload).
package proto

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io"
)

// Frame types, host -> sidecar.
const (
	TypeQuery       byte = 1
	TypeCancel      byte = 2
	TypeCredit      byte = 3
	TypeKillSession byte = 4
	// TypeRun: a statement with bound parameters and/or on a pinned session.
	TypeRun byte = 5
	// TypeOpenSession pins one database connection; the frame's qid becomes
	// the session id. TypeCloseSession (same qid) rolls back and closes it.
	TypeOpenSession  byte = 6
	TypeCloseSession byte = 7
)

// Frame types, sidecar -> host.
const (
	TypeStarted   byte = 10
	TypeColumns   byte = 11
	TypeBatch     byte = 12
	TypeDone      byte = 13
	TypeError     byte = 14
	TypeCancelled byte = 15
	// TypeAffected: a statement without a result set finished. u64 rows.
	TypeAffected byte = 16
	TypeReady    byte = 20
)

// Version is the protocol version reported in the ready frame.
const Version uint16 = 1

// Error kinds carried by TypeError. Same order as db_core::ErrorKind.
const (
	KindConnect  byte = 1
	KindAuth     byte = 2
	KindTLS      byte = 3
	KindQuery    byte = 4
	KindTimeout  byte = 5
	KindInternal byte = 6
)

// Column nullability.
const (
	NotNull     byte = 0
	Nullable    byte = 1
	NullUnknown byte = 2
)

const (
	headerLen = 9
	// NullLen marks a NULL cell in a batch.
	NullLen = 0xFFFFFFFF
	// MaxInbound is the largest frame the sidecar accepts from the host.
	MaxInbound = 64 << 20
	// MaxFrame is the largest frame the sidecar sends. A single row that does
	// not fit fails the query instead of the whole connection.
	MaxFrame = 256 << 20
)

var le = binary.LittleEndian

var (
	ErrFrameTooLarge = errors.New("frame too large")
	ErrShort         = errors.New("payload too short")
)

// Frame is one decoded frame.
type Frame struct {
	Type    byte
	QID     uint32
	Payload []byte
}

// AppendFrame appends the encoded frame to dst.
func AppendFrame(dst []byte, t byte, qid uint32, payload []byte) []byte {
	dst = le.AppendUint32(dst, uint32(5+len(payload)))
	dst = append(dst, t)
	dst = le.AppendUint32(dst, qid)
	return append(dst, payload...)
}

// Encode returns the frame as one buffer.
func Encode(t byte, qid uint32, payload []byte) []byte {
	return AppendFrame(make([]byte, 0, headerLen+len(payload)), t, qid, payload)
}

// ReadFrame reads one frame. io.EOF means the stream ended between frames;
// io.ErrUnexpectedEOF means it ended inside one.
func ReadFrame(r io.Reader, max uint32) (Frame, error) {
	var h [headerLen]byte
	if _, err := io.ReadFull(r, h[:]); err != nil {
		return Frame{}, err
	}
	n := le.Uint32(h[:4])
	if n < 5 {
		return Frame{}, fmt.Errorf("frame length %d is smaller than the header", n)
	}
	if n-5 > max {
		return Frame{}, fmt.Errorf("%w: %d bytes", ErrFrameTooLarge, n-5)
	}
	f := Frame{Type: h[4], QID: le.Uint32(h[5:]), Payload: make([]byte, n-5)}
	if _, err := io.ReadFull(r, f.Payload); err != nil {
		if err == io.EOF {
			err = io.ErrUnexpectedEOF
		}
		return Frame{}, err
	}
	return f, nil
}

// ---- query ----

// EncodeQuery builds a TypeQuery payload: u32 timeout_ms (0 = none),
// u32 batch_rows (0 = sidecar default), sql (UTF-8, rest of the payload).
func EncodeQuery(timeoutMS, batchRows uint32, sql string) []byte {
	p := make([]byte, 0, 8+len(sql))
	p = le.AppendUint32(p, timeoutMS)
	p = le.AppendUint32(p, batchRows)
	return append(p, sql...)
}

func DecodeQuery(p []byte) (timeoutMS, batchRows uint32, sql string, err error) {
	if len(p) < 8 {
		return 0, 0, "", ErrShort
	}
	return le.Uint32(p), le.Uint32(p[4:]), string(p[8:]), nil
}

// ---- run (execute with bound parameters, statements on a session) ----

// Parameter kinds of a bound value.
const (
	ParamNull  byte = 0
	ParamText  byte = 1
	ParamBytes byte = 2
)

// Param is one bound value. Data is nil for ParamNull.
type Param struct {
	Kind byte
	Data []byte
}

// Run is a decoded TypeRun payload.
type Run struct {
	Session   uint32 // qid of the open_session frame; 0 = any pooled connection
	TimeoutMS uint32
	BatchRows uint32
	Params    []Param
	SQL       string
}

// EncodeRun builds a TypeRun payload: u32 session, u32 timeout_ms,
// u32 batch_rows, u16 param count, per param u8 kind (+ u32 len + bytes for
// text and bytes), sql (rest of the payload).
func EncodeRun(r Run) []byte {
	p := make([]byte, 0, 14+len(r.SQL))
	p = le.AppendUint32(p, r.Session)
	p = le.AppendUint32(p, r.TimeoutMS)
	p = le.AppendUint32(p, r.BatchRows)
	p = le.AppendUint16(p, uint16(len(r.Params)))
	for _, a := range r.Params {
		p = append(p, a.Kind)
		if a.Kind != ParamNull {
			p = le.AppendUint32(p, uint32(len(a.Data)))
			p = append(p, a.Data...)
		}
	}
	return append(p, r.SQL...)
}

func DecodeRun(p []byte) (Run, error) {
	var r Run
	if len(p) < 14 {
		return r, ErrShort
	}
	r.Session, r.TimeoutMS, r.BatchRows = le.Uint32(p), le.Uint32(p[4:]), le.Uint32(p[8:])
	n := int(le.Uint16(p[12:]))
	p = p[14:]
	for i := 0; i < n; i++ {
		if len(p) < 1 {
			return r, ErrShort
		}
		a := Param{Kind: p[0]}
		p = p[1:]
		switch a.Kind {
		case ParamNull:
		case ParamText, ParamBytes:
			if len(p) < 4 {
				return r, ErrShort
			}
			l := int(le.Uint32(p))
			p = p[4:]
			if len(p) < l {
				return r, ErrShort
			}
			a.Data = p[:l:l]
			p = p[l:]
		default:
			return r, fmt.Errorf("unknown parameter kind %d", a.Kind)
		}
		r.Params = append(r.Params, a)
	}
	r.SQL = string(p)
	return r, nil
}

func EncodeU32(v uint32) []byte { return le.AppendUint32(nil, v) }
func EncodeU64(v uint64) []byte { return le.AppendUint64(nil, v) }

func DecodeU32(p []byte) (uint32, error) {
	if len(p) < 4 {
		return 0, ErrShort
	}
	return le.Uint32(p), nil
}

func DecodeU64(p []byte) (uint64, error) {
	if len(p) < 8 {
		return 0, ErrShort
	}
	return le.Uint64(p), nil
}

// ---- columns ----

// Column is one result column.
type Column struct {
	Name     string
	Type     string
	Nullable byte // NotNull, Nullable or NullUnknown
}

func appendStr(dst []byte, s string) []byte {
	if len(s) > 0xFFFF {
		s = s[:0xFFFF]
	}
	dst = le.AppendUint16(dst, uint16(len(s)))
	return append(dst, s...)
}

func readStr(p []byte) (string, []byte, error) {
	if len(p) < 2 {
		return "", nil, ErrShort
	}
	n := int(le.Uint16(p))
	if len(p) < 2+n {
		return "", nil, ErrShort
	}
	return string(p[2 : 2+n]), p[2+n:], nil
}

// EncodeColumns builds a TypeColumns payload: u16 count, then per column
// u16 len + name, u16 len + type name, u8 nullability.
func EncodeColumns(cols []Column) []byte {
	p := le.AppendUint16(nil, uint16(len(cols)))
	for _, c := range cols {
		p = appendStr(p, c.Name)
		p = appendStr(p, c.Type)
		p = append(p, c.Nullable)
	}
	return p
}

func DecodeColumns(p []byte) ([]Column, error) {
	if len(p) < 2 {
		return nil, ErrShort
	}
	n := int(le.Uint16(p))
	p = p[2:]
	cols := make([]Column, 0, n)
	for i := 0; i < n; i++ {
		var c Column
		var err error
		if c.Name, p, err = readStr(p); err != nil {
			return nil, err
		}
		if c.Type, p, err = readStr(p); err != nil {
			return nil, err
		}
		if len(p) < 1 {
			return nil, ErrShort
		}
		c.Nullable, p = p[0], p[1:]
		cols = append(cols, c)
	}
	return cols, nil
}

// ---- batch ----

// Batch builds a TypeBatch payload: u32 row count, then for every row, for
// every column, u32 length + bytes (length 0xFFFFFFFF = NULL).
type Batch struct {
	b []byte
	n uint32
}

func NewBatch(sizeHint int) *Batch {
	return &Batch{b: make([]byte, 4, 4+sizeHint)}
}

// AddRow appends one row; a nil cell is NULL (an empty non-nil cell is "").
func (b *Batch) AddRow(cells [][]byte) {
	for _, c := range cells {
		if c == nil {
			b.b = le.AppendUint32(b.b, NullLen)
			continue
		}
		b.b = le.AppendUint32(b.b, uint32(len(c)))
		b.b = append(b.b, c...)
	}
	b.n++
}

func (b *Batch) Rows() int { return int(b.n) }

// Len is the payload size so far.
func (b *Batch) Len() int { return len(b.b) }

// Bytes finishes the payload. The Batch must not be used afterwards.
func (b *Batch) Bytes() []byte {
	le.PutUint32(b.b, b.n)
	return b.b
}

// DecodeBatch returns rows of cells; a nil cell is NULL. Cells alias p.
func DecodeBatch(p []byte, cols int) ([][][]byte, error) {
	if len(p) < 4 {
		return nil, ErrShort
	}
	n := int(le.Uint32(p))
	p = p[4:]
	rows := make([][][]byte, 0, n)
	for r := 0; r < n; r++ {
		row := make([][]byte, cols)
		for c := 0; c < cols; c++ {
			if len(p) < 4 {
				return nil, ErrShort
			}
			l := le.Uint32(p)
			p = p[4:]
			if l == NullLen {
				continue
			}
			if uint32(len(p)) < l {
				return nil, ErrShort
			}
			row[c] = p[:l:l]
			p = p[l:]
		}
		rows = append(rows, row)
	}
	if len(p) != 0 {
		return nil, fmt.Errorf("%d trailing bytes after the last row", len(p))
	}
	return rows, nil
}

// ---- error / ready ----

// EncodeError builds a TypeError payload: u8 kind, u32 server error code
// (0 = none), message (UTF-8, rest of the payload).
func EncodeError(kind byte, code uint32, msg string) []byte {
	p := make([]byte, 0, 5+len(msg))
	p = append(p, kind)
	p = le.AppendUint32(p, code)
	return append(p, msg...)
}

func DecodeError(p []byte) (kind byte, code uint32, msg string, err error) {
	if len(p) < 5 {
		return 0, 0, "", ErrShort
	}
	return p[0], le.Uint32(p[1:]), string(p[5:]), nil
}

// EncodeReady builds a TypeReady payload: u16 protocol version, u16 len +
// driver name, u16 len + server version text.
func EncodeReady(driver, serverVersion string) []byte {
	p := le.AppendUint16(nil, Version)
	p = appendStr(p, driver)
	return appendStr(p, serverVersion)
}

func DecodeReady(p []byte) (version uint16, driver, server string, err error) {
	if len(p) < 2 {
		return 0, "", "", ErrShort
	}
	version = le.Uint16(p)
	if driver, p, err = readStr(p[2:]); err != nil {
		return
	}
	server, _, err = readStr(p)
	return
}
