//! In-process PostgreSQL driver on `tokio-postgres`. See README.md.
//!
//! The public signature of [`PgDriver::connect`] mirrors `db_mysql::MysqlDriver`.
//!
//! How a query runs:
//! * The statement is split into statements (PostgreSQL rules, see `split`).
//!   Each one is `prepare`d (column names, types, table oid / attnum), then
//!   run with the **simple query protocol streamed**: every value arrives as
//!   server text and rows are pulled one by one, so a consumer that stops
//!   reading stops the socket reads (back-pressure through the bounded
//!   `db_core` channel) and memory stays flat.
//! * Cancel, a dropped handle and a timeout send a PostgreSQL cancel request
//!   (`CancelToken`, its own socket) every 150 ms until the statement is gone;
//!   the connection stays usable afterwards. `Started.session_id` is the
//!   backend pid.
//! * `execute` binds values (text format; `Bytes` as binary for `bytea`) and
//!   never splices them into the SQL. A row-returning statement with
//!   parameters runs as `WITH q(c0, ..) AS (<stmt>) SELECT c0::text, ... FROM q`
//!   so its values are text as well.

mod catalog;
mod split;

use db_core::{
    Batch, BatchBuilder, ColumnMeta, ColumnOrigin, ConnectOptions, DEFAULT_WINDOW, DbError, Driver,
    ErrorKind, Outcome, Param, QueryControl, QueryEvent, QueryHandle, QueryOptions, TlsMode,
    channel,
};
use futures_util::{Stream, StreamExt};
use parking_lot::Mutex;
use postgres_native_tls::MakeTlsConnector;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot, watch};
use tokio_postgres::types::{IsNull, ToSql, Type, to_sql_checked};
use tokio_postgres::{CancelToken, Client, Config, SimpleQueryMessage};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A cancel request is repeated at this interval until the statement is gone
/// (it covers the race where the cancel reaches the server before the
/// statement does).
const CANCEL_RETRY: Duration = Duration::from_millis(150);
const POOL_MAX: usize = 8;
/// Rows already read are sent after this long without a new row, so the end
/// of a result set never waits for the next statement of a script.
const PARTIAL_FLUSH: Duration = Duration::from_millis(50);

pub struct PgDriver {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) rt: Handle,
    cfg: Config,
    tls: MakeTlsConnector,
    idle: Mutex<Vec<PgConn>>,
    slots: Arc<Semaphore>,
    /// Second connection that only runs `pg_cancel_backend`.
    control: tokio::sync::Mutex<Option<PgConn>>,
    /// Table oid -> its columns, for `ColumnMeta::origin`.
    tables: Mutex<HashMap<u32, Arc<TableInfo>>>,
    /// Session read-only guard: 0 = not managed, 1 = read-only, 2 = read-write.
    guard: std::sync::atomic::AtomicU8,
}

struct TableInfo {
    schema: String,
    table: String,
    /// attnum -> (name, not null)
    columns: HashMap<i16, (String, bool)>,
}

pub(crate) struct PgConn {
    client: Client,
    pid: u32,
    cancel: CancelToken,
}

impl PgConn {
    fn usable(&self) -> bool {
        !self.client.is_closed()
    }
}

// ---- errors ----

fn err_text(e: &tokio_postgres::Error) -> String {
    match e.as_db_error() {
        Some(d) => d.message().to_string(),
        None => {
            // The source chain holds the useful part (I/O, TLS).
            let mut s = e.to_string();
            let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
            while let Some(c) = src {
                s.push_str(": ");
                s.push_str(&c.to_string());
                src = c.source();
            }
            s
        }
    }
}

fn map_error(e: &tokio_postgres::Error, connecting: bool) -> DbError {
    if let Some(d) = e.as_db_error() {
        let state = d.code().code();
        let kind = if connecting {
            if state.starts_with("28") {
                ErrorKind::Auth
            } else {
                ErrorKind::Connect
            }
        } else if state == "57014" && d.message().contains("statement timeout") {
            ErrorKind::Timeout
        } else {
            ErrorKind::Query
        };
        // The message carries the SQLSTATE, e.g. `[42P01] relation ... does not exist`.
        return DbError::new(kind, format!("[{state}] {}", d.message()));
    }
    let text = err_text(e);
    let lower = text.to_lowercase();
    let kind = if connecting
        && (lower.contains("tls")
            || lower.contains("certificate")
            || lower.contains("ssl")
            || lower.contains("schannel"))
    {
        ErrorKind::Tls
    } else if connecting {
        ErrorKind::Connect
    } else {
        ErrorKind::Internal
    };
    DbError::new(kind, text)
}

// ---- connect ----

fn make_tls(mode: TlsMode) -> Result<MakeTlsConnector, DbError> {
    let mut b = native_tls::TlsConnector::builder();
    if mode == TlsMode::RequireUnverified {
        b.danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true);
    }
    let c = b
        .build()
        .map_err(|e| DbError::new(ErrorKind::Tls, format!("cannot set up TLS: {e}")))?;
    Ok(MakeTlsConnector::new(c))
}

fn make_config(opts: &ConnectOptions) -> Config {
    let mut c = Config::new();
    c.host(&opts.host)
        .port(opts.port)
        .user(&opts.user)
        .password(&opts.password);
    // Without a database name PostgreSQL connects to a database named like the user.
    if let Some(db) = opts.database.as_deref().filter(|d| !d.is_empty()) {
        c.dbname(db);
    }
    c.connect_timeout(CONNECT_TIMEOUT).application_name("locus");
    c.ssl_mode(match opts.tls {
        TlsMode::Disabled => tokio_postgres::config::SslMode::Disable,
        // Require: no fallback to plain text.
        _ => tokio_postgres::config::SslMode::Require,
    });
    c
}

impl Inner {
    async fn connect_one(&self) -> Result<PgConn, DbError> {
        let (client, connection) = self
            .cfg
            .connect(self.tls.clone())
            .await
            .map_err(|e| map_error(&e, true))?;
        self.rt.spawn(async move {
            let _ = connection.await;
        });
        let cancel = client.cancel_token();
        let pid = backend_pid(&client)
            .await
            .map_err(|e| map_error(&e, true))?;
        Ok(PgConn {
            client,
            pid,
            cancel,
        })
    }

    /// Takes a connection and applies the read-only guard to it.
    async fn acquire(&self) -> Result<(PgConn, OwnedSemaphorePermit), DbError> {
        let (conn, permit) = self.acquire_raw().await?;
        let sql = match self.guard.load(std::sync::atomic::Ordering::SeqCst) {
            1 => "SET default_transaction_read_only = on",
            2 => "SET default_transaction_read_only = off",
            _ => return Ok((conn, permit)),
        };
        match conn.client.simple_query(sql).await {
            Ok(_) => Ok((conn, permit)),
            Err(e) => {
                let e = map_error(&e, false);
                Err(DbError::new(
                    e.kind,
                    format!("could not set the session's read-only mode: {}", e.message),
                ))
            }
        }
    }

    async fn acquire_raw(&self) -> Result<(PgConn, OwnedSemaphorePermit), DbError> {
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| DbError::new(ErrorKind::Internal, "the driver is closed"))?;
        loop {
            let idle = self.idle.lock().pop();
            match idle {
                Some(c) if c.usable() => return Ok((c, permit)),
                Some(_) => continue,
                None => return self.connect_one().await.map(|c| (c, permit)),
            }
        }
    }

    fn release(&self, conn: PgConn) {
        if conn.usable() {
            self.idle.lock().push(conn);
        }
    }

    /// Runs `sql` on the control connection, reconnecting once if it died.
    async fn control_query(&self, sql: &str) -> Result<(), DbError> {
        let mut guard = self.control.lock().await;
        for attempt in 0..2 {
            if guard.as_ref().is_none_or(|c| !c.usable()) {
                *guard = Some(self.connect_one().await?);
            }
            match guard.as_ref().unwrap().client.simple_query(sql).await {
                Ok(_) => return Ok(()),
                Err(e) if e.is_closed() && attempt == 0 => *guard = None,
                Err(e) => return Err(map_error(&e, false)),
            }
        }
        Err(DbError::new(
            ErrorKind::Connect,
            "the control connection is down",
        ))
    }
}

async fn backend_pid(client: &Client) -> Result<u32, tokio_postgres::Error> {
    let msgs = client.simple_query("SELECT pg_backend_pid()").await?;
    for m in msgs {
        if let SimpleQueryMessage::Row(r) = m {
            return Ok(r.get(0).and_then(|s| s.parse().ok()).unwrap_or(0));
        }
    }
    Ok(0)
}

impl PgDriver {
    /// Puts every session this driver hands out into read-only mode
    /// (`default_transaction_read_only`), or back. Applied each time a
    /// connection is taken, so new and reused connections are covered.
    pub fn set_read_only(&self, on: bool) {
        self.inner
            .guard
            .store(if on { 1 } else { 2 }, std::sync::atomic::Ordering::SeqCst);
    }

    /// Connects and checks the credentials. All I/O runs on `rt`; the returned
    /// future can be awaited from any executor.
    ///
    /// `opts.database` is the database name (`None` = a database named like
    /// the user). TLS: `Require` verifies certificate and host name,
    /// `RequireUnverified` encrypts without checking, `Disabled` is plain text.
    pub fn connect(
        rt: &Handle,
        opts: ConnectOptions,
    ) -> impl Future<Output = Result<PgDriver, DbError>> + Send + 'static {
        let rt2 = rt.clone();
        let task = rt.spawn(async move {
            let tls = make_tls(opts.tls)?;
            let inner = Arc::new(Inner {
                rt: rt2,
                cfg: make_config(&opts),
                tls,
                idle: Mutex::new(Vec::new()),
                slots: Arc::new(Semaphore::new(POOL_MAX)),
                control: tokio::sync::Mutex::new(None),
                tables: Mutex::new(HashMap::new()),
                guard: std::sync::atomic::AtomicU8::new(0),
            });
            let first = inner.connect_one().await?;
            inner.idle.lock().push(first);
            Ok(PgDriver { inner })
        });
        async move {
            match task.await {
                Ok(r) => r,
                Err(e) => Err(DbError::new(
                    ErrorKind::Internal,
                    format!("connect task failed: {e}"),
                )),
            }
        }
    }
}

// ---- cancel plumbing (same shape as db-mysql) ----

const NOT_STOPPED: u8 = 0;
const CANCELLED: u8 = 1;
const TIMED_OUT: u8 = 2;

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
    let _ = rx.wait_for(|s| *s).await;
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

/// Sends cancel requests until told to finish.
struct Canceller {
    done: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Canceller {
    fn start(token: CancelToken, tls: MakeTlsConnector) -> Self {
        let (done, mut done_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            loop {
                let _ = token.cancel_query(tls.clone()).await;
                tokio::select! {
                    _ = &mut done_rx => break,
                    _ = tokio::time::sleep(CANCEL_RETRY) => {}
                }
            }
        });
        Canceller { done, task }
    }

    /// No cancel for this query is sent after this returns, so a late one can
    /// never hit the next statement on the same connection.
    async fn finish(self) {
        let _ = self.done.send(());
        let _ = self.task.await;
    }
}

// ---- statements ----

enum Stmt {
    Text(String),
    Prepared(String, Vec<Param>),
}

enum Source {
    Pool,
    Pinned(Arc<tokio::sync::Mutex<Option<PgConn>>>),
}

enum Lease {
    Pooled(Option<PgConn>, #[allow(dead_code)] OwnedSemaphorePermit),
    Pinned(tokio::sync::OwnedMutexGuard<Option<PgConn>>),
}

impl Lease {
    fn conn(&self) -> Option<&PgConn> {
        match self {
            Lease::Pooled(c, _) => c.as_ref(),
            Lease::Pinned(g) => g.as_ref(),
        }
    }
}

enum StreamErr {
    Db(tokio_postgres::Error),
    Stopped,
}

impl From<tokio_postgres::Error> for StreamErr {
    fn from(e: tokio_postgres::Error) -> Self {
        StreamErr::Db(e)
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
            Ok(l) => l,
            Err(e) => return Outcome::Failed(e),
        },
        _ = stopped(&mut stop_rx) => return stop_outcome(ctl),
    };
    let Some(conn) = lease.conn() else {
        return Outcome::Failed(DbError::new(ErrorKind::Connect, "the session is closed"));
    };
    if sink
        .send(QueryEvent::Started {
            session_id: u64::from(conn.pid),
        })
        .await
        .is_err()
    {
        return Outcome::Cancelled;
    }
    let (result, cancelled) = drive(inner, conn, &stmt, &opts, sink, ctl, stop_rx).await;
    // Hand the connection back before telling the consumer.
    if let Lease::Pooled(c, _) = &mut lease
        && let Some(c) = c.take()
    {
        inner.release(c);
    }
    drop(lease);
    match result {
        Ok(rows) if !cancelled => Outcome::Done { rows },
        Ok(_) | Err(StreamErr::Stopped) => stop_outcome(ctl),
        Err(StreamErr::Db(_)) if cancelled => stop_outcome(ctl),
        Err(StreamErr::Db(e)) => Outcome::Failed(map_error(&e, false)),
    }
}

/// Runs the statements while watching for cancel / timeout. Returns the
/// result and whether cancel requests were sent.
async fn drive(
    inner: &Arc<Inner>,
    conn: &PgConn,
    stmt: &Stmt,
    opts: &QueryOptions,
    sink: &db_core::EventSink,
    ctl: &Arc<Ctl>,
    mut stop_rx: watch::Receiver<bool>,
) -> (Result<u64, StreamErr>, bool) {
    let token = conn.cancel.clone();

    let work_stop = stop_rx.clone();
    let work = stream(
        inner,
        conn,
        stmt,
        opts.batch_rows.max(1),
        sink,
        ctl,
        work_stop,
    );
    tokio::pin!(work);
    let timer = async {
        match opts.timeout {
            Some(limit) => tokio::time::sleep(limit).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(timer);
    let mut canceller: Option<Canceller> = None;
    let mut timer_done = false;
    let result = loop {
        tokio::select! {
            r = &mut work => break r,
            _ = stopped(&mut stop_rx), if canceller.is_none() => {
                canceller = Some(Canceller::start(token.clone(), inner.tls.clone()));
            }
            _ = &mut timer, if !timer_done && canceller.is_none() => {
                timer_done = true;
                ctl.stop_with(TIMED_OUT);
            }
        }
    };
    let cancelled = canceller.is_some();
    if let Some(c) = canceller {
        c.finish().await;
    }
    (result, cancelled)
}

async fn acquire(inner: &Arc<Inner>, source: Source) -> Result<Lease, DbError> {
    match source {
        Source::Pool => inner
            .acquire()
            .await
            .map(|(c, p)| Lease::Pooled(Some(c), p)),
        Source::Pinned(c) => Ok(Lease::Pinned(c.lock_owned().await)),
    }
}

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
                ctl.stop_with(CANCELLED);
                Err(())
            }
        },
        _ = stopped(stop) => Err(()),
    }
}

/// Reads every statement of the call in order. Returns the rows streamed.
async fn stream(
    inner: &Arc<Inner>,
    conn: &PgConn,
    stmt: &Stmt,
    batch_rows: usize,
    sink: &db_core::EventSink,
    ctl: &Ctl,
    mut stop: watch::Receiver<bool>,
) -> Result<u64, StreamErr> {
    if *stop.borrow() {
        return Err(StreamErr::Stopped);
    }
    let mut total = 0;
    match stmt {
        Stmt::Text(sql) => {
            for r in split::split(sql) {
                total += one_statement(inner, conn, &sql[r], &[], batch_rows, sink, ctl, &mut stop)
                    .await?;
                if *stop.borrow() {
                    return Err(StreamErr::Stopped);
                }
            }
        }
        Stmt::Prepared(sql, params) => {
            let sql = split::bind_placeholders(sql);
            total +=
                one_statement(inner, conn, &sql, params, batch_rows, sink, ctl, &mut stop).await?;
        }
    }
    Ok(total)
}

/// One row (or the end of a statement) from either protocol.
enum Item {
    Row(Cells),
    Complete(u64),
}

enum Cells {
    Simple(tokio_postgres::SimpleQueryRow),
    Text(tokio_postgres::Row),
}

impl Cells {
    fn get(&self, i: usize) -> Option<&str> {
        match self {
            Cells::Simple(r) => r.try_get(i).ok().flatten(),
            Cells::Text(r) => r.try_get::<_, Option<&str>>(i).ok().flatten(),
        }
    }
}

type Items = Pin<Box<dyn Stream<Item = Result<Item, tokio_postgres::Error>> + Send>>;

/// Values of the statement as text. `None` for a plain script statement.
fn simple_items(s: tokio_postgres::SimpleQueryStream) -> Items {
    Box::pin(s.filter_map(|m| async move {
        match m {
            Ok(SimpleQueryMessage::Row(r)) => Some(Ok(Item::Row(Cells::Simple(r)))),
            Ok(SimpleQueryMessage::CommandComplete(n)) => Some(Ok(Item::Complete(n))),
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        }
    }))
}

/// A bound parameter. Text format for everything (the server parses the text
/// as whatever type the placeholder has); `Bytes` is binary for `bytea`.
#[derive(Debug)]
struct Bind<'a>(&'a Param);

impl ToSql for Bind<'_> {
    fn to_sql(
        &self,
        _ty: &Type,
        out: &mut bytes::BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match self.0 {
            Param::Null => Ok(IsNull::Yes),
            Param::Text(s) => {
                out.extend_from_slice(s.as_bytes());
                Ok(IsNull::No)
            }
            Param::Bytes(b) => {
                out.extend_from_slice(b);
                Ok(IsNull::No)
            }
        }
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }

    fn encode_format(&self, ty: &Type) -> tokio_postgres::types::Format {
        if matches!(self.0, Param::Bytes(_)) && *ty == Type::BYTEA {
            tokio_postgres::types::Format::Binary
        } else {
            tokio_postgres::types::Format::Text
        }
    }

    to_sql_checked!();
}

#[allow(clippy::too_many_arguments)]
async fn one_statement(
    inner: &Arc<Inner>,
    conn: &PgConn,
    sql: &str,
    params: &[Param],
    batch_rows: usize,
    sink: &db_core::EventSink,
    ctl: &Ctl,
    stop: &mut watch::Receiver<bool>,
) -> Result<u64, StreamErr> {
    let prepared = conn.client.prepare(sql).await?;
    let columns = prepared.columns();
    let ncols = columns.len();
    let mut metas = Vec::new();
    let mut bytea = Vec::new();
    let items: Items;
    if ncols == 0 {
        if params.is_empty() {
            items = simple_items(conn.client.simple_query_raw(sql).await?);
        } else {
            let n = conn
                .client
                .execute_raw(&prepared, params.iter().map(Bind))
                .await?;
            return match send(sink, ctl, stop, QueryEvent::Affected { rows: n }).await {
                Ok(()) => Ok(0),
                Err(()) => Err(StreamErr::Stopped),
            };
        }
    } else {
        metas = column_metas(inner, conn, columns).await;
        bytea = columns.iter().map(|c| *c.type_() == Type::BYTEA).collect();
        if send(sink, ctl, stop, QueryEvent::Columns(metas.clone()))
            .await
            .is_err()
        {
            return Err(StreamErr::Stopped);
        }
        items = if params.is_empty() {
            simple_items(conn.client.simple_query_raw(sql).await?)
        } else {
            // Values as text: WITH q(c0, ...) AS (<stmt>) SELECT c0::text, ... FROM q
            let names: Vec<String> = (0..ncols).map(|i| format!("\"c{i}\"")).collect();
            let wrapped = format!(
                "WITH \"__q\"({}) AS (\n{sql}\n) SELECT {} FROM \"__q\"",
                names.join(", "),
                names
                    .iter()
                    .map(|n| format!("{n}::text"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let stmt = conn.client.prepare(&wrapped).await?;
            let rows = conn
                .client
                .query_raw(&stmt, params.iter().map(Bind))
                .await?;
            Box::pin(rows.map(|r| r.map(|row| Item::Row(Cells::Text(row)))))
        };
    }
    let _ = metas;
    pump(items, ncols, &bytea, batch_rows, sink, ctl, stop).await
}

async fn drain(mut items: Items) {
    while items.next().await.is_some() {}
}

async fn pump(
    mut items: Items,
    ncols: usize,
    bytea: &[bool],
    batch_rows: usize,
    sink: &db_core::EventSink,
    ctl: &Ctl,
    stop: &mut watch::Receiver<bool>,
) -> Result<u64, StreamErr> {
    let mut builder = BatchBuilder::new(ncols);
    let mut total = 0u64;
    loop {
        // Wait for the next message; a slow server flushes the partial batch.
        let msg = loop {
            if builder.rows() > 0 {
                tokio::select! {
                    m = items.next() => break m,
                    _ = tokio::time::sleep(PARTIAL_FLUSH) => {
                        let batch: Batch = builder.finish();
                        if send(sink, ctl, stop, QueryEvent::Batch(batch)).await.is_err() {
                            drain(items).await;
                            return Err(StreamErr::Stopped);
                        }
                    }
                }
            } else {
                break items.next().await;
            }
        };
        match msg {
            None => break,
            Some(Err(e)) => {
                if builder.rows() > 0 {
                    let _ = send(sink, ctl, stop, QueryEvent::Batch(builder.finish())).await;
                }
                return Err(e.into());
            }
            Some(Ok(Item::Row(cells))) => {
                for i in 0..ncols {
                    match cells.get(i) {
                        None => builder.push(None),
                        Some(t) if bytea.get(i).copied().unwrap_or(false) => match unhex(t) {
                            Some(b) => builder.push(Some(&b)),
                            None => builder.push(Some(t.as_bytes())),
                        },
                        Some(t) => builder.push(Some(t.as_bytes())),
                    }
                }
                builder.end_row();
                total += 1;
                if builder.rows() >= batch_rows
                    && send(sink, ctl, stop, QueryEvent::Batch(builder.finish()))
                        .await
                        .is_err()
                {
                    drain(items).await;
                    return Err(StreamErr::Stopped);
                }
            }
            Some(Ok(Item::Complete(n))) => {
                if builder.rows() > 0
                    && send(sink, ctl, stop, QueryEvent::Batch(builder.finish()))
                        .await
                        .is_err()
                {
                    drain(items).await;
                    return Err(StreamErr::Stopped);
                }
                if ncols == 0
                    && send(sink, ctl, stop, QueryEvent::Affected { rows: n })
                        .await
                        .is_err()
                {
                    drain(items).await;
                    return Err(StreamErr::Stopped);
                }
            }
        }
    }
    if builder.rows() > 0
        && send(sink, ctl, stop, QueryEvent::Batch(builder.finish()))
            .await
            .is_err()
    {
        return Err(StreamErr::Stopped);
    }
    Ok(total)
}

/// `\x48656c6c6f` -> bytes (bytea hex output).
fn unhex(t: &str) -> Option<Vec<u8>> {
    let h = t.strip_prefix("\\x")?;
    if h.len() % 2 != 0 {
        return None;
    }
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok())
        .collect()
}

// ---- column metadata and origin ----

async fn column_metas(
    inner: &Arc<Inner>,
    conn: &PgConn,
    cols: &[tokio_postgres::Column],
) -> Vec<ColumnMeta> {
    let missing: Vec<i64> = {
        let known = inner.tables.lock();
        let mut v: Vec<i64> = cols
            .iter()
            .filter_map(|c| c.table_oid().filter(|o| *o != 0))
            .filter(|o| !known.contains_key(o))
            .map(i64::from)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    if !missing.is_empty() {
        load_tables(inner, conn, &missing).await;
    }
    let known = inner.tables.lock();
    cols.iter()
        .map(|c| {
            let info = c.table_oid().and_then(|o| known.get(&o)).and_then(|t| {
                c.column_id()
                    .and_then(|a| t.columns.get(&a))
                    .map(|col| (t, col))
            });
            ColumnMeta {
                name: c.name().to_string(),
                type_name: c.type_().name().to_string(),
                nullable: info.map(|(_, (_, notnull))| !notnull),
                origin: info.map(|(t, (name, _))| ColumnOrigin {
                    schema: t.schema.clone(),
                    table: t.table.clone(),
                    column: name.clone(),
                }),
            }
        })
        .collect()
}

/// Fills the table cache from pg_class / pg_attribute; errors leave the
/// columns without an origin.
async fn load_tables(inner: &Arc<Inner>, conn: &PgConn, oids: &[i64]) {
    let rows = conn
        .client
        .query(
            "SELECT a.attrelid::int8, a.attnum::int4, n.nspname::text, c.relname::text, a.attname::text, a.attnotnull \
             FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE a.attrelid = ANY($1::int8[]::oid[]) AND a.attnum > 0 AND NOT a.attisdropped",
            &[&oids],
        )
        .await;
    let Ok(rows) = rows else { return };
    let mut found: HashMap<u32, TableInfo> = HashMap::new();
    for r in rows {
        let oid: i64 = r.get(0);
        let t = found.entry(oid as u32).or_insert_with(|| TableInfo {
            schema: r.get(2),
            table: r.get(3),
            columns: HashMap::new(),
        });
        t.columns
            .insert(r.get::<_, i32>(1) as i16, (r.get(4), r.get(5)));
    }
    let mut known = inner.tables.lock();
    for (oid, t) in found {
        known.insert(oid, Arc::new(t));
    }
}

// ---- Driver ----

fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

impl Driver for PgDriver {
    fn kind(&self) -> &'static str {
        "postgres"
    }

    fn query(&self, sql: String, opts: QueryOptions) -> QueryHandle {
        spawn_query(&self.inner, Source::Pool, Stmt::Text(sql), opts)
    }

    fn execute(&self, sql: String, params: Vec<Param>, opts: QueryOptions) -> QueryHandle {
        spawn_query(&self.inner, Source::Pool, Stmt::Prepared(sql, params), opts)
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

/// `pg_cancel_backend(pid)` on the control connection.
fn kill_session(inner: &Arc<Inner>, pid: u64) -> QueryHandle {
    struct Noop;
    impl QueryControl for Noop {
        fn cancel(&self) {}
    }
    let (sink, handle) = channel(2, Arc::new(Noop));
    let inner = inner.clone();
    inner.rt.clone().spawn(async move {
        let outcome = match inner
            .control_query(&format!("SELECT pg_cancel_backend({})", pid as u32))
            .await
        {
            Ok(()) => Outcome::Done { rows: 0 },
            Err(e) => Outcome::Failed(e),
        };
        let _ = sink.send(QueryEvent::Finished(outcome)).await;
    });
    handle
}

/// A [`Driver`] pinned to one server connection, for a transaction
/// (`query("BEGIN")`, statements, `COMMIT` / `ROLLBACK`), session settings or
/// temporary tables. Statements run one at a time in the order they were
/// started. Cancel, timeout and back-pressure work as on [`PgDriver`]; a
/// cancelled statement leaves the transaction open but marks it aborted (the
/// PostgreSQL rule: `ROLLBACK` it). Dropping the session cancels what runs and
/// closes the connection, so an unfinished transaction is rolled back and
/// temporary tables vanish.
pub struct PgSession {
    inner: Arc<Inner>,
    conn: Arc<tokio::sync::Mutex<Option<PgConn>>>,
    cancel: CancelToken,
    pid: u32,
    _slot: OwnedSemaphorePermit,
}

impl PgDriver {
    /// Opens a new connection and pins a [`PgSession`] to it.
    pub fn session(&self) -> impl Future<Output = Result<PgSession, DbError>> + Send + 'static {
        let inner = self.inner.clone();
        let task = self.inner.rt.spawn(async move {
            let (conn, slot) = inner.acquire().await?;
            let (cancel, pid) = (conn.cancel.clone(), conn.pid);
            Ok(PgSession {
                inner,
                conn: Arc::new(tokio::sync::Mutex::new(Some(conn))),
                cancel,
                pid,
                _slot: slot,
            })
        });
        async move {
            match task.await {
                Ok(r) => r,
                Err(e) => Err(DbError::new(
                    ErrorKind::Internal,
                    format!("session task failed: {e}"),
                )),
            }
        }
    }
}

impl PgSession {
    /// The backend pid of this session (what `Started` reports).
    pub fn session_id(&self) -> u64 {
        u64::from(self.pid)
    }
}

impl Drop for PgSession {
    fn drop(&mut self) {
        let (conn, cancel, inner) = (self.conn.clone(), self.cancel.clone(), self.inner.clone());
        self.inner.rt.spawn(async move {
            // A statement that is still running is cancelled first, then the
            // connection is closed (rollback on the server).
            let canceller = Canceller::start(cancel, inner.tls.clone());
            let taken = conn.lock().await.take();
            canceller.finish().await;
            drop(taken);
        });
    }
}

impl Driver for PgSession {
    fn kind(&self) -> &'static str {
        "postgres"
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
            Stmt::Prepared(sql, params),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_and_hex() {
        assert_eq!(quote_identifier("a\"b"), "\"a\"\"b\"");
        assert_eq!(unhex("\\x00ff10"), Some(vec![0, 255, 16]));
        assert_eq!(unhex("\\xabc"), None);
        assert_eq!(unhex("plain"), None);
    }
}
