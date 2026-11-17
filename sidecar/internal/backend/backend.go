// Package backend is the database-specific part of the sidecar. The protocol
// code (package server) only talks to these interfaces, so another driver
// (go-ora, clickhouse-go) is a new package plus one line in main.go.
package backend

import (
	"context"

	"sidecar/internal/proto"
)

// Config comes from environment variables only (see main.go).
type Config struct {
	Host     string
	Port     int
	User     string
	Password string
	Database string
	// RequireTLS: refuse to connect without TLS. False means plain text.
	RequireTLS bool
	// SkipVerify: with RequireTLS, encrypt but do not check the server
	// certificate (self-signed or private CA).
	SkipVerify bool
}

// Failure is a classified error: Kind is one of proto.Kind*, Code the server
// error number (0 = none).
type Failure struct {
	Kind byte
	Code uint32
}

// Opener connects to the database and returns a ready Backend, or an error
// that Classify of the same driver understands.
type Opener func(ctx context.Context, cfg Config) (Backend, error)

// Backend is one logical database connection pool plus a control connection.
type Backend interface {
	// Name is a short id, e.g. "mysql".
	Name() string
	// ServerVersion is shown in the ready frame.
	ServerVersion() string
	// Acquire returns a dedicated connection for one query.
	Acquire(ctx context.Context) (Session, error)
	// KillQuery stops the statement running on a session, on the control
	// connection (never the session's own).
	KillQuery(ctx context.Context, id uint64) error
	// KillSession ends a whole server session.
	KillSession(ctx context.Context, id uint64) error
	// Classify maps a driver error to a kind and a server code.
	Classify(err error) Failure
	// CancelsWithContext reports whether the driver itself stops a running
	// statement when its context is cancelled (Oracle: a break on the
	// connection) and leaves the connection usable. False (MySQL): cancel is
	// a KILL QUERY on the control connection, and the driver call must not
	// see the cancelled context on a pinned session, because the driver
	// would close the connection and lose the open transaction.
	CancelsWithContext() bool
	Close() error
}

// Session is one connection that runs one statement at a time.
type Session interface {
	// ID is the server's session id, reported in the started frame.
	ID(ctx context.Context) (uint64, error)
	// Query runs sql. With params it is one statement with the values bound
	// in order (`?` placeholders in the text, whatever the database's own
	// syntax is); without, it may be a script.
	Query(ctx context.Context, sql string, params []proto.Param) (Rows, error)
	// Close returns the connection. discard=true drops it instead of
	// pooling it (after a cancel, timeout or error).
	Close(discard bool)
}

// Rows is the forward-only result of one call. A call may hold several
// results (a script of several statements, a CALL with several result
// sets): NextResult moves to each in order. Err reports the first error of
// NextResult or Next; results delivered before it stay valid.
type Rows interface {
	// NextResult advances to the next result (the first call selects the
	// first one). False at the end or after an error (see Err).
	NextResult() bool
	// Columns of the current result; empty when the statement has no result
	// set (then Affected holds its row count and Next is false).
	Columns() []proto.Column
	// Affected is the row count of a statement without a result set.
	Affected() uint64
	Next() bool
	// Row returns the current row; a nil cell is NULL. The cells are valid
	// until the next call to Next.
	Row() ([][]byte, error)
	Err() error
	Close() error
}
