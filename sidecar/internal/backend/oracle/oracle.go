// Package oracle is the Oracle backend (pure Go driver sijms/go-ora, no
// Oracle client install needed).
//
// Connection: SIDECAR_HOST / SIDECAR_PORT plus SIDECAR_DATABASE = the
// service name (not the SID).
//
// TLS modes (TCPS, Oracle's TLS-wrapped listener protocol):
//
//	require             TCPS with certificate and host-name verification
//	require-unverified  TCPS, certificate not checked
//	disabled            plain TCP. Many Oracle listeners only offer plain
//	                    TCP (port 1521); against those, `disabled` is the
//	                    only mode that connects. Oracle's own network
//	                    encryption (sqlnet.ora) may still apply on top of it.
//
// Cancel: the statement's context is handed to the driver; cancelling it
// makes go-ora send a break (urgent TCP byte / INTERRUPT marker) on the
// statement's own connection and the server answers ORA-01013. That is all
// the sidecar does: it never issues ALTER SYSTEM KILL SESSION or any other
// administrative statement, so KillQuery does nothing and KillSession is
// refused. A statement that ignores the break (rare, e.g. in a long
// uninterruptible server phase) keeps running until the break is
// processed; the sidecar discards the connection after a cancel, and the
// server ends the session when the TCP connection drops.
//
// Row locator for edits: ROWID. The host builds `UPDATE t SET c = ? WHERE
// ROWID = ?` and selects `ROWID` with the rows it wants to edit.
package oracle

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"database/sql"
	"database/sql/driver"
	"errors"
	"net"
	"strconv"
	"strings"

	go_ora "github.com/sijms/go-ora/v2"
	"github.com/sijms/go-ora/v2/network"

	"sidecar/internal/backend"
	"sidecar/internal/proto"
)

type db struct {
	pool    *sql.DB
	version string
}

// URL builds the go-ora connection URL for a config. Exported for tests.
func URL(cfg backend.Config) string {
	opts := map[string]string{"CONNECT TIMEOUT": "10"}
	if cfg.RequireTLS {
		opts["SSL"] = "true"
		if cfg.SkipVerify {
			opts["SSL VERIFY"] = "false"
		} else {
			opts["SSL VERIFY"] = "true"
		}
	}
	return go_ora.BuildUrl(cfg.Host, cfg.Port, cfg.Database, cfg.User, cfg.Password, opts)
}

// Open connects and checks the credentials. Nothing is written to the
// database.
func Open(ctx context.Context, cfg backend.Config) (backend.Backend, error) {
	if cfg.Database == "" {
		return nil, errors.New("SIDECAR_DATABASE must hold the Oracle service name")
	}
	d := &db{pool: sql.OpenDB(go_ora.NewConnector(URL(cfg)))}
	var one int
	if err := d.pool.QueryRowContext(ctx, "SELECT 1 FROM DUAL").Scan(&one); err != nil {
		d.Close()
		return nil, err
	}
	// PRODUCT_COMPONENT_VERSION is readable by PUBLIC.
	if err := d.pool.QueryRowContext(ctx, "SELECT VERSION FROM PRODUCT_COMPONENT_VERSION WHERE ROWNUM = 1").Scan(&d.version); err != nil {
		d.version = "Oracle"
	}
	return d, nil
}

func (d *db) Name() string             { return "oracle" }
func (d *db) ServerVersion() string    { return d.version }
func (d *db) CancelsWithContext() bool { return true }
func (d *db) Close() error             { return d.pool.Close() }

func (d *db) Acquire(ctx context.Context) (backend.Session, error) {
	c, err := d.pool.Conn(ctx)
	if err != nil {
		return nil, err
	}
	return &session{c: c}, nil
}

// KillQuery does nothing: the driver breaks the statement on context
// cancel (see the package comment). No administrative statements.
func (d *db) KillQuery(ctx context.Context, id uint64) error { return nil }

// KillSession is refused: it would need ALTER SYSTEM KILL SESSION.
func (d *db) KillSession(ctx context.Context, id uint64) error {
	return errors.New("oracle: killing a session needs administrative rights and is not done; the server ends the session when its connection drops")
}

func (d *db) Classify(err error) backend.Failure { return Classify(err) }

// Classify maps a go-ora error to a failure kind.
func Classify(err error) backend.Failure {
	var oe *network.OracleError
	if errors.As(err, &oe) {
		switch oe.ErrCode {
		case 1017, 1005, 1045, 28000, 28001, 28002, 28003, 28007, 1031: // bad login, locked, expired, no CREATE SESSION
			return backend.Failure{Kind: proto.KindAuth, Code: uint32(oe.ErrCode)}
		case 12154, 12162, 12170, 12504, 12505, 12514, 12516, 12518, 12519, 12520, 12521, 12523, 12526, 12527, 12528, 12537, 12541, 12543, 12545, 12547, 12560:
			return backend.Failure{Kind: proto.KindConnect, Code: uint32(oe.ErrCode)}
		}
		return backend.Failure{Kind: proto.KindQuery, Code: uint32(oe.ErrCode)}
	}
	if isTLS(err) {
		return backend.Failure{Kind: proto.KindTLS}
	}
	var ne net.Error
	var op *net.OpError
	if errors.As(err, &op) || errors.As(err, &ne) {
		return backend.Failure{Kind: proto.KindConnect}
	}
	return backend.Failure{Kind: proto.KindInternal}
}

func isTLS(err error) bool {
	var rh tls.RecordHeaderError
	var cv *tls.CertificateVerificationError
	var ua x509.UnknownAuthorityError
	var hn x509.HostnameError
	var ci x509.CertificateInvalidError
	var al tls.AlertError
	return errors.As(err, &rh) || errors.As(err, &cv) || errors.As(err, &ua) ||
		errors.As(err, &hn) || errors.As(err, &ci) || errors.As(err, &al)
}

// session is one connection. go-ora commits every statement run on a plain
// *sql.Conn, so a host transaction (`START TRANSACTION` / `BEGIN
// TRANSACTION`, the text the host sends for Manual mode and grid Apply)
// opens a *sql.Tx on the connection; statements then run on it until
// `COMMIT` / `ROLLBACK` (with or without WORK) ends it. `ROLLBACK TO
// SAVEPOINT` and any other text goes to the server unchanged. Closing the
// session with a transaction open discards the connection, so the server
// rolls the transaction back.
type session struct {
	c  *sql.Conn
	tx *sql.Tx
}

// TxVerb is the transaction-control meaning of one statement: "begin",
// "commit", "rollback", or "" for an ordinary statement.
func TxVerb(stmt string) string {
	f := strings.Fields(strings.ToUpper(strings.TrimRight(strings.TrimSpace(stmt), "; \t\r\n")))
	switch {
	case len(f) == 2 && (f[0] == "START" || f[0] == "BEGIN") && f[1] == "TRANSACTION":
		return "begin"
	case len(f) == 1 && f[0] == "COMMIT", len(f) == 2 && f[0] == "COMMIT" && f[1] == "WORK":
		return "commit"
	case len(f) == 1 && f[0] == "ROLLBACK", len(f) == 2 && f[0] == "ROLLBACK" && f[1] == "WORK":
		return "rollback"
	}
	return ""
}

// runner is what a statement runs on: the connection, or its open Tx.
type runner interface {
	QueryContext(ctx context.Context, q string, args ...any) (*sql.Rows, error)
	ExecContext(ctx context.Context, q string, args ...any) (sql.Result, error)
}

func (s *session) runner() runner {
	if s.tx != nil {
		return s.tx
	}
	return s.c
}

// control runs a transaction-control statement. A begin inside an open
// transaction keeps it. The Tx gets a background context: cancelling a
// statement must never roll the transaction back.
func (s *session) control(verb string) error {
	switch verb {
	case "begin":
		if s.tx != nil {
			return nil
		}
		tx, err := s.c.BeginTx(context.Background(), nil)
		if err != nil {
			return err
		}
		s.tx = tx
	case "commit", "rollback":
		if s.tx == nil {
			_, err := s.c.ExecContext(context.Background(), strings.ToUpper(verb))
			return err
		}
		tx := s.tx
		s.tx = nil
		if verb == "commit" {
			return tx.Commit()
		}
		return tx.Rollback()
	}
	return nil
}

// ID is the SID of the session (V$SESSION.SID), read with SYS_CONTEXT so no
// privileges on V$ views are needed. The serial number (needed only for
// ALTER SYSTEM KILL SESSION) is not carried.
func (s *session) ID(ctx context.Context) (uint64, error) {
	var sid string
	if err := s.c.QueryRowContext(ctx, "SELECT SYS_CONTEXT('USERENV', 'SID') FROM DUAL").Scan(&sid); err != nil {
		return 0, err
	}
	return strconv.ParseUint(strings.TrimSpace(sid), 10, 64)
}

func (s *session) Query(ctx context.Context, q string, params []proto.Param) (backend.Rows, error) {
	var stmts []string
	var args []any
	if len(params) > 0 {
		stmts = []string{PrepareStatement(BindPlaceholders(q))}
		args = make([]any, len(params))
		for i, p := range params {
			switch p.Kind {
			case proto.ParamText:
				args[i] = string(p.Data)
			case proto.ParamBytes:
				args[i] = append([]byte{}, p.Data...)
			}
		}
	} else {
		stmts = SplitScript(q)
	}
	return &results{ctx: ctx, sess: s, stmts: stmts, args: args}, nil
}

func (s *session) Close(discard bool) {
	if s.tx != nil {
		discard = true // never pool a connection with an open transaction
		s.tx = nil
	}
	if discard {
		_ = s.c.Raw(func(any) error { return driver.ErrBadConn })
	}
	_ = s.c.Close()
}

// results runs the statements of a script one after the other on one
// connection. A SELECT / WITH streams rows; anything else is executed and
// reports its affected row count.
type results struct {
	ctx      context.Context
	sess     *session
	stmts    []string
	args     []any
	next     int
	cur      *sql.Rows
	cols     []proto.Column
	types    []string
	raw      []any
	ptr      []any
	out      [][]byte
	affected uint64
	err      error
}

func (r *results) Columns() []proto.Column { return r.cols }
func (r *results) Affected() uint64        { return r.affected }
func (r *results) Err() error              { return r.err }

func (r *results) Close() error {
	if r.cur != nil {
		err := r.cur.Close()
		r.cur = nil
		return err
	}
	return nil
}

func (r *results) NextResult() bool {
	if r.err != nil {
		return false
	}
	r.cols, r.affected = nil, 0
	if r.cur != nil {
		r.cur.Close()
		r.cur = nil
	}
	if r.next >= len(r.stmts) {
		return false
	}
	stmt := r.stmts[r.next]
	r.next++
	if verb := TxVerb(stmt); verb != "" && len(r.args) == 0 {
		if err := r.sess.control(verb); err != nil {
			r.err = err
			return false
		}
		return true
	}
	conn := r.sess.runner()
	if IsQuery(stmt) {
		rows, err := conn.QueryContext(r.ctx, stmt, r.args...)
		if err != nil {
			r.err = err
			return false
		}
		r.cur = rows
		r.setRows()
		return true
	}
	res, err := conn.ExecContext(r.ctx, stmt, r.args...)
	if err != nil {
		r.err = err
		return false
	}
	if n, err := res.RowsAffected(); err == nil && n > 0 {
		r.affected = uint64(n)
	}
	return true
}

func (r *results) setRows() {
	names, _ := r.cur.Columns()
	cts, _ := r.cur.ColumnTypes()
	cols := make([]proto.Column, len(names))
	r.types = make([]string, len(names))
	for i, n := range names {
		cols[i] = proto.Column{Name: n, Nullable: proto.NullUnknown}
		if i < len(cts) {
			cols[i].Type = cts[i].DatabaseTypeName()
			r.types[i] = cols[i].Type
			if nullable, ok := cts[i].Nullable(); ok {
				cols[i].Nullable = proto.NotNull
				if nullable {
					cols[i].Nullable = proto.Nullable
				}
			}
		}
	}
	r.cols = cols
	r.raw = make([]any, len(cols))
	r.ptr = make([]any, len(cols))
	r.out = make([][]byte, len(cols))
	for i := range r.raw {
		r.ptr[i] = &r.raw[i]
	}
}

func (r *results) Next() bool {
	if r.cur == nil || len(r.cols) == 0 {
		return false
	}
	if r.cur.Next() {
		return true
	}
	if err := r.cur.Err(); err != nil {
		r.err = err
	}
	return false
}

func (r *results) Row() ([][]byte, error) {
	for i := range r.raw {
		r.raw[i] = nil
	}
	if err := r.cur.Scan(r.ptr...); err != nil {
		return nil, err
	}
	for i, v := range r.raw {
		r.out[i] = Text(v, r.types[i])
	}
	return r.out, nil
}
