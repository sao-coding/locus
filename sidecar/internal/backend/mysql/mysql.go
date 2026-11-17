// Package mysql is the MySQL / MariaDB backend (go-sql-driver/mysql).
package mysql

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"database/sql"
	"database/sql/driver"
	"errors"
	"fmt"
	"net"
	"strconv"
	"time"

	my "github.com/go-sql-driver/mysql"

	"sidecar/internal/backend"
	"sidecar/internal/proto"
)

type db struct {
	pool, ctl *sql.DB
	version   string
}

// Open connects both the query pool and the control pool and fails if either
// cannot reach the server, so credential and TLS errors show up at startup.
func Open(ctx context.Context, cfg backend.Config) (backend.Backend, error) {
	c := my.NewConfig()
	c.User = cfg.User
	c.Passwd = cfg.Password
	c.Net = "tcp"
	c.Addr = net.JoinHostPort(cfg.Host, strconv.Itoa(cfg.Port))
	c.DBName = cfg.Database
	c.Timeout = 10 * time.Second
	// "true" = TLS with certificate and host name verification, no fallback
	// to plain text. "false" = plain text.
	// "skip-verify" = TLS required, certificate not checked.
	if cfg.RequireTLS && cfg.SkipVerify {
		c.TLSConfig = "skip-verify"
	} else if cfg.RequireTLS {
		c.TLSConfig = "true"
	} else {
		c.TLSConfig = "false"
	}
	conn, err := my.NewConnector(c)
	if err != nil {
		return nil, err
	}
	d := &db{pool: sql.OpenDB(conn), ctl: sql.OpenDB(conn)}
	d.ctl.SetMaxOpenConns(2)
	if err := d.pool.QueryRowContext(ctx, "SELECT VERSION()").Scan(&d.version); err != nil {
		d.Close()
		return nil, err
	}
	if err := d.ctl.PingContext(ctx); err != nil {
		d.Close()
		return nil, err
	}
	return d, nil
}

func (d *db) Name() string          { return "mysql" }
func (d *db) ServerVersion() string { return d.version }

func (d *db) Close() error {
	d.pool.Close()
	return d.ctl.Close()
}

func (d *db) Acquire(ctx context.Context) (backend.Session, error) {
	c, err := d.pool.Conn(ctx)
	if err != nil {
		return nil, err
	}
	return &session{c: c}, nil
}

func (d *db) KillQuery(ctx context.Context, id uint64) error {
	return ignoreGone(d.ctl.ExecContext(ctx, fmt.Sprintf("KILL QUERY %d", id)))
}

func (d *db) KillSession(ctx context.Context, id uint64) error {
	return ignoreGone(d.ctl.ExecContext(ctx, fmt.Sprintf("KILL %d", id)))
}

// ignoreGone treats "Unknown thread id" (1094) as success: the session is
// already gone, which is what the caller wanted.
func ignoreGone(_ sql.Result, err error) error {
	var me *my.MySQLError
	if errors.As(err, &me) && me.Number == 1094 {
		return nil
	}
	return err
}

func (d *db) CancelsWithContext() bool { return false }

func (d *db) Classify(err error) backend.Failure { return Classify(err) }

// Classify maps a go-sql-driver error to a failure kind.
func Classify(err error) backend.Failure {
	var me *my.MySQLError
	if errors.As(err, &me) {
		switch me.Number {
		case 1044, 1045, 1698, 1130, 1251, 1862: // access denied, host not allowed, auth plugin, expired password
			return backend.Failure{Kind: proto.KindAuth, Code: uint32(me.Number)}
		}
		return backend.Failure{Kind: proto.KindQuery, Code: uint32(me.Number)}
	}
	if isTLS(err) {
		return backend.Failure{Kind: proto.KindTLS}
	}
	var ne net.Error
	var oe *net.OpError
	if errors.As(err, &oe) || errors.As(err, &ne) {
		return backend.Failure{Kind: proto.KindConnect}
	}
	return backend.Failure{Kind: proto.KindInternal}
}

func isTLS(err error) bool {
	if errors.Is(err, my.ErrNoTLS) {
		return true
	}
	var rh tls.RecordHeaderError
	var cv *tls.CertificateVerificationError
	var ua x509.UnknownAuthorityError
	var hn x509.HostnameError
	var ci x509.CertificateInvalidError
	var al tls.AlertError
	return errors.As(err, &rh) || errors.As(err, &cv) || errors.As(err, &ua) ||
		errors.As(err, &hn) || errors.As(err, &ci) || errors.As(err, &al)
}

type session struct{ c *sql.Conn }

func (s *session) ID(ctx context.Context) (id uint64, err error) {
	err = s.c.QueryRowContext(ctx, "SELECT CONNECTION_ID()").Scan(&id)
	return
}

func (s *session) Query(ctx context.Context, q string, params []proto.Param) (backend.Rows, error) {
	if len(params) > 0 {
		// One statement, values bound (never spliced into the text).
		args := make([]any, len(params))
		for i, p := range params {
			switch p.Kind {
			case proto.ParamText:
				args[i] = string(p.Data)
			case proto.ParamBytes:
				args[i] = append([]byte{}, p.Data...)
			}
		}
		return &results{ctx: ctx, conn: s.c, stmts: []string{q}, args: args}, nil
	}
	return &results{ctx: ctx, conn: s.c, stmts: splitStatements(q)}, nil
}

func (s *session) Close(discard bool) {
	if discard {
		// Tell database/sql the connection is unusable so it is closed
		// instead of going back to the pool.
		_ = s.c.Raw(func(any) error { return driver.ErrBadConn })
	}
	_ = s.c.Close()
}

// results runs the statements of a script one after the other on one
// connection. Per statement: rows.NextResultSet() walks the result sets of a
// CALL; a statement without a result set reports ROW_COUNT().
//
// Why not multiStatements: go-sql-driver skips statements without a result
// set in NextResultSet and only reports their affected rows as one list for
// Exec, so a multi-statement query cannot say "3 rows inserted" between two
// result sets. Splitting on the client (same rules as db_core::sql) keeps
// the order and the counts.
type results struct {
	ctx      context.Context
	conn     *sql.Conn
	stmts    []string
	args     []any // bound values of a single parameterised statement
	next     int   // index of the next statement to run
	cur      *sql.Rows
	cols     []proto.Column
	raw      []sql.RawBytes
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
		if r.cur.NextResultSet() {
			r.setRows()
			return true
		}
		if err := r.cur.Err(); err != nil {
			r.err = err
			return false
		}
		r.cur.Close()
		r.cur = nil
	}
	if r.next >= len(r.stmts) {
		return false
	}
	stmt := r.stmts[r.next]
	r.next++
	rows, err := r.conn.QueryContext(r.ctx, stmt, r.args...)
	if err != nil {
		r.err = err
		return false
	}
	r.cur = rows
	if names, _ := rows.Columns(); len(names) > 0 {
		r.setRows()
		return true
	}
	// No result set: INSERT / UPDATE / DDL. Read the OK packet's count.
	rows.Close()
	r.cur = nil
	var n int64
	if err := r.conn.QueryRowContext(r.ctx, "SELECT ROW_COUNT()").Scan(&n); err != nil {
		r.err = err
		return false
	}
	if n > 0 {
		r.affected = uint64(n)
	}
	return true
}

// setRows prepares the column list and scan buffers for the current result set.
func (r *results) setRows() {
	names, _ := r.cur.Columns()
	types, _ := r.cur.ColumnTypes()
	cols := make([]proto.Column, len(names))
	for i, n := range names {
		cols[i] = proto.Column{Name: n, Nullable: proto.NullUnknown}
		if i < len(types) {
			cols[i].Type = types[i].DatabaseTypeName()
			if nullable, ok := types[i].Nullable(); ok {
				cols[i].Nullable = proto.NotNull
				if nullable {
					cols[i].Nullable = proto.Nullable
				}
			}
		}
	}
	r.cols = cols
	r.raw = make([]sql.RawBytes, len(cols))
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
	if err := r.cur.Scan(r.ptr...); err != nil {
		return nil, err
	}
	for i, c := range r.raw {
		if c == nil {
			r.out[i] = nil
		} else if len(c) == 0 {
			r.out[i] = []byte{} // empty string, not NULL
		} else {
			r.out[i] = c
		}
	}
	return r.out, nil
}
