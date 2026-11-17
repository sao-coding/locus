// Package server is the protocol loop: one stdin reader that never blocks on
// query work, one stdout writer, one goroutine per query, credit
// back-pressure. It knows nothing about the database; see package backend.
package server

import (
	"bufio"
	"context"
	"io"
	"sync"
	"sync/atomic"
	"time"

	"sidecar/internal/backend"
	"sidecar/internal/proto"
)

const (
	defaultBatchRows = 1000
	// A batch is also closed when its payload reaches this many bytes, so
	// wide rows cannot build huge frames.
	batchBytes = 4 << 20
	// Upper bound for the KILL statements.
	killTimeout = 5 * time.Second
	// On stdin EOF: how long to wait for the queries to finish their KILLs.
	exitGrace = 3 * time.Second
)

type query struct {
	cancel   context.CancelFunc
	credits  chan uint32
	cancelAt atomic.Int64 // unix ns of the host's cancel frame, 0 = none
	killErr  error        // result of the KILL QUERY; read only after the kill finished
}

type Server struct {
	be  backend.Backend
	out chan []byte

	mu   sync.Mutex
	qs   map[uint32]*query
	sess map[uint32]*pinned
	wg   sync.WaitGroup
}

// pinned is one session opened with open_session: a connection that stays
// with the host until close_session (or host exit). Statements on it run
// one at a time, in the order their frames arrived (sem has capacity 1).
type pinned struct {
	sess backend.Session
	id   uint64
	sem  chan struct{}
	done chan struct{} // closed by close_session

	mu     sync.Mutex
	cur    context.CancelFunc // cancel of the statement holding sem
	closed bool
}

func New(be backend.Backend) *Server {
	return &Server{be: be, out: make(chan []byte, 64), qs: map[uint32]*query{}, sess: map[uint32]*pinned{}}
}

func (s *Server) send(t byte, qid uint32, p []byte) { s.out <- proto.Encode(t, qid, p) }

func (s *Server) sendErr(qid uint32, kind byte, code uint32, msg string) {
	s.send(proto.TypeError, qid, proto.EncodeError(kind, code, msg))
}

func (s *Server) sendFailure(qid uint32, err error) {
	f := s.be.Classify(err)
	s.sendErr(qid, f.Kind, f.Code, err.Error())
}

// Run serves the protocol until in reaches EOF (or a protocol error), then
// cancels every query, waits for the server-side KILLs and returns.
// The ready frame has already been written by the caller.
func (s *Server) Run(in io.Reader, out io.Writer) {
	writerDone := make(chan struct{})
	go func() { // single stdout writer
		defer close(writerDone)
		w := bufio.NewWriterSize(out, 1<<16)
		for f := range s.out {
			w.Write(f)
			if len(s.out) == 0 {
				w.Flush()
			}
		}
		w.Flush()
	}()

	r := bufio.NewReaderSize(in, 1<<16) // single stdin reader: never blocks on query work
	for {
		f, err := proto.ReadFrame(r, proto.MaxInbound)
		if err != nil {
			break
		}
		s.mu.Lock()
		q := s.qs[f.QID]
		s.mu.Unlock()
		switch f.Type {
		case proto.TypeQuery:
			timeoutMS, batchRows, sqlText, err := proto.DecodeQuery(f.Payload)
			if err != nil {
				s.sendErr(f.QID, proto.KindInternal, 0, "bad query frame")
				continue
			}
			s.start(f.QID, q, proto.Run{TimeoutMS: timeoutMS, BatchRows: batchRows, SQL: sqlText})
		case proto.TypeRun:
			r, err := proto.DecodeRun(f.Payload)
			if err != nil {
				s.sendErr(f.QID, proto.KindInternal, 0, "bad run frame: "+err.Error())
				continue
			}
			s.start(f.QID, q, r)
		case proto.TypeOpenSession:
			s.openSession(f.QID)
		case proto.TypeCloseSession:
			s.closeSession(f.QID)
		case proto.TypeCancel:
			if q != nil {
				q.cancelAt.CompareAndSwap(0, time.Now().UnixNano())
				q.cancel()
			}
		case proto.TypeCredit:
			if n, err := proto.DecodeU32(f.Payload); err == nil && q != nil {
				select {
				case q.credits <- n:
				default:
				}
			}
		case proto.TypeKillSession:
			id, err := proto.DecodeU64(f.Payload)
			if err != nil {
				s.sendErr(f.QID, proto.KindInternal, 0, "bad kill-session frame")
				continue
			}
			s.wg.Add(1)
			go func(qid uint32, id uint64) {
				defer s.wg.Done()
				ctx, cancel := context.WithTimeout(context.Background(), killTimeout)
				defer cancel()
				if err := s.be.KillSession(ctx, id); err != nil {
					s.sendFailure(qid, err)
				} else {
					s.send(proto.TypeDone, qid, proto.EncodeU64(0))
				}
			}(f.QID, id)
		}
	}
	// Host is gone (stdin EOF). Cancel everything: each query's AfterFunc
	// issues KILL QUERY on the control connection. Wait for them, then stop.
	s.mu.Lock()
	for _, q := range s.qs {
		q.cancel()
	}
	s.mu.Unlock()
	s.closeAllSessions()
	done := make(chan struct{})
	go func() { s.wg.Wait(); close(done) }()
	select {
	case <-done:
	case <-time.After(exitGrace):
	}
	close(s.out)
	select {
	case <-writerDone:
	case <-time.After(time.Second):
	}
}

func (s *Server) start(qid uint32, existing *query, r proto.Run) {
	if existing != nil {
		s.sendErr(qid, proto.KindInternal, 0, "query id already in use")
		return
	}
	var pin *pinned
	if r.Session != 0 {
		s.mu.Lock()
		pin = s.sess[r.Session]
		s.mu.Unlock()
		if pin == nil {
			s.sendErr(qid, proto.KindInternal, 0, "unknown or closed session")
			return
		}
	}
	ctx, cancel := context.WithCancel(context.Background())
	if r.TimeoutMS > 0 {
		ctx, cancel = context.WithTimeout(context.Background(), time.Duration(r.TimeoutMS)*time.Millisecond)
	}
	batchRows := r.BatchRows
	if batchRows == 0 {
		batchRows = defaultBatchRows
	}
	q := &query{cancel: cancel, credits: make(chan uint32, 1024)}
	s.mu.Lock()
	s.qs[qid] = q
	s.mu.Unlock()
	s.wg.Add(1)
	go s.run(ctx, qid, q, r.SQL, r.Params, pin, int(batchRows))
}

// openSession takes one connection out of the pool for the host. The
// frame's qid is the session id; the answer is started (server session id)
// and done, or an error.
func (s *Server) openSession(qid uint32) {
	s.mu.Lock()
	_, dup := s.sess[qid]
	_, busy := s.qs[qid]
	s.mu.Unlock()
	if dup || busy {
		s.sendErr(qid, proto.KindInternal, 0, "id already in use")
		return
	}
	s.wg.Add(1)
	go func() {
		defer s.wg.Done()
		ctx, cancel := context.WithTimeout(context.Background(), killTimeout)
		defer cancel()
		sess, err := s.be.Acquire(ctx)
		if err != nil {
			s.sendFailure(qid, err)
			return
		}
		id, err := sess.ID(ctx)
		if err != nil {
			sess.Close(true)
			s.sendFailure(qid, err)
			return
		}
		p := &pinned{sess: sess, id: id, sem: make(chan struct{}, 1), done: make(chan struct{})}
		s.mu.Lock()
		s.sess[qid] = p
		s.mu.Unlock()
		s.send(proto.TypeStarted, qid, proto.EncodeU64(id))
		s.send(proto.TypeDone, qid, proto.EncodeU64(0))
	}()
}

// closeSession ends a pinned session: the statement running on it is
// cancelled, then the connection is closed (discarded), so an open
// transaction is rolled back by the server. No answer is sent.
func (s *Server) closeSession(qid uint32) {
	s.mu.Lock()
	p := s.sess[qid]
	delete(s.sess, qid)
	s.mu.Unlock()
	if p != nil {
		s.wg.Add(1)
		go func() { defer s.wg.Done(); p.shut() }()
	}
}

func (s *Server) closeAllSessions() {
	s.mu.Lock()
	all := s.sess
	s.sess = map[uint32]*pinned{}
	s.mu.Unlock()
	for _, p := range all {
		s.wg.Add(1)
		go func(p *pinned) { defer s.wg.Done(); p.shut() }(p)
	}
}

func (p *pinned) shut() {
	p.mu.Lock()
	if !p.closed {
		close(p.done)
	}
	p.closed = true
	if p.cur != nil {
		p.cur()
	}
	p.mu.Unlock()
	p.sem <- struct{}{} // waits for the running statement to let go
	p.sess.Close(true)
}

func (s *Server) run(ctx context.Context, qid uint32, q *query, sqlText string, params []proto.Param, pin *pinned, batchRows int) {
	defer s.wg.Done()
	defer func() { s.mu.Lock(); delete(s.qs, qid); s.mu.Unlock(); q.cancel() }()

	var sess backend.Session
	var id uint64
	var err error
	clean := false // true only when the statement ran to its end
	if pin != nil {
		// One statement at a time on a pinned session; a cancel while
		// waiting ends the wait.
		select {
		case pin.sem <- struct{}{}:
		case <-ctx.Done():
			s.endFailed(ctx, qid, q, ctx.Err(), nil)
			return
		case <-pin.done:
			s.sendErr(qid, proto.KindInternal, 0, "the session was closed")
			return
		}
		defer func() { <-pin.sem }() // registered first, runs last
		pin.mu.Lock()
		if pin.closed {
			pin.mu.Unlock()
			s.sendErr(qid, proto.KindInternal, 0, "the session was closed")
			return
		}
		pin.cur = q.cancel
		pin.mu.Unlock()
		defer func() { pin.mu.Lock(); pin.cur = nil; pin.mu.Unlock() }()
		sess, id = pin.sess, pin.id
	} else {
		if sess, err = s.be.Acquire(ctx); err != nil {
			s.endFailed(ctx, qid, q, err, nil)
			return
		}
		defer func() { sess.Close(!clean) }()
		if id, err = sess.ID(ctx); err != nil {
			s.endFailed(ctx, qid, q, err, nil)
			return
		}
	}

	// Cancel, deadline and host exit all end in ctx.Done -> server-side KILL
	// QUERY on the control connection, because closing the client socket
	// leaves the statement running on the server.
	var finished atomic.Bool
	killed := make(chan struct{})
	stop := context.AfterFunc(ctx, func() {
		defer close(killed)
		if finished.Load() {
			return
		}
		kctx, cancel := context.WithTimeout(context.Background(), killTimeout)
		defer cancel()
		q.killErr = s.be.KillQuery(kctx, id)
	})
	defer func() {
		finished.Store(true)
		if !stop() {
			<-killed
		}
	}()
	s.send(proto.TypeStarted, qid, proto.EncodeU64(id))

	// A pinned MySQL session must survive a cancel: the driver call gets a
	// context that is never cancelled and the KILL QUERY above stops the
	// statement (the driver would close the connection otherwise and take
	// the transaction with it).
	qctx := ctx
	if pin != nil && !s.be.CancelsWithContext() {
		qctx = context.WithoutCancel(ctx)
	}
	rows, err := sess.Query(qctx, sqlText, params)
	if err != nil {
		s.endFailed(ctx, qid, q, err, killed)
		return
	}
	defer rows.Close()

	var credits uint32
	var total uint64
	batch := proto.NewBatch(1 << 16)
	// flush sends the current batch once the host has granted a credit.
	// False means the query was cancelled while waiting.
	flush := func() bool {
		for credits == 0 {
			select {
			case n := <-q.credits:
				credits += n
			case <-ctx.Done():
				return false
			}
		}
		credits--
		p := batch.Bytes()
		if len(p) > proto.MaxFrame {
			err = errRowTooLarge
			return false
		}
		s.send(proto.TypeBatch, qid, p)
		batch = proto.NewBatch(1 << 16)
		return true
	}
	// One result per statement (or per result set of a CALL):
	// columns + batches, or affected for a statement without a result set.
	for err == nil && rows.NextResult() {
		cols := rows.Columns()
		if len(cols) == 0 {
			s.send(proto.TypeAffected, qid, proto.EncodeU64(rows.Affected()))
			continue
		}
		s.send(proto.TypeColumns, qid, proto.EncodeColumns(cols))
		for rows.Next() {
			var row [][]byte
			if row, err = rows.Row(); err != nil {
				break
			}
			batch.AddRow(row)
			total++
			if (batch.Rows() >= batchRows || batch.Len() >= batchBytes) && !flush() {
				break
			}
		}
		if err == nil {
			err = rows.Err()
		}
		// The rows of this result set go out before the next Columns.
		// A cancel that arrives after the last row was sent still ends in done.
		if err == nil && batch.Rows() > 0 {
			if ctx.Err() != nil || !flush() {
				if err == nil {
					err = ctx.Err()
				}
			}
		}
	}
	if err == nil {
		err = rows.Err()
	}
	if err != nil {
		s.endFailed(ctx, qid, q, err, killed)
		return
	}
	clean = true
	s.send(proto.TypeDone, qid, proto.EncodeU64(total))
}

type rowTooLarge struct{}

func (rowTooLarge) Error() string { return "a batch exceeds the maximum frame size" }

var errRowTooLarge = rowTooLarge{}

// endFailed sends the terminal frame for a query that did not finish:
// cancelled (after the server-side KILL), timeout, or an error.
func (s *Server) endFailed(ctx context.Context, qid uint32, q *query, err error, killed <-chan struct{}) {
	if ctx.Err() == nil {
		if _, ok := err.(rowTooLarge); ok {
			s.sendErr(qid, proto.KindInternal, 0, err.Error())
			return
		}
		s.sendFailure(qid, err)
		return
	}
	if killed != nil {
		<-killed
	}
	if q.killErr != nil {
		s.sendErr(qid, proto.KindInternal, 0, "could not stop the query on the server: "+q.killErr.Error())
	} else if at := q.cancelAt.Load(); at != 0 {
		s.send(proto.TypeCancelled, qid, proto.EncodeU32(uint32((time.Now().UnixNano()-at)/1000)))
	} else if ctx.Err() == context.DeadlineExceeded {
		s.sendErr(qid, proto.KindTimeout, 0, "timeout: "+ctx.Err().Error())
	} else {
		// Cancelled by the shutdown path; nobody is listening.
		s.send(proto.TypeCancelled, qid, proto.EncodeU32(0))
	}
}
