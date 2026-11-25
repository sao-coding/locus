// Package mssql is the Microsoft SQL Server backend (microsoft/go-mssqldb).
//
// A script is split into batches on GO lines (SplitBatches); each batch goes
// to the server as one SQL batch, so DECLAREd variables and multi-statement
// batches behave as in SSMS. The results of a batch are read through the
// driver's message queue (sqlexp.ReturnMessage), which reports every result
// set and every row count in server order.
//
// Cancel: the driver sends a TDS attention when the context is cancelled and
// waits for the server's acknowledgement. Only the running statement stops;
// the connection and an open transaction stay (unless the session runs with
// XACT_ABORT ON, where the server rolls the transaction back).
package mssql

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"database/sql"
	"database/sql/driver"
	"errors"
	"fmt"
	"net"
	"net/url"
	"strconv"
	"strings"
	"time"

	"github.com/golang-sql/sqlexp"
	mssqldb "github.com/microsoft/go-mssqldb"

	"sidecar/internal/backend"
	"sidecar/internal/proto"
)

type db struct {
	pool, ctl *sql.DB
	version   string
}

// URL is the go-mssqldb connection string for cfg.
func URL(cfg backend.Config) string {
	q := url.Values{}
	if cfg.Database != "" {
		q.Set("database", cfg.Database)
	}
	q.Set("app name", "locus")
	q.Set("dial timeout", "10")
	q.Set("connection timeout", "0") // per-call deadlines come from the context
	switch {
	case cfg.RequireTLS && cfg.SkipVerify:
		q.Set("encrypt", "true")
		q.Set("TrustServerCertificate", "true")
	case cfg.RequireTLS:
		q.Set("encrypt", "true")
		q.Set("TrustServerCertificate", "false")
	default:
		// "false": only the login packet is encrypted. A server that forces
		// encryption still gets it.
		q.Set("encrypt", "false")
	}
	u := url.URL{
		Scheme:   "sqlserver",
		User:     url.UserPassword(cfg.User, cfg.Password),
		Host:     net.JoinHostPort(cfg.Host, strconv.Itoa(cfg.Port)),
		RawQuery: q.Encode(),
	}
	return u.String()
}

// Open connects the query pool and the control pool and fails if either
// cannot reach the server, so credential and TLS errors show up at startup.
func Open(ctx context.Context, cfg backend.Config) (backend.Backend, error) {
	conn, err := mssqldb.NewConnector(URL(cfg))
	if err != nil {
		return nil, err
	}
	d := &db{pool: sql.OpenDB(conn), ctl: sql.OpenDB(conn)}
	d.ctl.SetMaxOpenConns(2)
	if err := d.pool.QueryRowContext(ctx,
		"SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128))").Scan(&d.version); err != nil {
		d.Close()
		return nil, err
	}
	if err := d.ctl.PingContext(ctx); err != nil {
		d.Close()
		return nil, err
	}
	return d, nil
}

func (d *db) Name() string             { return "mssql" }
func (d *db) ServerVersion() string    { return d.version }
func (d *db) CancelsWithContext() bool { return true }

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

// KillQuery does nothing: cancelling the context sends the attention.
func (d *db) KillQuery(ctx context.Context, id uint64) error { return nil }

// KillSession ends a whole server session. Needs ALTER ANY CONNECTION; error
// 6106 (no such session) counts as success.
func (d *db) KillSession(ctx context.Context, id uint64) error {
	_, err := d.ctl.ExecContext(ctx, fmt.Sprintf("KILL %d", id))
	var me mssqldb.Error
	if errors.As(err, &me) && me.Number == 6106 {
		return nil
	}
	return err
}

func (d *db) Classify(err error) backend.Failure { return Classify(err) }

// Classify maps a go-mssqldb error to a failure kind.
func Classify(err error) backend.Failure {
	var me mssqldb.Error
	if errors.As(err, &me) {
		switch me.Number {
		// login failed, untrusted domain, password expired / must change,
		// account locked, cannot open the database of the login
		case 18456, 18452, 18486, 18487, 18488, 4060:
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
	if s := strings.ToLower(err.Error()); strings.Contains(s, "unable to open tcp connection") ||
		strings.Contains(s, "login error") {
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
	if errors.As(err, &rh) || errors.As(err, &cv) || errors.As(err, &ua) ||
		errors.As(err, &hn) || errors.As(err, &ci) || errors.As(err, &al) {
		return true
	}
	s := strings.ToLower(err.Error())
	return strings.Contains(s, "tls handshake") || strings.Contains(s, "certificate")
}

// session is one pooled connection. Transactions are plain T-SQL
// (BEGIN TRAN / COMMIT / ROLLBACK) on the pinned connection; database/sql
// never sees them, so a cancelled statement cannot roll them back.
type session struct{ c *sql.Conn }

func (s *session) ID(ctx context.Context) (id uint64, err error) {
	var n int64
	err = s.c.QueryRowContext(ctx, "SELECT CAST(@@SPID AS bigint)").Scan(&n)
	return uint64(n), err
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
		return &results{ctx: ctx, conn: s.c, batches: []string{BindPlaceholders(q)}, args: args}, nil
	}
	return &results{ctx: ctx, conn: s.c, batches: SplitBatches(q)}, nil
}

func (s *session) Close(discard bool) {
	if discard {
		_ = s.c.Raw(func(any) error { return driver.ErrBadConn })
	}
	_ = s.c.Close()
}

// results walks the batches of a script; per batch, the driver's message
// queue gives the result sets and row counts in order.
type results struct {
	ctx     context.Context
	conn    *sql.Conn
	batches []string
	args    []any
	next    int // index of the next batch to run

	cur       *sql.Rows
	msg       *sqlexp.ReturnMessage
	inResult  bool // the current item is a result set
	afterRows bool // a result set just ended: its row count is not an item
	cols      []proto.Column
	types     []string
	raw       []any
	ptr       []any
	out       [][]byte
	affected  uint64
	err       error
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

func (r *results) fail(err error) bool {
	if r.err == nil {
		r.err = err
	}
	return false
}

func (r *results) NextResult() bool {
	if r.err != nil {
		return false
	}
	if r.inResult {
		// Rows the caller did not read are skipped by the driver.
		r.inResult, r.afterRows = false, true
	}
	r.cols, r.affected = nil, 0
	for {
		if r.cur == nil {
			if r.next >= len(r.batches) {
				return false
			}
			b := r.batches[r.next]
			r.next++
			r.msg = &sqlexp.ReturnMessage{}
			args := append(append([]any{}, r.args...), r.msg)
			rows, err := r.conn.QueryContext(r.ctx, b, args...)
			if err != nil {
				return r.fail(err)
			}
			r.cur, r.afterRows = rows, false
		}
		switch m := r.msg.Message(r.ctx).(type) {
		case sqlexp.MsgNext:
			r.setRows()
			r.inResult = true
			return true
		case sqlexp.MsgRowsAffected:
			if r.afterRows {
				r.afterRows = false // the count of the SELECT just read
				continue
			}
			if m.Count > 0 {
				r.affected = uint64(m.Count)
			}
			return true
		case sqlexp.MsgNextResultSet:
			r.afterRows = false
			if !r.cur.NextResultSet() {
				err := r.cur.Err()
				r.cur.Close()
				r.cur = nil
				if err != nil {
					return r.fail(err)
				}
			}
		case sqlexp.MsgError:
			return r.fail(m.Error)
		case sqlexp.MsgNotice:
			// PRINT / informational messages: not part of the protocol yet.
		default:
			// The queue ended (context cancelled or connection gone).
			err := r.cur.Err()
			if err == nil {
				err = r.ctx.Err()
			}
			r.cur.Close()
			r.cur = nil
			if err != nil {
				return r.fail(err)
			}
		}
	}
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
	if r.cur == nil || !r.inResult {
		return false
	}
	if r.cur.Next() {
		return true
	}
	r.inResult, r.afterRows = false, true
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

// Text converts one scanned value to the bytes the protocol carries. A nil
// result is NULL.
//
//	DECIMAL / MONEY    the exact decimal text
//	UNIQUEIDENTIFIER   8-4-4-4-12 upper-case hex (the driver gives 16 raw bytes)
//	DATE               2006-01-02
//	TIME               15:04:05[.fraction]
//	DATETIME*          ISO 8601 without zone; DATETIMEOFFSET with offset
//	BINARY / IMAGE     the bytes
func Text(v any, typeName string) []byte {
	switch x := v.(type) {
	case nil:
		return nil
	case string:
		return []byte(x)
	case []byte:
		if x == nil {
			return nil
		}
		if typeName == "UNIQUEIDENTIFIER" && len(x) == 16 {
			var g mssqldb.UniqueIdentifier
			if g.Scan(x) == nil {
				return []byte(g.String())
			}
		}
		return append([]byte{}, x...)
	case int64:
		return strconv.AppendInt(nil, x, 10)
	case float64:
		return strconv.AppendFloat(nil, x, 'g', -1, 64)
	case float32:
		return strconv.AppendFloat(nil, float64(x), 'g', -1, 32)
	case bool:
		if x {
			return []byte("1")
		}
		return []byte("0")
	case time.Time:
		switch typeName {
		case "DATE":
			return []byte(x.Format("2006-01-02"))
		case "TIME":
			return []byte(x.Format("15:04:05.9999999"))
		case "DATETIMEOFFSET":
			return []byte(x.Format("2006-01-02T15:04:05.9999999Z07:00"))
		}
		return []byte(x.Format("2006-01-02T15:04:05.9999999"))
	case driver.Valuer:
		if dv, err := x.Value(); err == nil {
			return Text(dv, typeName)
		}
	}
	return []byte(fmt.Sprint(v))
}
