# db-postgres

In-process PostgreSQL driver (`tokio-postgres` + `postgres-native-tls`). Same
shape as `db-mysql`; the app can treat both through `db_core::Driver`.

```rust
use db_postgres::{PgDriver, PgSession};

// Connects and checks the credentials. All I/O runs on `rt`; the future can be awaited anywhere.
PgDriver::connect(rt: &tokio::runtime::Handle, opts: db_core::ConnectOptions)
    -> impl Future<Output = Result<PgDriver, DbError>> + Send + 'static

impl db_core::Driver for PgDriver      // kind() == "postgres"
    // query(sql, opts)                 streaming, several statements per call
    // execute(sql, params, opts)       bound `?` placeholders
    // kill_session(pid)                SELECT pg_cancel_backend(pid) on a control connection
    // quote_identifier(name)           "name" with " doubled
    // catalog()                        Some(..): base + extended catalog (see below)

PgDriver::session(&self) -> impl Future<Output = Result<PgSession, DbError>> + Send + 'static
impl db_core::Driver for PgSession      // every statement on one connection
PgSession::session_id(&self) -> u64     // backend pid
// Dropping a PgSession cancels a running statement, closes the connection => open transaction rolled back, temp tables gone.
```

## ConnectOptions

* `host`, `port` (5432), `user`, `password`.
* `database`: database name; `None` (or empty) = the server's default (a database named like the user).
* `tls`: `Require` = TLS, certificate **and** host name verified, no fallback to plain text; `RequireUnverified` = TLS without checking the certificate (self-signed); `Disabled` = plain text. A server without TLS fails `Require*` with `ErrorKind::Tls`/`Connect`.
* Errors: bad password / unknown role = `ErrorKind::Auth` (SQLSTATE class 28); the message of a server error starts with its SQLSTATE, e.g. `[42P01] relation "x" does not exist` (`DbError::code` stays `None`).

## Behaviour the UI should know

* **Values are server text** (simple-query protocol, streamed): booleans are `t` / `f`, timestamps in the server's DateStyle, arrays as `{a,b}`. `bytea` arrives as `Cell::Bytes` (a value that happens to be valid UTF-8 shows as `Cell::Text`, like MySQL BLOB). NULL is `Cell::Null`.
* **Scripts**: statements are split with PostgreSQL rules (`$$ ... $$`, `E'..'`, nested comments; `#` is not a comment). One `Columns` + `Batch`* or one `Affected` per statement; a failing statement keeps the results before it.
* **Cancel / timeout / dropped handle**: a PostgreSQL cancel request on its own socket, repeated every 150 ms until the statement is gone. The connection stays usable (measured p50 17 ms for `pg_sleep(30)`). `Started.session_id` = backend pid. After a cancel inside a transaction the server marks it aborted: send `ROLLBACK`.
* **Back-pressure**: the sink is bounded (`DEFAULT_WINDOW`); a consumer that stops reading blocks the server in `ClientWrite`.
* **execute**: `?` placeholders (outside strings, quoted identifiers, `$$` quotes and comments) become `$1, $2, ...`; values are bound, never spliced. `Param::Text` is sent in text format whatever the column type, `Param::Bytes` is binary for `bytea` (raw text otherwise). A statement that returns rows with parameters (`... RETURNING`, `SELECT ... WHERE x = ?`) is wrapped as `WITH q(c0..) AS (<stmt>) SELECT c0::text.. FROM q`, so values are text there too.
* **`ColumnMeta.origin`**: from RowDescription table oid + attnum through a cached pg_class / pg_attribute lookup; `None` for expressions. `nullable` is set for origin columns (`!attnotnull`).
* **Pool**: up to 8 connections. A plain `query` that opens a transaction (`BEGIN`) without finishing it leaves the connection in that state: use `session()` for transactions.

## Catalog (`driver.catalog()`)

Base: `schemas()` (hides `pg_toast*` / `pg_temp*`), `objects(schema)` (tables incl. partitioned / foreign, views, materialized views), `columns` (`format_type`, nullability, primary key), `foreign_keys(schema)`, `current_schema()`.
Extended (`crates/db-core/CATALOG-API.md`, SQL in `db_core::dictionary`, `Dialect::Postgres`):

* kinds: Table, View, MaterializedView, Index, Sequence, Function, Procedure, Trigger, Type, Extension (two new `DbObjectKind` variants: `Type`, `Extension`).
* `list_objects` / `children` (indexes: `UNIQUE (a, b)`; triggers: `BEFORE INSERT OR UPDATE`) / `routine_params` (position 0 = return value).
* function / procedure names are `name(identity args)` so overloads are distinct; pass that string back to `routine_params` and `ddl`.
* `ddl`: tables reconstructed (columns, defaults, constraints), views and materialized views `pg_get_viewdef`, functions `pg_get_functiondef`, indexes `pg_get_indexdef`, triggers `pg_get_triggerdef` (first table when several share the name), sequences reconstructed, enum types, extensions.
* `principals` (login roles = `user`, other roles = `role`), `server_info` (version, database size, encodings, `pg_settings`, tablespaces).

## Tests

`cargo test -p db-postgres`: unit tests always; `tests/live.rs` runs against the user's test server and is skipped with a message when the Windows credential `Locus/test/postgres` is missing. Host / port / user / database come from `PG_TEST_HOST` (127.0.0.1), `PG_TEST_PORT` (5434), `PG_TEST_USER` (blog), `PG_TEST_DB` (tries `blog`, then `postgres`), `PG_TEST_TLS`. Read-only except `CREATE TEMP TABLE` inside a pinned session.
