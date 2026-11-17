package server

import (
	"context"
	"errors"
	"io"
	"sync"
	"testing"
	"time"

	"sidecar/internal/backend"
	"sidecar/internal/proto"
)

// fake is a backend that streams n numbered rows and blocks until killed
// when asked to "hang".
type fake struct {
	mu      sync.Mutex
	killed  []uint64
	sessKil []uint64
	nextID  uint64
	live    map[uint64]chan struct{}
	closed  map[uint64]bool // session id -> closed with discard
	params  [][]proto.Param
}

func newFake() *fake { return &fake{live: map[uint64]chan struct{}{}, closed: map[uint64]bool{}} }

func (f *fake) Name() string             { return "fake" }
func (f *fake) ServerVersion() string    { return "0" }
func (f *fake) Close() error             { return nil }
func (f *fake) CancelsWithContext() bool { return false }
func (f *fake) Classify(err error) backend.Failure {
	return backend.Failure{Kind: proto.KindQuery, Code: 7}
}
func (f *fake) Acquire(ctx context.Context) (backend.Session, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.nextID++
	ch := make(chan struct{})
	f.live[f.nextID] = ch
	return &fakeSession{f: f, id: f.nextID, kill: ch}, nil
}
func (f *fake) KillQuery(ctx context.Context, id uint64) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.killed = append(f.killed, id)
	if ch := f.live[id]; ch != nil {
		close(ch)
		delete(f.live, id)
	}
	return nil
}
func (f *fake) KillSession(ctx context.Context, id uint64) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.sessKil = append(f.sessKil, id)
	return nil
}
func (f *fake) killedIDs() []uint64 {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]uint64(nil), f.killed...)
}

type fakeSession struct {
	f    *fake
	id   uint64
	kill chan struct{}
}

func (s *fakeSession) ID(context.Context) (uint64, error) { return s.id, nil }
func (s *fakeSession) Close(discard bool) {
	s.f.mu.Lock()
	s.f.closed[s.id] = discard
	s.f.mu.Unlock()
}
func (s *fakeSession) Query(ctx context.Context, q string, params []proto.Param) (backend.Rows, error) {
	s.f.mu.Lock()
	s.f.params = append(s.f.params, params)
	s.f.mu.Unlock()
	if q == "fail" {
		return nil, errors.New("boom")
	}
	return &fakeRows{s: s, hang: q == "hang", multi: q == "multi", n: 2500, ctx: ctx}, nil
}

type fakeRows struct {
	s     *fakeSession
	hang  bool
	multi bool
	step  int
	n, i  int
	ctx   context.Context
	err   error
}

func (r *fakeRows) NextResult() bool {
	r.step++
	r.i = 0
	if r.multi {
		// "multi": no result set (5 affected), then 3 rows, then 2 rows.
		r.n = map[int]int{2: 3, 3: 2}[r.step]
		return r.step <= 3
	}
	return r.step == 1
}
func (r *fakeRows) Affected() uint64 { return 5 }
func (r *fakeRows) Columns() []proto.Column {
	if r.multi && r.step == 1 {
		return nil
	}
	return []proto.Column{{Name: "n", Type: "INT", Nullable: proto.NotNull}}
}
func (r *fakeRows) Next() bool {
	if r.multi && r.step == 1 {
		return false
	}
	if r.hang { // like SELECT SLEEP: returns only when the statement is killed
		<-r.s.kill
		r.err = errors.New("interrupted")
		return false
	}
	if r.ctx.Err() != nil {
		r.err = r.ctx.Err()
		return false
	}
	r.i++
	return r.i <= r.n
}
func (r *fakeRows) Row() ([][]byte, error) { return [][]byte{[]byte{byte(r.i)}}, nil }
func (r *fakeRows) Err() error             { return r.err }
func (r *fakeRows) Close() error           { return nil }

// host drives a Server through in-memory pipes.
type host struct {
	t    *testing.T
	w    *io.PipeWriter
	r    *io.PipeReader
	done chan struct{}
}

func start(t *testing.T, be backend.Backend) *host {
	inR, inW := io.Pipe()
	outR, outW := io.Pipe()
	h := &host{t: t, w: inW, r: outR, done: make(chan struct{})}
	go func() { New(be).Run(inR, outW); outW.Close(); close(h.done) }()
	return h
}

func (h *host) send(typ byte, qid uint32, p []byte) {
	if _, err := h.w.Write(proto.Encode(typ, qid, p)); err != nil {
		h.t.Fatal(err)
	}
}

// until skips frames until one of type typ arrives.
func (h *host) until(typ byte) proto.Frame {
	h.t.Helper()
	for {
		if f := h.next(); f.Type == typ || f.Type == 0xFF {
			return f
		}
	}
}

func (h *host) next() proto.Frame {
	h.t.Helper()
	ch := make(chan proto.Frame, 1)
	go func() {
		f, err := proto.ReadFrame(h.r, proto.MaxFrame)
		if err != nil {
			f = proto.Frame{Type: 0xFF}
		}
		ch <- f
	}()
	select {
	case f := <-ch:
		return f
	case <-time.After(5 * time.Second):
		h.t.Fatal("no frame within 5 s")
		return proto.Frame{}
	}
}

func TestStreamHonoursCredits(t *testing.T) {
	h := start(t, newFake())
	h.send(proto.TypeQuery, 1, proto.EncodeQuery(0, 1000, "rows"))
	h.send(proto.TypeCredit, 1, proto.EncodeU32(1))
	if f := h.next(); f.Type != proto.TypeStarted {
		t.Fatalf("want started, got %d", f.Type)
	}
	if f := h.next(); f.Type != proto.TypeColumns {
		t.Fatalf("want columns, got %d", f.Type)
	}
	if f := h.next(); f.Type != proto.TypeBatch {
		t.Fatalf("want batch, got %d", f.Type)
	}
	// One credit used: the second batch must not arrive until another is granted.
	got := make(chan proto.Frame, 1)
	go func() { got <- h.next() }()
	select {
	case f := <-got:
		t.Fatalf("frame %d arrived without a credit", f.Type)
	case <-time.After(300 * time.Millisecond):
	}
	h.send(proto.TypeCredit, 1, proto.EncodeU32(2))
	if f := <-got; f.Type != proto.TypeBatch {
		t.Fatalf("want batch, got %d", f.Type)
	}
	f := h.next() // third batch (500 rows), then done
	rows, _ := proto.DecodeBatch(f.Payload, 1)
	if f.Type != proto.TypeBatch || len(rows) != 500 {
		t.Fatalf("want 500-row batch, got type %d rows %d", f.Type, len(rows))
	}
	f = h.next()
	if total, _ := proto.DecodeU64(f.Payload); f.Type != proto.TypeDone || total != 2500 {
		t.Fatalf("want done(2500), got type %d total %d", f.Type, total)
	}
}

func TestCancelKillsOnControlConnection(t *testing.T) {
	be := newFake()
	h := start(t, be)
	h.send(proto.TypeQuery, 5, proto.EncodeQuery(0, 0, "hang"))
	id := uint64(0)
	if f := h.next(); f.Type != proto.TypeStarted {
		t.Fatalf("got %d", f.Type)
	} else {
		id, _ = proto.DecodeU64(f.Payload)
	}
	h.send(proto.TypeCancel, 5, nil)
	// The reader goroutine must keep serving while the query is stuck.
	f := h.until(proto.TypeCancelled)
	if f.Type != proto.TypeCancelled {
		t.Fatalf("want cancelled, got %d", f.Type)
	}
	if k := be.killedIDs(); len(k) != 1 || k[0] != id {
		t.Fatalf("KILL QUERY ids %v, want [%d]", k, id)
	}
}

func TestTimeoutAndError(t *testing.T) {
	be := newFake()
	h := start(t, be)
	h.send(proto.TypeQuery, 1, proto.EncodeQuery(50, 0, "hang"))
	h.until(proto.TypeStarted)
	f := h.until(proto.TypeError)
	if kind, _, _, _ := proto.DecodeError(f.Payload); f.Type != proto.TypeError || kind != proto.KindTimeout {
		t.Fatalf("want timeout error, got type %d kind %d", f.Type, kind)
	}
	h.send(proto.TypeQuery, 2, proto.EncodeQuery(0, 0, "fail"))
	h.until(proto.TypeStarted)
	f = h.until(proto.TypeError)
	if kind, code, msg, _ := proto.DecodeError(f.Payload); f.Type != proto.TypeError || kind != proto.KindQuery || code != 7 || msg != "boom" {
		t.Fatalf("got type %d kind %d code %d %q", f.Type, kind, code, msg)
	}
}

func TestKillSessionFrame(t *testing.T) {
	be := newFake()
	h := start(t, be)
	h.send(proto.TypeKillSession, 9, proto.EncodeU64(1234))
	if f := h.next(); f.Type != proto.TypeDone || f.QID != 9 {
		t.Fatalf("got %+v", f)
	}
	be.mu.Lock()
	defer be.mu.Unlock()
	if len(be.sessKil) != 1 || be.sessKil[0] != 1234 {
		t.Fatalf("sessions killed: %v", be.sessKil)
	}
}

func TestStdinEOFCancelsAndKills(t *testing.T) {
	be := newFake()
	h := start(t, be)
	h.send(proto.TypeQuery, 1, proto.EncodeQuery(0, 0, "hang"))
	h.send(proto.TypeQuery, 2, proto.EncodeQuery(0, 0, "hang"))
	h.until(proto.TypeStarted)
	h.until(proto.TypeStarted) // both started
	h.w.Close()
	select {
	case <-h.done:
	case <-time.After(5 * time.Second):
		t.Fatal("Run did not return after stdin EOF")
	}
	if k := be.killedIDs(); len(k) != 2 {
		t.Fatalf("want both queries killed before exit, got %v", k)
	}
}

// Several results of one call: Affected, then two result sets, each with its
// own Columns, and one Done with the rows streamed in total.
func TestMultipleResults(t *testing.T) {
	h := start(t, newFake())
	h.send(proto.TypeQuery, 1, proto.EncodeQuery(0, 1000, "multi"))
	h.send(proto.TypeCredit, 1, proto.EncodeU32(8))
	var order []byte
	for {
		f := h.next()
		order = append(order, f.Type)
		if f.Type == proto.TypeAffected {
			if n, _ := proto.DecodeU64(f.Payload); n != 5 {
				t.Fatalf("affected = %d, want 5", n)
			}
		}
		if f.Type == proto.TypeDone {
			if n, _ := proto.DecodeU64(f.Payload); n != 5 {
				t.Fatalf("done rows = %d, want 3+2", n)
			}
			break
		}
		if f.Type == 0xFF || f.Type == proto.TypeError {
			t.Fatalf("unexpected frame %d after %v", f.Type, order)
		}
	}
	want := []byte{proto.TypeStarted, proto.TypeAffected, proto.TypeColumns, proto.TypeBatch, proto.TypeColumns, proto.TypeBatch, proto.TypeDone}
	if string(order) != string(want) {
		t.Fatalf("frame order %v, want %v", order, want)
	}
}

func TestRunBindsParametersAndRoundTrips(t *testing.T) {
	in := proto.Run{Session: 3, TimeoutMS: 9, BatchRows: 7, SQL: "update t set a = ? where b = ?",
		Params: []proto.Param{{Kind: proto.ParamText, Data: []byte("x")}, {Kind: proto.ParamNull}, {Kind: proto.ParamBytes, Data: []byte{0xff, 0}}}}
	out, err := proto.DecodeRun(proto.EncodeRun(in))
	if err != nil || out.SQL != in.SQL || out.Session != 3 || len(out.Params) != 3 || out.Params[1].Kind != proto.ParamNull || string(out.Params[2].Data) != "\xff\x00" {
		t.Fatalf("round trip: %+v %v", out, err)
	}
	if _, err := proto.DecodeRun(proto.EncodeRun(in)[:20]); err == nil {
		t.Fatal("truncated run frame decoded")
	}
	be := newFake()
	h := start(t, be)
	h.send(proto.TypeRun, 1, proto.EncodeRun(in0(0, "rows", in.Params)))
	h.send(proto.TypeCredit, 1, proto.EncodeU32(8))
	h.until(proto.TypeDone)
	be.mu.Lock()
	defer be.mu.Unlock()
	if len(be.params) != 1 || len(be.params[0]) != 3 || string(be.params[0][0].Data) != "x" {
		t.Fatalf("backend saw params %v", be.params)
	}
}

func in0(session uint32, sql string, ps []proto.Param) proto.Run {
	return proto.Run{Session: session, SQL: sql, Params: ps}
}

func TestPinnedSessionKeepsOneConnectionAndClosesDiscarding(t *testing.T) {
	be := newFake()
	h := start(t, be)
	h.send(proto.TypeOpenSession, 10, nil)
	st := h.next()
	if st.Type != proto.TypeStarted {
		t.Fatalf("want started, got %d", st.Type)
	}
	id, _ := proto.DecodeU64(st.Payload)
	h.until(proto.TypeDone)
	for qid := uint32(11); qid <= 12; qid++ {
		h.send(proto.TypeRun, qid, proto.EncodeRun(in0(10, "rows", nil)))
		h.send(proto.TypeCredit, qid, proto.EncodeU32(8))
		f := h.next()
		if got, _ := proto.DecodeU64(f.Payload); f.Type != proto.TypeStarted || got != id {
			t.Fatalf("statement %d ran on session %d, want %d (type %d)", qid, got, id, f.Type)
		}
		h.until(proto.TypeDone)
	}
	be.mu.Lock()
	if _, closed := be.closed[id]; closed || be.nextID != 1 {
		be.mu.Unlock()
		t.Fatalf("session closed early or a second connection was taken (acquired %d)", be.nextID)
	}
	be.mu.Unlock()
	h.send(proto.TypeCloseSession, 10, nil)
	deadline := time.Now().Add(3 * time.Second)
	for {
		be.mu.Lock()
		discard, closed := be.closed[id]
		be.mu.Unlock()
		if closed {
			if !discard {
				t.Fatal("session was returned to the pool instead of being discarded")
			}
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("session was not closed")
		}
		time.Sleep(10 * time.Millisecond)
	}
	h.send(proto.TypeRun, 13, proto.EncodeRun(in0(10, "rows", nil)))
	if f := h.until(proto.TypeError); f.Type != proto.TypeError {
		t.Fatal("statement on a closed session did not fail")
	}
}

func TestCancelOnPinnedSessionKeepsConnection(t *testing.T) {
	be := newFake()
	h := start(t, be)
	h.send(proto.TypeOpenSession, 1, nil)
	h.until(proto.TypeDone)
	h.send(proto.TypeRun, 2, proto.EncodeRun(in0(1, "hang", nil)))
	h.until(proto.TypeStarted)
	h.send(proto.TypeCancel, 2, nil)
	if f := h.until(proto.TypeCancelled); f.Type != proto.TypeCancelled {
		t.Fatalf("want cancelled, got %d", f.Type)
	}
	be.mu.Lock()
	_, closed := be.closed[1]
	be.mu.Unlock()
	if closed {
		t.Fatal("a cancelled statement closed the pinned session")
	}
}
