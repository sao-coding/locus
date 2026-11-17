# Sidecar protocol (version 1)

The sidecar is a child process of the host. It serves one database over its
stdin / stdout. Code: `internal/proto` (encoding, with unit tests),
`internal/server` (protocol loop), `internal/backend` (database interface; `mysql`, `oracle`),
host side: `crates/db-sidecar-client`.

stderr is not used for protocol data and is never read by the host. Nothing
secret is ever written to it.

## Process start

Settings come from **environment variables only** (never argv, never logged).
`SIDECAR_PASSWORD` is removed from the sidecar's own environment after it is
read.

| Variable | Meaning |
|---|---|
| `SIDECAR_DRIVER` | `mysql` (default) or `oracle`. A new database = a new package under `internal/backend` and one entry in `main.go`. |
| `SIDECAR_HOST`, `SIDECAR_PORT` | required |
| `SIDECAR_USER`, `SIDECAR_PASSWORD` | credentials |
| `SIDECAR_DATABASE` | default database, optional. Oracle: the **service name** (required). |
| `SIDECAR_TLS` | `require` (default, also when unset), `require-unverified` or `disabled`. `require-unverified` encrypts but does not check the server certificate (self-signed or private CA). `require` means TLS with certificate and host-name verification and **no fallback to plain text**: a server without TLS fails the connect with kind `tls`. |

Startup = **hello / ready exchange** (frames with `qid = 0`):

1. The sidecar connects to the database (query pool and control connection)
   and checks the credentials, within 15 s.
2. Success: it writes one `ready` frame and then serves requests.
3. Failure: it writes one `error` frame (kind `connect`, `auth`, `tls` or
   `internal`) and exits with status 1.

The host sends nothing before `ready`. `SidecarDriver::spawn` resolves only
after `ready`, so connect / auth / TLS errors surface there as the matching
`ErrorKind`.

## Frame

All integers are little endian.

```
[u32 len][u8 type][u32 qid][payload]      len = 5 + len(payload)
```

`qid` is chosen by the host: one id per request, never reused within a
sidecar process. `qid = 0` is for the startup frames. Payload limits: host ->
sidecar 64 MiB, sidecar -> host 256 MiB. A frame over the limit, or a length
below 5, ends the session (the sidecar exits like on EOF). Unknown frame types
are ignored.

Strings are UTF-8. `str16` = `u16 length` + bytes.

## Host -> sidecar

| Type | Name | Payload |
|---|---|---|
| 1 | `query` | `u32 timeout_ms` (0 = none), `u32 batch_rows` (0 = 1000), `sql` (rest of the payload) |
| 2 | `cancel` | empty. Ignored when the query already ended. |
| 3 | `credit` | `u32 n`: the sidecar may send `n` more batches for this query |
| 4 | `kill_session` | `u64 session_id`. Answered with `done` (rows = 0) or `error` under the same `qid`. Used after a sidecar crash and by `Driver::kill_session`. A session that no longer exists counts as success. |

| 5 | `run` | `u32 session` (0 = pooled connection, else the qid of an `open_session`), `u32 timeout_ms`, `u32 batch_rows`, `u16 nparams`, then per parameter `u8 kind` (0 NULL, 1 text, 2 bytes) and, for kinds 1 and 2, `u32 len` + bytes, then `sql` (rest of the payload). Same answer frames as `query`. Used for `Driver::execute` and for every statement of a pinned session. |
| 6 | `open_session` | empty. Takes one connection out of the pool; this frame's `qid` is the session id from now on. Answered with `started` (server session id) and `done` (rows = 0), or `error`. |
| 7 | `close_session` | empty, `qid` = session id. Cancels the statement running on it, closes and **discards** the connection (the server rolls back an open transaction), no answer. |

A query gets its own goroutine and its own database connection. The reader
goroutine only decodes frames and hands them over, so `cancel` and `credit`
are handled while queries are busy.

### Bound parameters (`run`)

Values travel as data in their own fields and are never spliced into the SQL
text. The text uses `?` placeholders in order; the Oracle backend rewrites
them to `:1`, `:2` (outside strings, quoted identifiers and comments). With
at least one parameter the text is **one statement** (no script splitting);
with none, `run` behaves exactly like `query`. Text values are sent to the
server as strings and converted by it; bytes as binary. Credits, cancel and
timeout work as for `query`.

### Pinned sessions (`open_session` / `run` / `close_session`)

A session is one database connection that stays with the host, for
transactions, session variables and temporary tables. Statements on it
(`run` with `session` ≠ 0) run **one at a time, in the order their frames
arrived**; a second one waits for the first (and can be cancelled while it
waits: it ends `cancelled`). Per statement the rules are those of any query:
credits, `cancel`, timeout. Differences:

* the connection is never returned to the pool after a statement, also not
  after a cancel or an error;
* **cancel keeps the session** (MySQL): the statement is stopped with
  `KILL QUERY` and the driver call itself does not see the cancelled
  context (the driver would close the connection and drop the open
  transaction). An open transaction survives a cancelled statement. On
  Oracle the driver's break does the cancel and the connection normally
  stays usable;
* `close_session`, host exit (stdin EOF) and a crashed host all close the
  connection, so an unfinished transaction is rolled back by the server.
  A `run` on a closed or unknown session ends with `error` (internal).

The host keeps the `SidecarSession` bound to the sidecar process that opened
it. If that process dies, its statements fail and the connection is gone with it.

### Back-pressure

The sidecar sends a `batch` only while it holds a credit; it consumes one per
batch. The host grants `DEFAULT_WINDOW` (4) credits right after `query`, and
one more every time the consumer takes a batch out of the stream. A consumer
that stops reading therefore stops the sidecar after at most 4 unread
batches; the query waits (and can still be cancelled). Measured: no batches
beyond the credit, sidecar memory flat.

A batch also closes early when its payload reaches 4 MiB.

### Cancel, timeout, shutdown

On **Oracle** the sidecar cannot use `KILL QUERY`: it never issues administrative statements. The statement's context is passed to the driver, and cancelling it makes go-ora send a break on that statement's own connection (urgent TCP byte or INTERRUPT marker); the server answers ORA-01013 and the query ends `cancelled`. `kill_session` is refused (`error`, internal): the server ends a session when its connection drops, which happens when the sidecar discards the connection after a cancel, closes a session, or exits.

Cancel, timeout and host exit all end in the same place: the query's context
is cancelled and the sidecar sends `KILL QUERY <session id>` on its **control
connection** (never the query's own: closing the client socket leaves the
statement running on the server). The terminal frame is sent after the kill
has returned. A connection that was cancelled, timed out or failed is
discarded instead of going back to the pool.

**stdin EOF** (the host closed the pipe or died): cancel every query, wait for
the KILLs (at most 3 s), exit with status 0.

Host shutdown order (measured rule): **close stdin first**, wait for the
sidecar to clean up and exit (about 250 ms in the POC, grace 1 s), terminate
only after that. A Job Object with `KILL_ON_JOB_CLOSE` must not be the only
defence: it kills the sidecar before it can send KILL and the statement lives
about 4.9 s longer.

## Sidecar -> host

| Type | Name | Payload |
|---|---|---|
| 20 | `ready` | `u16 protocol_version` (1), `str16 driver`, `str16 server_version`. `qid = 0`. |
| 10 | `started` | `u64 session_id`: the server's connection / session id. The host keeps it for every running query. |
| 11 | `columns` | `u16 count`, then per column: `str16 name`, `str16 type_name`, `u8 nullable` |
| 12 | `batch` | `u32 rows`, then for every row, for every column: `u32 len` + bytes; `len = 0xFFFFFFFF` is NULL (an empty cell is `len = 0`) |
| 13 | `done` | `u64 rows` |
| 14 | `error` | `u8 kind`, `u32 code` (server error number, 0 = none), message (rest of the payload) |
| 15 | `cancelled` | `u32 micros` from the cancel frame until the server-side query was stopped |
| 16 | `affected` | `u64 rows`: a statement without a result set (INSERT, UPDATE, DDL) finished. Sent instead of `columns` + `batch`. |

`nullable`: 0 = NOT NULL, 1 = nullable, 2 = the driver cannot tell.

Cell bytes are the server's text representation (MySQL text protocol); the
host treats invalid UTF-8 as binary (`Cell::Bytes`).

`error` kinds (same order as `db_core::ErrorKind`): 1 connect, 2 auth, 3 tls,
4 query, 5 timeout, 6 internal. A query that ran into its `timeout_ms` ends
with kind 5 after the KILL.

Order for one query:

```
started -> (columns -> batch* | affected)* -> done | error | cancelled
```

A `query` may hold a script of several statements separated by `;` (the MySQL
client command `DELIMITER xx` is understood) and a `CALL` may return several
result sets. Every result set sends its own `columns` followed by its
`batch`es; a statement without a result set sends `affected` instead. The
batches of one result set are always sent before the next `columns` /
`affected`. `done.rows` is the number of rows streamed over all result sets
(`affected` counts are not included). If a later statement fails, the results
before it were already sent and the query ends with `error`.

Implementation note (MySQL backend): the sidecar splits the script itself
(same rules as `db_core::sql::split_statements`) and runs the statements one
after the other on the query's connection, reading `ROW_COUNT()` for
statements without a result set. `multiStatements` is not used because
go-sql-driver skips statements without a result set when walking result sets
and reports their affected rows only as one list.

A `;` inside the BEGIN ... END body of a `CREATE PROCEDURE / FUNCTION /
TRIGGER / EVENT` does not end that statement, so a routine sent as one
statement (the app does that after splitting a `DELIMITER` block) stays whole.

Column origin (schema / real table / real column) is **not** carried:
go-sql-driver does not expose it, so the host reports `origin = None` for
every sidecar column.

An `error` or `cancelled` may come earlier (no `started` when no connection
could be acquired; no `columns` when the statement failed). Exactly one
terminal frame (`done`, `error`, `cancelled`) is sent per `qid`. If the
cancel arrives after the last row was sent, the query ends with `done`.

If the `KILL QUERY` itself fails, the query ends with `error` (kind internal)
instead of `cancelled`, because the statement may still be running.

## Host behaviour (db-sidecar-client)

* Events reach the consumer in `db_core` order. Batches are decoded straight
  into columnar `db_core::Batch` in the reader task.
* The host keeps the session id of every running query (`running_sessions`).
* Pipe closed without a shutdown (sidecar crash): the running queries finish
  with `Failed(Internal)`, a fresh sidecar is started and sent `kill_session`
  for each saved id (up to 3 attempts). Without it MariaDB notices the dead
  socket only when the statement ends (about 4.9 s for `SLEEP`).
* Dropping a `QueryHandle` before `Finished` sends `cancel`.

## Oracle backend

* Connects with go-ora (pure Go, no Oracle client). Host / port / service
  name (`SIDECAR_DATABASE`).
* `SIDECAR_TLS` (TCPS = Oracle's TLS-wrapped listener): `require` = TCPS with
  certificate and host-name verification; `require-unverified` = TCPS without
  checking the certificate; `disabled` = plain TCP. A listener that offers only
  plain TCP (usual on port 1521) connects with `disabled` only.
* `started.session_id` = the session `SID` (`SYS_CONTEXT('USERENV','SID')`, no V$
  privileges needed). The serial number is not carried, because it is only
  needed for `ALTER SYSTEM KILL SESSION`, which is not used.
* A script is split on `;` (strings, quoted identifiers, comments, q-quotes
  respected; a trailing `;` is removed because Oracle rejects it). A script
  starting with `BEGIN`, `DECLARE` or `CREATE` is one statement and keeps its
  `END;`. `SELECT` / `WITH` stream rows, everything else is executed and
  reports `affected`.
* Text conversion: NUMBER = exact decimal text (no float rounding); DATE and
  TIMESTAMP = ISO 8601 without zone (`2026-11-15T08:30:01.123456`); TIMESTAMP
  WITH (LOCAL) TIME ZONE = RFC 3339 with offset; CLOB / NCLOB = text;
  BLOB / RAW = bytes; NULL = NULL. (Oracle has no empty string: `''` is NULL.)
* Identifiers are quoted with double quotes. The **row locator for edits is
  `ROWID`** (`SELECT ROWID, ...`, `UPDATE t SET c = ? WHERE ROWID = ?`); column
  origin is not reported, so the host uses `db_core::sql::single_table`.

## Catalog (host side, no frame)

`SidecarDriver::catalog()` runs data-dictionary SQL through `run` (schema and
table names as bound parameters), chosen by the driver name of the `ready`
frame: MySQL `information_schema` (same statements as `db-mysql`); Oracle
`ALL_USERS`, `ALL_OBJECTS` (TABLE, VIEW), `ALL_TAB_COLUMNS`, `ALL_CONSTRAINTS` +
`ALL_CONS_COLUMNS` (primary and foreign keys). Oracle reads are filtered by
owner (the user list is the only unfiltered one) and wrapped in
`ROWNUM <= cap` (default 20 000, `SidecarDriver::set_catalog_row_cap`).

The extended catalog (object kinds, indexes / triggers of a table, routine
parameters, DDL, users / roles, server info) uses the same mechanism: the SQL
lives in `db_core::dictionary` (shared with `db-mysql`), see
`crates/db-core/CATALOG-API.md`. Oracle DDL is `SELECT DBMS_METADATA.GET_DDL(...)
FROM DUAL`, a read-only SELECT; every Oracle list is filtered by `OWNER` and
capped with `ROWNUM`.
