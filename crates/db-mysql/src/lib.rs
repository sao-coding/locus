//! In-process MySQL / MariaDB driver.
//!
//! The public signature of [`MysqlDriver::connect`] is a contract with
//! `crates/app`; keep it.
//!
//! How a query runs:
//! * One pooled connection runs the statement and streams rows into a
//!   bounded `db_core` channel. When the consumer is behind, the task stops
//!   reading the socket, so memory stays flat (back-pressure).
//! * Cancel, a dropped handle and a timeout all do the same thing: send
//!   `KILL QUERY <id>` on a separate control connection (measured in the POC:
//!   dropping the future or closing the socket leaves the statement running
//!   for ~4.9 s). The query connection is then drained until the server's
//!   final packet, so `Finished` means the statement is really gone and the
//!   connection can be reused at once.

mod catalog;
mod errors;
mod types;

use db_core::{
    Batch, BatchBuilder, ConnectOptions, DEFAULT_WINDOW, DbError, Driver, ErrorKind, Outcome,
    Param, QueryControl, QueryEvent, QueryHandle, QueryOptions, TlsMode, channel,
};
use errors::{Phase, map_error};
use mysql_async::prelude::{Protocol, Queryable};
use mysql_async::{
    Conn, Opts, OptsBuilder, Params, Pool, PoolConstraints, PoolOpts, QueryResult, SslOpts, Value,
};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::{oneshot, watch};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// While a query is being stopped, `KILL QUERY` is repeated at this interval
/// until the query connection reports back. It covers the (microsecond) race
/// where the kill reaches the server before the statement does.
const KILL_RETRY: Duration = Duration::from_millis(150);
/// Idle connections the pool keeps / connections it may open.
const POOL_MIN: usize = 1;
const POOL_MAX: usize = 8;

pub struct MysqlDriver {
    inner: Arc<Inner>,
}

struct Inner {
    rt: Handle,
    pool: Pool,
    opts: Opts,
    /// Second connection that only sends KILL. Not part of the pool, so a
    /// cancel still works when every pooled connection is busy.
    control: tokio::sync::Mutex<Option<Conn>>,
    /// Session read-only guard: 0 = not managed, 1 = read-only, 2 = read-write.
    guard: AtomicU8,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Connections must be closed from inside the runtime; the driver may
        // be dropped on the UI thread.
        let pool = self.pool.clone();
        let control = self.control.get_mut().take();
        self.rt.spawn(async move {
            if let Some(conn) = control {
                let _ = conn.disconnect().await;
            }
            let _ = pool.disconnect().await;
        });
    }
}

impl Inner {
    /// Runs `sql` (a KILL statement) on the control connection, reconnecting
    /// once if that connection died.
    async fn kill(&self, sql: &str) -> Result<(), mysql_async::Error> {
        let mut guard = self.control.lock().await;
        let mut attempt = 0;
        loop {
            if guard.is_none() {
                *guard = Some(Conn::new(self.opts.clone()).await?);
            }
            let conn = guard.as_mut().expect("control connection was just opened");
            match conn.query_drop(sql).await {
                Ok(()) => return Ok(()),
                Err(e) if e.is_fatal() => {
                    *guard = None;
                    attempt += 1;
                    if attempt >= 2 {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl MysqlDriver {
    /// Connects and checks the credentials. All I/O runs on `rt`; the returned
    /// future can be awaited from any executor (GPUI's included).
    pub fn connect(
        rt: &Handle,
        opts: ConnectOptions,
    ) -> impl Future<Output = Result<MysqlDriver, DbError>> + Send + 'static {
        let task = rt.spawn(connect_inner(rt.clone(), opts));
        async move {
            match task.await {
                Ok(result) => result,
                Err(e) => Err(DbError::new(
                    ErrorKind::Internal,
                    format!("connect task failed: {e}"),
                )),
            }
        }
    }
}

async fn connect_inner(rt: Handle, opts: ConnectOptions) -> Result<MysqlDriver, DbError> {
    let ssl = match opts.tls {
        // Certificates are verified against the system roots and the host
        // name: "Require" must not be satisfiable by a man in the middle.
        TlsMode::Require => Some(SslOpts::default()),
        // Still refuses a server without TLS; only the certificate check is off.
        TlsMode::RequireUnverified => Some(
            SslOpts::default()
                .with_danger_accept_invalid_certs(true)
                .with_danger_skip_domain_validation(true),
        ),
        TlsMode::Disabled => None,
    };
    let pool_opts = PoolOpts::default()
        .with_constraints(PoolConstraints::new(POOL_MIN, POOL_MAX).expect("POOL_MIN <= POOL_MAX"))
        // A session is not shared between tabs: skip the extra
        // round trip COM_RESET_CONNECTION costs every time a connection returns.
        .with_reset_connection(false);
    let mysql_opts: Opts = OptsBuilder::default()
        .ip_or_hostname(opts.host)
        .tcp_port(opts.port)
        .user(Some(opts.user))
        .pass(Some(opts.password))
        .db_name(opts.database)
        .ssl_opts(ssl)
        .prefer_socket(false)
        // The server aborts a result that nobody reads for net_write_timeout
        // (60 s by default); a consumer that applies back-pressure must not
        // lose its query to that.
        .init(vec!["SET SESSION net_write_timeout = 3600"])
        .pool_opts(pool_opts)
        .into();

    let pool = Pool::new(mysql_opts.clone());
    // A real connection: this is what verifies host, credentials and TLS.
    // It goes back to the pool and serves the first query.
    let warm = match tokio::time::timeout(CONNECT_TIMEOUT, pool.get_conn()).await {
        Ok(Ok(conn)) => conn,
        Ok(Err(e)) => return Err(map_error(&e, Phase::Connect)),
        Err(_) => return Err(DbError::new(ErrorKind::Connect, "connecting timed out")),
    };
    let control = match tokio::time::timeout(CONNECT_TIMEOUT, Conn::new(mysql_opts.clone())).await {
        Ok(Ok(conn)) => conn,
        Ok(Err(e)) => return Err(map_error(&e, Phase::Connect)),
        Err(_) => return Err(DbError::new(ErrorKind::Connect, "connecting timed out")),
    };
    drop(warm);
    let inner = Inner {
        rt,
        pool,
        opts: mysql_opts,
        control: tokio::sync::Mutex::new(Some(control)),
        guard: AtomicU8::new(0),
    };
    Ok(MysqlDriver {
        inner: Arc::new(inner),
    })
}

impl MysqlDriver {
    /// Puts every session this driver hands out into read-only transaction
    /// mode (`true`) or back to read-write (`false`). The mode is set again
    /// each time a connection is taken (pool or pinned session), so new
    /// connections and ones changed by a `SET` statement are covered. A
    /// connection that cannot be made read-only is not used.
    pub fn set_read_only(&self, on: bool) {
        self.inner
            .guard
            .store(if on { 1 } else { 2 }, Ordering::SeqCst);
    }
}

/// Applies the driver's read-only guard to `conn`.
async fn apply_guard(inner: &Inner, conn: &mut Conn) -> Result<(), DbError> {
    match inner.guard.load(Ordering::SeqCst) {
        1 => conn
            .query_drop("SET SESSION TRANSACTION READ ONLY")
            .await
            .map_err(|e| {
                let e = map_error(&e, Phase::Query);
                DbError::new(
                    e.kind,
                    format!("could not make the session read-only: {}", e.message),
                )
            }),
        2 => {
            let _ = conn.query_drop("SET SESSION TRANSACTION READ WRITE").await;
            Ok(())
        }
        _ => Ok(()),
    }
}

impl Driver for MysqlDriver {
    fn kind(&self) -> &'static str {
        "mysql"
    }

    fn query(&self, sql: String, opts: QueryOptions) -> QueryHandle {
        spawn_query(&self.inner, Source::Pool, Stmt::Text(sql), opts)
    }

    fn execute(&self, sql: String, params: Vec<Param>, opts: QueryOptions) -> QueryHandle {
        spawn_query(
            &self.inner,
            Source::Pool,
            Stmt::Prepared(sql, to_values(params)),
            opts,
        )
    }

    fn kill_session(&self, session_id: u64) -> QueryHandle {
        kill_session(&self.inner, session_id)
    }

    fn quote_identifier(&self, name: &str) -> String {
        quote_identifier(name)
    }

    fn catalog(&self) -> Option<&dyn db_core::Catalog> {
        Some(self)
    }
}

fn quote_identifier(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

fn to_values(params: Vec<Param>) -> Vec<Value> {
    params
        .into_iter()
        .map(|p| match p {
            Param::Null => Value::NULL,
            Param::Text(s) => Value::Bytes(s.into_bytes()),
            Param::Bytes(b) => Value::Bytes(b),
        })
        .collect()
}

fn kill_session(inner: &Arc<Inner>, session_id: u64) -> QueryHandle {
    let (sink, handle) = channel(1, Arc::new(Ctl::new()));
    let inner = inner.clone();
    inner.rt.clone().spawn(async move {
        let outcome = match inner.kill(&format!("KILL {session_id}")).await {
            Ok(()) => Outcome::Done { rows: 0 },
            Err(e) => Outcome::Failed(map_error(&e, Phase::Query)),
        };
        let _ = sink.send(QueryEvent::Finished(outcome)).await;
    });
    handle
}

/// What a query runs.
enum Stmt {
    /// Text protocol. May hold several statements separated by `;`.
    Text(String),
    /// Prepared statement with bound parameters (binary protocol).
    Prepared(String, Vec<Value>),
}

/// Which connection a query runs on.
#[derive(Clone)]
enum Source {
    /// Any pooled connection, returned to the pool when the query ends.
    Pool,
    /// The one connection of a [`MysqlSession`].
    Pinned(Arc<tokio::sync::Mutex<Option<Conn>>>),
}

/// The connection a running query holds.
enum Lease {
    Pooled(Conn),
    Pinned(tokio::sync::OwnedMutexGuard<Option<Conn>>),
}

impl Lease {
    fn conn(&mut self) -> Option<&mut Conn> {
        match self {
            Lease::Pooled(c) => Some(c),
            Lease::Pinned(g) => g.as_mut(),
        }
    }
}

fn spawn_query(inner: &Arc<Inner>, source: Source, stmt: Stmt, opts: QueryOptions) -> QueryHandle {
    let ctl = Arc::new(Ctl::new());
    let (sink, handle) = channel(DEFAULT_WINDOW, ctl.clone());
    let inner = inner.clone();
    inner.rt.clone().spawn(async move {
        let outcome = run_query(&inner, source, stmt, opts, &sink, &ctl).await;
        let _ = sink.send(QueryEvent::Finished(outcome)).await;
    });
    handle
}

/// A [`Driver`] pinned to one server connection, for work that has to stay on
/// one connection: a transaction (`query("START TRANSACTION")`, statements,
/// `COMMIT` / `ROLLBACK`), session variables, temporary tables.
///
/// Queries on a session run one at a time, in the order they were started
/// (a second one waits for the first). Cancel, timeout and back-pressure work
/// as on [`MysqlDriver`]; a cancelled statement leaves the transaction open.
/// Dropping the session closes its connection, so an unfinished transaction
/// is rolled back by the server and no session state leaks into the pool.
pub struct MysqlSession {
    inner: Arc<Inner>,
    conn: Arc<tokio::sync::Mutex<Option<Conn>>>,
}

impl MysqlDriver {
    /// Takes one connection out of the pool and pins a [`MysqlSession`] to it.
    pub fn session(&self) -> impl Future<Output = Result<MysqlSession, DbError>> + Send + 'static {
        let inner = self.inner.clone();
        let task = self.inner.rt.spawn(async move {
            match tokio::time::timeout(CONNECT_TIMEOUT, inner.pool.get_conn()).await {
                Ok(Ok(mut conn)) => {
                    apply_guard(&inner, &mut conn).await?;
                    Ok(MysqlSession {
                        inner,
                        conn: Arc::new(tokio::sync::Mutex::new(Some(conn))),
                    })
                }
                Ok(Err(e)) => Err(map_error(&e, Phase::Connect)),
                Err(_) => Err(DbError::new(
                    ErrorKind::Connect,
                    "getting a connection timed out",
                )),
            }
        });
        async move {
            match task.await {
                Ok(result) => result,
                Err(e) => Err(DbError::new(
                    ErrorKind::Internal,
                    format!("session task failed: {e}"),
                )),
            }
        }
    }
}

impl MysqlSession {
    /// The server's connection id of this session (what `Started` reports).
    pub fn session_id(&self) -> Option<u64> {
        self.conn
            .try_lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| u64::from(c.id())))
    }
}

impl Drop for MysqlSession {
    fn drop(&mut self) {
        let conn = self.conn.clone();
        self.inner.rt.spawn(async move {
            // Waits for a statement that is still running.
            if let Some(conn) = conn.lock().await.take() {
                let _ = conn.disconnect().await;
            }
        });
    }
}

impl Driver for MysqlSession {
    fn kind(&self) -> &'static str {
        "mysql"
    }

    fn query(&self, sql: String, opts: QueryOptions) -> QueryHandle {
        spawn_query(
            &self.inner,
            Source::Pinned(self.conn.clone()),
            Stmt::Text(sql),
            opts,
        )
    }

    fn execute(&self, sql: String, params: Vec<Param>, opts: QueryOptions) -> QueryHandle {
        spawn_query(
            &self.inner,
            Source::Pinned(self.conn.clone()),
            Stmt::Prepared(sql, to_values(params)),
            opts,
        )
    }

    fn kill_session(&self, session_id: u64) -> QueryHandle {
        kill_session(&self.inner, session_id)
    }

    fn quote_identifier(&self, name: &str) -> String {
        quote_identifier(name)
    }
}

const NOT_STOPPED: u8 = 0;
const CANCELLED: u8 = 1;
const TIMED_OUT: u8 = 2;

/// The cancel switch of one query. `cancel` (also called when the consumer
/// drops its handle) and the timeout both flip `stop`; the first one to do so
/// decides how the query ends.
struct Ctl {
    stop: watch::Sender<bool>,
    reason: AtomicU8,
}

impl Ctl {
    fn new() -> Self {
        Self {
            stop: watch::Sender::new(false),
            reason: AtomicU8::new(NOT_STOPPED),
        }
    }

    fn stop_with(&self, reason: u8) {
        let _ =
            self.reason
                .compare_exchange(NOT_STOPPED, reason, Ordering::SeqCst, Ordering::SeqCst);
        self.stop.send_replace(true);
    }
}

impl QueryControl for Ctl {
    fn cancel(&self) {
        self.stop_with(CANCELLED);
    }
}

async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|stop| *stop).await;
}

/// Why `stream` returned early.
enum StreamErr {
    Db(mysql_async::Error),
    /// Cancel, timeout or dropped consumer; the query connection is drained.
    Stopped,
}

impl From<mysql_async::Error> for StreamErr {
    fn from(e: mysql_async::Error) -> Self {
        StreamErr::Db(e)
    }
}

async fn run_query(
    inner: &Arc<Inner>,
    source: Source,
    stmt: Stmt,
    opts: QueryOptions,
    sink: &db_core::EventSink,
    ctl: &Arc<Ctl>,
) -> Outcome {
    let mut stop_rx = ctl.stop.subscribe();
    let mut lease = tokio::select! {
        got = acquire(inner, source) => match got {
            Ok(lease) => lease,
            Err(e) => return Outcome::Failed(e),
        },
        _ = stopped(&mut stop_rx) => return stop_outcome(ctl),
    };
    let Some(conn) = lease.conn() else {
        return Outcome::Failed(DbError::new(ErrorKind::Connect, "the session is closed"));
    };
    let id = conn.id();
    if sink
        .send(QueryEvent::Started {
            session_id: u64::from(id),
        })
        .await
        .is_err()
    {
        return Outcome::Cancelled;
    }

    let (result, killed) = drive(inner, conn, id, &stmt, &opts, sink, ctl, stop_rx).await;
    // The connection is clean (or broken) here; hand it back before telling
    // the consumer, so the next query can reuse it.
    drop(lease);
    match result {
        Ok(rows) if !killed => Outcome::Done { rows },
        Ok(_) | Err(StreamErr::Stopped) => stop_outcome(ctl),
        Err(StreamErr::Db(_)) if killed => stop_outcome(ctl),
        Err(StreamErr::Db(e)) => Outcome::Failed(map_error(&e, Phase::Query)),
    }
}

async fn acquire(inner: &Arc<Inner>, source: Source) -> Result<Lease, DbError> {
    match source {
        Source::Pool => match inner.pool.get_conn().await {
            Ok(mut conn) => {
                apply_guard(inner, &mut conn).await?;
                Ok(Lease::Pooled(conn))
            }
            Err(e) => Err(map_error(&e, Phase::Connect)),
        },
        Source::Pinned(conn) => Ok(Lease::Pinned(conn.lock_owned().await)),
    }
}

fn stop_outcome(ctl: &Ctl) -> Outcome {
    if ctl.reason.load(Ordering::SeqCst) == TIMED_OUT {
        Outcome::Failed(DbError::new(
            ErrorKind::Timeout,
            "the query hit its timeout and was stopped on the server",
        ))
    } else {
        Outcome::Cancelled
    }
}

/// Runs the statement while watching for cancel / timeout. Returns the
/// result and whether a KILL was sent.
#[allow(clippy::too_many_arguments)]
async fn drive(
    inner: &Arc<Inner>,
    conn: &mut Conn,
    id: u32,
    stmt: &Stmt,
    opts: &QueryOptions,
    sink: &db_core::EventSink,
    ctl: &Arc<Ctl>,
    mut stop_rx: watch::Receiver<bool>,
) -> (Result<u64, StreamErr>, bool) {
    let work_stop = stop_rx.clone();
    let work = stream(conn, stmt, opts.batch_rows.max(1), sink, ctl, work_stop);
    tokio::pin!(work);
    let timer = async {
        match opts.timeout {
            Some(limit) => tokio::time::sleep(limit).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(timer);

    let mut killer: Option<Killer> = None;
    let mut timer_done = false;
    let result = loop {
        tokio::select! {
            r = &mut work => break r,
            _ = stopped(&mut stop_rx), if killer.is_none() => {
                killer = Some(Killer::start(inner, id));
            }
            _ = &mut timer, if !timer_done && killer.is_none() => {
                timer_done = true;
                ctl.stop_with(TIMED_OUT);
            }
        }
    };
    let killed = killer.is_some();
    if let Some(k) = killer {
        k.finish().await;
    }
    (result, killed)
}

/// Sends `KILL QUERY <id>` on the control connection, again every
/// [`KILL_RETRY`] until told to finish.
struct Killer {
    done: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Killer {
    fn start(inner: &Arc<Inner>, id: u32) -> Self {
        let (done, mut done_rx) = oneshot::channel::<()>();
        let inner = inner.clone();
        let task = tokio::spawn(async move {
            let sql = format!("KILL QUERY {id}");
            loop {
                // A failed KILL is retried: the next round reconnects.
                let _ = inner.kill(&sql).await;
                tokio::select! {
                    _ = &mut done_rx => break,
                    _ = tokio::time::sleep(KILL_RETRY) => {}
                }
            }
        });
        Killer { done, task }
    }

    /// No KILL for this query is sent after this returns, so a late one can
    /// never hit the next statement on the same connection.
    async fn finish(self) {
        let _ = self.done.send(());
        let _ = self.task.await;
    }
}

/// Sends an event, giving up when the query is being stopped.
async fn send(
    sink: &db_core::EventSink,
    ctl: &Ctl,
    stop: &mut watch::Receiver<bool>,
    event: QueryEvent,
) -> Result<(), ()> {
    if *stop.borrow() {
        return Err(());
    }
    tokio::select! {
        sent = sink.send(event) => match sent {
            Ok(()) => Ok(()),
            Err(_) => {
                // The consumer is gone: make sure the server stops too.
                ctl.stop_with(CANCELLED);
                Err(())
            }
        },
        _ = stopped(stop) => Err(()),
    }
}

async fn stream(
    conn: &mut Conn,
    stmt: &Stmt,
    batch_rows: usize,
    sink: &db_core::EventSink,
    ctl: &Ctl,
    stop: watch::Receiver<bool>,
) -> Result<u64, StreamErr> {
    if *stop.borrow() {
        return Err(StreamErr::Stopped);
    }
    match stmt {
        Stmt::Text(sql) => {
            pump(
                conn.query_iter(sql.as_str()).await?,
                &call_flags(sql),
                batch_rows,
                sink,
                ctl,
                stop,
            )
            .await
        }
        Stmt::Prepared(sql, params) => {
            let result = conn
                .exec_iter(sql.as_str(), Params::Positional(params.clone()))
                .await?;
            pump(result, &call_flags(sql), batch_rows, sink, ctl, stop).await
        }
    }
}

/// Reads every result of the call, in order: `Columns` + batches for a result
/// set, `Affected` for a statement without one. Returns the rows streamed in
/// total. When a later statement fails, what was read before it has been
/// delivered already and the error is returned.
async fn pump<P: Protocol>(
    mut result: QueryResult<'_, '_, P>,
    calls: &[bool],
    batch_rows: usize,
    sink: &db_core::EventSink,
    ctl: &Ctl,
    mut stop: watch::Receiver<bool>,
) -> Result<u64, StreamErr> {
    let mut total = 0u64;
    // A CALL that returned result sets ends with an OK packet of its own; it
    // is not a statement and is not reported. `idx` is the statement the
    // current result belongs to.
    let mut idx = 0usize;
    let mut call_had_rows = false;
    while let Some(columns) = result.columns() {
        let ncols = columns.len();
        let is_call = calls.get(idx).copied().unwrap_or(false);
        if ncols == 0 {
            let affected = result.affected_rows();
            let report = !(is_call && call_had_rows);
            idx += 1;
            call_had_rows = false;
            // Moves on to the next result (or the end).
            if let Err(e) = result.next().await {
                return Err(e.into());
            }
            if report
                && send(
                    sink,
                    ctl,
                    &mut stop,
                    QueryEvent::Affected { rows: affected },
                )
                .await
                .is_err()
            {
                let _ = result.drop_result().await;
                return Err(StreamErr::Stopped);
            }
            continue;
        }
        let metas: Vec<_> = columns.iter().map(types::column_meta).collect();
        if send(sink, ctl, &mut stop, QueryEvent::Columns(metas))
            .await
            .is_err()
        {
            let _ = result.drop_result().await;
            return Err(StreamErr::Stopped);
        }

        let mut builder = BatchBuilder::new(ncols);
        let outcome = loop {
            let next = next_row(&mut result, &mut builder, sink, ctl, &mut stop).await;
            let next = match next {
                Some(r) => r,
                None => {
                    let _ = result.drop_result().await;
                    return Err(StreamErr::Stopped);
                }
            };
            match next {
                Ok(Some(row)) => {
                    for (i, col) in columns.iter().enumerate() {
                        types::push_cell(&mut builder, row.as_ref(i), col.column_type());
                    }
                    builder.end_row();
                    total += 1;
                    if builder.rows() >= batch_rows {
                        let batch: Batch = builder.finish();
                        if send(sink, ctl, &mut stop, QueryEvent::Batch(batch))
                            .await
                            .is_err()
                        {
                            // Read the rest of the result so the connection is reusable.
                            // The KILL is on its way, so this ends quickly.
                            let _ = result.drop_result().await;
                            return Err(StreamErr::Stopped);
                        }
                    }
                }
                // End of this result set; the next one (if any) is now current.
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        // Rows read before the end of the set (or before the error) go out
        // first, so a failing next statement never swallows them.
        if builder.rows() > 0 {
            let batch = builder.finish();
            if send(sink, ctl, &mut stop, QueryEvent::Batch(batch))
                .await
                .is_err()
            {
                let _ = result.drop_result().await;
                return Err(StreamErr::Stopped);
            }
        }
        if is_call {
            call_had_rows = true;
        } else {
            idx += 1;
        }
        outcome?;
    }
    // An error in a later statement is kept by the connection (columns() cannot
    // see it) and raised here.
    result.next().await?;
    Ok(total)
}

/// For every statement of a script: is it a `CALL`? Only a CALL sends an OK
/// packet after its result sets that is not a statement of its own.
fn call_flags(sql: &str) -> Vec<bool> {
    db_core::sql::split_statements(sql)
        .into_iter()
        .map(|r| is_call(&sql[r]))
        .collect()
}

fn is_call(mut s: &str) -> bool {
    loop {
        s = s.trim_start();
        if let Some(rest) = s.strip_prefix("/*") {
            s = rest.split_once("*/").map_or("", |(_, tail)| tail);
        } else if s.starts_with('#') || s.starts_with("-- ") {
            s = s.split_once('\n').map_or("", |(_, tail)| tail);
        } else {
            break;
        }
    }
    s.get(..4).is_some_and(|w| w.eq_ignore_ascii_case("call"))
        && !s[4..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
}

/// How long the driver waits for the next row before it sends the rows it
/// already has. Without it the last rows of a result set would sit in the
/// builder while the server runs the next statement of a script (`SELECT 1;
/// SELECT SLEEP(30)` would show the first result after 30 s).
const PARTIAL_FLUSH: Duration = Duration::from_millis(50);

/// `result.next()`, but a wait for the server longer than [`PARTIAL_FLUSH`]
/// sends the partial batch first. `None` means the consumer is gone.
async fn next_row<P: Protocol>(
    result: &mut QueryResult<'_, '_, P>,
    builder: &mut BatchBuilder,
    sink: &db_core::EventSink,
    ctl: &Ctl,
    stop: &mut watch::Receiver<bool>,
) -> Option<mysql_async::Result<Option<mysql_async::Row>>> {
    use std::task::Poll;
    let mut fut = std::pin::pin!(result.next());
    // Rows that are already buffered by the socket do not start a timer.
    let first = std::future::poll_fn(|cx| match fut.as_mut().poll(cx) {
        Poll::Ready(v) => Poll::Ready(Some(v)),
        Poll::Pending => Poll::Ready(None),
    })
    .await;
    if let Some(r) = first {
        return Some(r);
    }
    if builder.rows() == 0 {
        return Some(fut.await);
    }
    tokio::select! {
        r = &mut fut => Some(r),
        _ = tokio::time::sleep(PARTIAL_FLUSH) => {
            let batch = builder.finish();
            send(sink, ctl, stop, QueryEvent::Batch(batch)).await.ok()?;
            Some(fut.await)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_identifier_doubles_backticks() {
        assert_eq!(quote_identifier("student"), "`student`");
        assert_eq!(quote_identifier("we`ird"), "`we``ird`");
        assert_eq!(quote_identifier("``"), "``````");
        assert_eq!(quote_identifier("学生 表"), "`学生 表`");
        assert_eq!(quote_identifier(""), "``");
    }

    #[test]
    fn call_detection() {
        assert_eq!(
            call_flags(
                "SELECT 1; call p(); /* x */ CALL q;\n-- c\nCallx(1); update call_t set a=1"
            ),
            [false, true, true, false, false]
        );
    }
}
