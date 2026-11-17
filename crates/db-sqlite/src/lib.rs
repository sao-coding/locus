//! In-process SQLite driver (`rusqlite` with the bundled library, so no
//! system SQLite is needed).
//!
//! SQLite is blocking, so every query runs on its own thread and writes into
//! the bounded `db_core` channel with `blocking_send`: a consumer that falls
//! behind stops the thread (back-pressure). Cancel, a dropped handle and a
//! timeout all end the statement with `sqlite3_interrupt` (rusqlite's
//! `InterruptHandle`) plus a flag the row loop checks.
//!
//! A connection is a file path. Each query opens its own connection (cheap for
//! a file) except on a [`SqliteSession`], which pins one connection for a
//! transaction. Read-only mode is `SQLITE_OPEN_READ_ONLY` when the connection
//! was opened that way, and `PRAGMA query_only` for the Production guard.

mod catalog;
mod structure;

use db_core::{
    BatchBuilder, ColumnMeta, ColumnOrigin, DEFAULT_WINDOW, DbError, Driver, ErrorKind, Outcome,
    Param, QueryControl, QueryEvent, QueryHandle, QueryOptions, channel,
};
use parking_lot::Mutex;
use rusqlite::fallible_iterator::FallibleIterator as _;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, InterruptHandle, OpenFlags};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;

pub struct SqliteDriver {
    cfg: Arc<Cfg>,
}

pub(crate) struct Cfg {
    pub(crate) path: PathBuf,
    /// Opened with `SQLITE_OPEN_READ_ONLY`: nothing can write, ever.
    pub(crate) file_read_only: bool,
    /// The Production guard (`PRAGMA query_only`), switchable at any time.
    guard: AtomicBool,
}

/// Creates an empty database file. Fails when the file already exists, so a
/// mistyped path never overwrites anything.
pub fn create_database(path: &Path) -> Result<(), DbError> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| DbError::new(ErrorKind::Connect, format!("{}: {e}", dir.display())))?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(drop)
        .map_err(|e| DbError::new(ErrorKind::Connect, format!("{}: {e}", path.display())))
}

/// `sqlite:///C:/dir/file.db`, `sqlite://C:/dir/file.db`, `sqlite:////tmp/x.db`
/// (absolute Unix path, SQLAlchemy style), `sqlite:file.db` to a path. `None`
/// when it is not a `sqlite:` URL or names no file. Percent escapes are
/// decoded; a `?query` is dropped.
pub fn path_from_url(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("sqlite") && !scheme.eq_ignore_ascii_case("sqlite3") {
        return None;
    }
    let rest = rest.split('?').next().unwrap_or("");
    let rest = match rest.strip_prefix("//") {
        // `sqlite://` + empty host: the next `/` only separates host and path.
        Some(r) => r.strip_prefix('/').unwrap_or(r),
        None => rest,
    };
    let path = pct_decode(rest);
    (!path.is_empty()).then_some(path)
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl SqliteDriver {
    /// Opens the file once to check it is a database, then keeps only the
    /// path. The returned future can be awaited from any executor.
    pub fn connect(
        path: impl Into<PathBuf>,
        read_only: bool,
    ) -> impl Future<Output = Result<SqliteDriver, DbError>> + Send + 'static {
        let cfg = Arc::new(Cfg {
            path: path.into(),
            file_read_only: read_only,
            guard: AtomicBool::new(false),
        });
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let r = open(&cfg).and_then(|c| {
                // Reads the header: an existing file that is not a database fails here.
                c.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
                    .map_err(|e| connect_error(&e))
            });
            let _ = tx.send(r.map(|()| SqliteDriver { cfg }));
        });
        async move {
            rx.await
                .unwrap_or_else(|_| Err(DbError::new(ErrorKind::Internal, "connect thread failed")))
        }
    }

    /// Production guard: every query runs with `PRAGMA query_only`.
    pub fn set_read_only(&self, on: bool) {
        self.cfg.guard.store(on, Ordering::SeqCst);
    }

    /// One pinned connection, for a transaction.
    pub async fn session(&self) -> Result<SqliteSession, DbError> {
        let cfg = self.cfg.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(open(&cfg).map(|c| SqliteSession {
                cfg,
                conn: Arc::new(Mutex::new(c)),
            }));
        });
        rx.await
            .unwrap_or_else(|_| Err(DbError::new(ErrorKind::Internal, "session thread failed")))
    }
}

/// A [`Driver`] pinned to one connection. Dropping it closes the connection,
/// which rolls back an unfinished transaction.
pub struct SqliteSession {
    cfg: Arc<Cfg>,
    conn: Arc<Mutex<Connection>>,
}

pub(crate) fn open(cfg: &Cfg) -> Result<Connection, DbError> {
    if !cfg.path.is_file() {
        return Err(DbError::new(
            ErrorKind::Connect,
            format!("file not found: {}", cfg.path.display()),
        ));
    }
    let mode = if cfg.file_read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let conn = Connection::open_with_flags(&cfg.path, mode | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| connect_error(&e))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| map_error(&e))?;
    // Off by default in SQLite; a client that shows foreign keys should enforce them.
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(|e| map_error(&e))?;
    Ok(conn)
}

fn connect_error(e: &rusqlite::Error) -> DbError {
    let mut err = map_error(e);
    err.kind = ErrorKind::Connect;
    err
}

pub(crate) fn map_error(e: &rusqlite::Error) -> DbError {
    match e {
        rusqlite::Error::SqliteFailure(f, msg) => {
            let text = msg.clone().unwrap_or_else(|| f.to_string());
            DbError::new(ErrorKind::Query, text).with_code(i64::from(f.extended_code))
        }
        other => DbError::new(ErrorKind::Query, other.to_string()),
    }
}

impl Driver for SqliteDriver {
    fn kind(&self) -> &'static str {
        "sqlite"
    }

    fn query(&self, sql: String, opts: QueryOptions) -> QueryHandle {
        spawn_query(self.cfg.clone(), Target::Own, sql, Vec::new(), opts)
    }

    fn execute(&self, sql: String, params: Vec<Param>, opts: QueryOptions) -> QueryHandle {
        spawn_query(self.cfg.clone(), Target::Own, sql, to_values(params), opts)
    }

    fn kill_session(&self, _session_id: u64) -> QueryHandle {
        db_core::finished(Outcome::Done { rows: 0 })
    }

    fn catalog(&self) -> Option<&dyn db_core::Catalog> {
        Some(self)
    }
}

impl Driver for SqliteSession {
    fn kind(&self) -> &'static str {
        "sqlite"
    }

    fn query(&self, sql: String, opts: QueryOptions) -> QueryHandle {
        spawn_query(
            self.cfg.clone(),
            Target::Pinned(self.conn.clone()),
            sql,
            Vec::new(),
            opts,
        )
    }

    fn execute(&self, sql: String, params: Vec<Param>, opts: QueryOptions) -> QueryHandle {
        spawn_query(
            self.cfg.clone(),
            Target::Pinned(self.conn.clone()),
            sql,
            to_values(params),
            opts,
        )
    }

    fn kill_session(&self, _session_id: u64) -> QueryHandle {
        db_core::finished(Outcome::Done { rows: 0 })
    }
}

fn to_values(params: Vec<Param>) -> Vec<Value> {
    params
        .into_iter()
        .map(|p| match p {
            Param::Null => Value::Null,
            Param::Text(s) => Value::Text(s),
            Param::Bytes(b) => Value::Blob(b),
        })
        .collect()
}

enum Target {
    Own,
    Pinned(Arc<Mutex<Connection>>),
}

const RUNNING: u8 = 0;
const CANCELLED: u8 = 1;
const TIMED_OUT: u8 = 2;

/// Cancel switch of one query.
struct Ctl {
    reason: AtomicU8,
    handle: Mutex<Option<InterruptHandle>>,
}

impl Ctl {
    fn stop_with(&self, reason: u8) {
        let _ = self
            .reason
            .compare_exchange(RUNNING, reason, Ordering::SeqCst, Ordering::SeqCst);
        if let Some(h) = self.handle.lock().as_ref() {
            h.interrupt();
        }
    }

    fn stopped(&self) -> bool {
        self.reason.load(Ordering::SeqCst) != RUNNING
    }

    /// Called once the connection exists; a cancel that came earlier is applied now.
    fn attach(&self, h: InterruptHandle) {
        let mut slot = self.handle.lock();
        if self.stopped() {
            h.interrupt();
        }
        *slot = Some(h);
    }

    fn outcome(&self) -> Outcome {
        if self.reason.load(Ordering::SeqCst) == TIMED_OUT {
            Outcome::Failed(DbError::new(
                ErrorKind::Timeout,
                "the query hit its timeout and was stopped",
            ))
        } else {
            Outcome::Cancelled
        }
    }
}

impl QueryControl for Ctl {
    fn cancel(&self) {
        self.stop_with(CANCELLED);
    }
}

fn spawn_query(
    cfg: Arc<Cfg>,
    target: Target,
    sql: String,
    params: Vec<Value>,
    opts: QueryOptions,
) -> QueryHandle {
    let ctl = Arc::new(Ctl {
        reason: AtomicU8::new(RUNNING),
        handle: Mutex::new(None),
    });
    let (sink, handle) = channel(DEFAULT_WINDOW, ctl.clone());
    std::thread::spawn(move || {
        // The timeout watcher ends when `done` is dropped (query over).
        let (done, wait) = std::sync::mpsc::channel::<()>();
        if let Some(t) = opts.timeout {
            let ctl = ctl.clone();
            std::thread::spawn(move || {
                if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = wait.recv_timeout(t) {
                    ctl.stop_with(TIMED_OUT);
                }
            });
        }
        let outcome = match &target {
            Target::Own => match open(&cfg) {
                Ok(conn) => run(&cfg, &conn, &sql, &params, &opts, &sink, &ctl),
                Err(e) => Outcome::Failed(e),
            },
            Target::Pinned(m) => {
                let conn = m.lock();
                run(&cfg, &conn, &sql, &params, &opts, &sink, &ctl)
            }
        };
        drop(done);
        let _ = sink.blocking_send(QueryEvent::Finished(outcome));
    });
    handle
}

enum Fail {
    Db(rusqlite::Error),
    /// The consumer dropped its handle.
    Closed,
}

impl From<rusqlite::Error> for Fail {
    fn from(e: rusqlite::Error) -> Self {
        Fail::Db(e)
    }
}

fn run(
    cfg: &Cfg,
    conn: &Connection,
    sql: &str,
    params: &[Value],
    opts: &QueryOptions,
    sink: &db_core::EventSink,
    ctl: &Ctl,
) -> Outcome {
    ctl.attach(conn.get_interrupt_handle());
    if ctl.stopped() {
        return ctl.outcome();
    }
    // Re-applied on every query, so a pinned connection follows the guard too.
    if let Err(e) = conn.pragma_update(
        None,
        "query_only",
        cfg.guard.load(Ordering::SeqCst) || cfg.file_read_only,
    ) {
        return Outcome::Failed(map_error(&e));
    }
    if sink
        .blocking_send(QueryEvent::Started { session_id: 0 })
        .is_err()
    {
        return Outcome::Cancelled;
    }
    let sql = transaction_alias(sql);
    match run_statements(conn, &sql, params, opts, sink, ctl) {
        Ok(rows) if !ctl.stopped() => Outcome::Done { rows },
        Ok(_) | Err(Fail::Closed) => ctl.outcome(),
        Err(Fail::Db(_)) if ctl.stopped() => ctl.outcome(),
        Err(Fail::Db(e)) => Outcome::Failed(map_error(&e)),
    }
}

/// `START TRANSACTION` (what the Apply path sends) is `BEGIN` in SQLite.
fn transaction_alias(sql: &str) -> std::borrow::Cow<'_, str> {
    let t = sql.trim().trim_end_matches(';').trim();
    if t.eq_ignore_ascii_case("start transaction") {
        "BEGIN".into()
    } else {
        sql.into()
    }
}

fn run_statements(
    conn: &Connection,
    sql: &str,
    params: &[Value],
    opts: &QueryOptions,
    sink: &db_core::EventSink,
    ctl: &Ctl,
) -> Result<u64, Fail> {
    let mut total = 0u64;
    let mut batch = rusqlite::Batch::new(conn, sql);
    while let Some(mut stmt) = batch.next()? {
        if ctl.stopped() {
            break;
        }
        let ncols = stmt.column_count();
        if ncols == 0 {
            let rows = stmt.execute(rusqlite::params_from_iter(params.iter()))?;
            sink.blocking_send(QueryEvent::Affected { rows: rows as u64 })
                .map_err(|_| Fail::Closed)?;
            continue;
        }
        let metas: Vec<ColumnMeta> = stmt
            .columns()
            .zip(stmt.columns_with_metadata())
            .map(|(c, m)| ColumnMeta {
                name: c.name().to_string(),
                type_name: c.decl_type().unwrap_or("").to_string(),
                nullable: None,
                origin: match (m.table_name(), m.origin_name()) {
                    (Some(t), Some(col)) => Some(ColumnOrigin {
                        schema: m.database_name().unwrap_or("main").to_string(),
                        table: t.to_string(),
                        column: col.to_string(),
                    }),
                    _ => None,
                },
            })
            .collect();
        sink.blocking_send(QueryEvent::Columns(metas))
            .map_err(|_| Fail::Closed)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
        let mut builder = BatchBuilder::new(ncols);
        let mut text = String::new();
        while let Some(row) = rows.next()? {
            if ctl.stopped() {
                return Ok(total);
            }
            for i in 0..ncols {
                match row.get_ref(i)? {
                    ValueRef::Null => builder.push(None),
                    ValueRef::Integer(v) => {
                        use std::fmt::Write;
                        text.clear();
                        let _ = write!(text, "{v}");
                        builder.push(Some(text.as_bytes()))
                    }
                    ValueRef::Real(f) => builder.push(Some(real_text(f).as_bytes())),
                    ValueRef::Text(b) | ValueRef::Blob(b) => builder.push(Some(b)),
                }
            }
            builder.end_row();
            total += 1;
            if builder.rows() >= opts.batch_rows.max(1) {
                sink.blocking_send(QueryEvent::Batch(builder.finish()))
                    .map_err(|_| Fail::Closed)?;
            }
        }
        if builder.rows() > 0 {
            sink.blocking_send(QueryEvent::Batch(builder.finish()))
                .map_err(|_| Fail::Closed)?;
        }
    }
    Ok(total)
}

/// A REAL the way SQLite's shell prints it: `1.0`, not `1`.
fn real_text(f: f64) -> String {
    if f.is_infinite() {
        return if f > 0.0 { "Inf".into() } else { "-Inf".into() };
    }
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.1}")
    } else {
        f.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(
            path_from_url("sqlite:///C:/data/my%20db.sqlite").as_deref(),
            Some("C:/data/my db.sqlite")
        );
        assert_eq!(
            path_from_url("sqlite://C:/x.db").as_deref(),
            Some("C:/x.db")
        );
        assert_eq!(
            path_from_url("sqlite:////var/lib/x.db?mode=ro").as_deref(),
            Some("/var/lib/x.db")
        );
        assert_eq!(
            path_from_url("SQLite:rel/x.db").as_deref(),
            Some("rel/x.db")
        );
        assert_eq!(path_from_url("sqlite://"), None);
        assert_eq!(path_from_url("mysql://x"), None);
    }

    #[test]
    fn reals() {
        assert_eq!(real_text(1.0), "1.0");
        assert_eq!(real_text(0.5), "0.5");
        assert_eq!(real_text(f64::INFINITY), "Inf");
    }
}
