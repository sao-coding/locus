//! In-process Microsoft SQL Server driver on `tiberius` (TDS 7.3+).
//!
//! The public signature of [`MssqlDriver::connect`] mirrors `db_postgres::PgDriver`.
//!
//! How a query runs:
//! * The script is split into batches on `GO` lines (see `split`); each batch
//!   goes to the server as one SQL batch, so `DECLARE`d variables and
//!   multi-statement batches behave as in SSMS. Every result set of a batch
//!   is streamed; a batch without a result set reports `@@ROWCOUNT`.
//! * All values arrive as text (dates ISO 8601); binary columns as bytes.
//! * Cancel, a dropped handle and a timeout run `KILL <spid>` on a separate
//!   control connection. tiberius cannot send a TDS attention, so the
//!   connection is gone afterwards: a pooled one is replaced, a pinned session
//!   (transaction) is lost and rolled back by the server.
//! * `execute` turns the `?` placeholders into `@P1..` and binds the values
//!   (never spliced into the SQL).

mod catalog;
mod split;

use db_core::{
    BatchBuilder, ColumnMeta, ConnectOptions, DEFAULT_WINDOW, DbError, Driver, ErrorKind, Outcome,
    Param, QueryControl, QueryEvent, QueryHandle, QueryOptions, TlsMode, channel,
};
use futures_util::TryStreamExt;
use parking_lot::Mutex;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;
use tiberius::{AuthMethod, Client, ColumnData, Config, EncryptionLevel, FromSql, QueryItem};
use tokio::net::TcpStream;
use tokio::runtime::Handle;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POOL_MAX: usize = 8;
/// Rows already read are sent after this long without a new row.
const PARTIAL_FLUSH: Duration = Duration::from_millis(50);

type Tds = Client<Compat<TcpStream>>;

pub struct MssqlDriver {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) rt: Handle,
    cfg: Config,
    idle: Mutex<Vec<Conn>>,
    slots: Arc<Semaphore>,
    /// Second connection that only runs `KILL`.
    control: tokio::sync::Mutex<Option<Conn>>,
}

pub(crate) struct Conn {
    client: Tds,
    spid: u64,
    /// Set when the connection was killed or failed at the transport level.
    dead: Arc<AtomicBool>,
}

// ---- errors ----

fn map_error(e: &tiberius::error::Error, connecting: bool) -> DbError {
    use tiberius::error::Error as E;
    match e {
        E::Server(t) => {
            let code = t.code();
            // 18456 login failed, 18452 untrusted domain, 1848x locked / expired,
            // 4060 cannot open the database.
            let kind = if matches!(code, 18456 | 18452 | 18486 | 18487 | 18488 | 4060) {
                ErrorKind::Auth
            } else if connecting {
                ErrorKind::Connect
            } else {
                ErrorKind::Query
            };
            DbError::new(kind, format!("[{code}] {}", t.message()))
        }
        E::Tls(m) => DbError::new(ErrorKind::Tls, m.to_string()),
        _ => {
            let text = e.to_string();
            let lower = text.to_lowercase();
            let kind = if connecting && (lower.contains("tls") || lower.contains("certificate")) {
                ErrorKind::Tls
            } else if connecting {
                ErrorKind::Connect
            } else {
                ErrorKind::Internal
            };
            DbError::new(kind, text)
        }
    }
}

// ---- connect ----

fn make_config(opts: &ConnectOptions) -> Config {
    let mut c = Config::new();
    c.host(&opts.host);
    c.port(opts.port);
    c.authentication(AuthMethod::sql_server(&opts.user, &opts.password));
    if let Some(db) = opts.database.as_deref().filter(|d| !d.is_empty()) {
        c.database(db);
    }
    c.application_name("locus");
    match opts.tls {
        TlsMode::Require => c.encryption(EncryptionLevel::Required),
        TlsMode::RequireUnverified => {
            c.encryption(EncryptionLevel::Required);
            c.trust_cert();
        }
        TlsMode::Disabled => c.encryption(EncryptionLevel::NotSupported),
    }
    c
}

impl Inner {
    async fn connect_one(&self) -> Result<Conn, DbError> {
        let fut = async {
            let tcp = TcpStream::connect(self.cfg.get_addr()).await.map_err(|e| {
                DbError::new(ErrorKind::Connect, format!("cannot reach the server: {e}"))
            })?;
            let _ = tcp.set_nodelay(true);
            let mut client = Client::connect(self.cfg.clone(), tcp.compat_write())
                .await
                .map_err(|e| map_error(&e, true))?;
            let spid = scalar_u64(&mut client, "SELECT CAST(@@SPID AS bigint)")
                .await
                .map_err(|e| map_error(&e, true))?;
            Ok(Conn {
                client,
                spid,
                dead: Arc::new(AtomicBool::new(false)),
            })
        };
        match tokio::time::timeout(CONNECT_TIMEOUT, fut).await {
            Ok(r) => r,
            Err(_) => Err(DbError::new(ErrorKind::Connect, "connect timed out")),
        }
    }

    async fn acquire(&self) -> Result<(Conn, OwnedSemaphorePermit), DbError> {
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| DbError::new(ErrorKind::Internal, "the driver is closed"))?;
        loop {
            let idle = self.idle.lock().pop();
            match idle {
                Some(c) if !c.dead.load(Ordering::SeqCst) => return Ok((c, permit)),
                Some(_) => continue,
                None => return self.connect_one().await.map(|c| (c, permit)),
            }
        }
    }

    fn release(&self, conn: Conn) {
        if !conn.dead.load(Ordering::SeqCst) {
            self.idle.lock().push(conn);
        }
    }

    /// `KILL <spid>` on the control connection, reconnecting once if it died.
    async fn kill(&self, spid: u64) -> Result<(), DbError> {
        let mut guard = self.control.lock().await;
        let sql = format!("KILL {spid}");
        for attempt in 0..2 {
            if guard.is_none() {
                *guard = Some(self.connect_one().await?);
            }
            let client = &mut guard.as_mut().unwrap().client;
            let r = match client.simple_query(sql.as_str()).await {
                Ok(s) => s.into_results().await.map(drop),
                Err(e) => Err(e),
            };
            match r {
                Ok(()) => return Ok(()),
                // 6106: the session is already gone.
                Err(tiberius::error::Error::Server(t)) if t.code() == 6106 => return Ok(()),
                Err(e @ tiberius::error::Error::Server(_)) => return Err(map_error(&e, false)),
                Err(_) if attempt == 0 => *guard = None,
                Err(e) => return Err(map_error(&e, false)),
            }
        }
        Err(DbError::new(
            ErrorKind::Connect,
            "the control connection is down",
        ))
    }
}

async fn scalar_u64(client: &mut Tds, sql: &str) -> Result<u64, tiberius::error::Error> {
    let row = client.simple_query(sql).await?.into_row().await?;
    Ok(row
        .and_then(|r| r.get::<i64, _>(0))
        .map(|v| v.max(0) as u64)
        .unwrap_or(0))
}

impl MssqlDriver {
    /// Connects and checks the credentials. All I/O runs on `rt`; the returned
    /// future can be awaited from any executor.
    ///
    /// `opts.database` is the initial database (`None` = the login's default).
    /// TLS: `Require` verifies the certificate, `RequireUnverified` encrypts
    /// without checking, `Disabled` encrypts only the login packet (TDS "not
    /// supported" level) and sends the rest in plain text.
    pub fn connect(
        rt: &Handle,
        opts: ConnectOptions,
    ) -> impl Future<Output = Result<MssqlDriver, DbError>> + Send + 'static {
        let rt2 = rt.clone();
        let task = rt.spawn(async move {
            let inner = Arc::new(Inner {
                rt: rt2,
                cfg: make_config(&opts),
                idle: Mutex::new(Vec::new()),
                slots: Arc::new(Semaphore::new(POOL_MAX)),
                control: tokio::sync::Mutex::new(None),
            });
            let first = inner.connect_one().await?;
            inner.idle.lock().push(first);
            Ok(MssqlDriver { inner })
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

// ---- cancel plumbing ----

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

// ---- statements ----

pub(crate) enum Stmt {
    Text(String),
    Prepared(String, Vec<Param>),
}

pub(crate) enum Source {
    Pool,
    Pinned(Arc<tokio::sync::Mutex<Option<Conn>>>),
}

enum Lease {
    Pooled(Option<Conn>, #[allow(dead_code)] OwnedSemaphorePermit),
    Pinned(tokio::sync::OwnedMutexGuard<Option<Conn>>),
}

impl Lease {
    fn conn(&mut self) -> Option<&mut Conn> {
        match self {
            Lease::Pooled(c, _) => c.as_mut(),
            Lease::Pinned(g) => g.as_mut(),
        }
    }
}

enum StreamErr {
    Db(tiberius::error::Error),
    Stopped,
}

impl From<tiberius::error::Error> for StreamErr {
    fn from(e: tiberius::error::Error) -> Self {
        StreamErr::Db(e)
    }
}

pub(crate) fn spawn_query(
    inner: &Arc<Inner>,
    source: Source,
    stmt: Stmt,
    opts: QueryOptions,
) -> QueryHandle {
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
    if conn.dead.load(Ordering::SeqCst) {
        return Outcome::Failed(DbError::new(
            ErrorKind::Connect,
            "the session was ended (a cancelled statement ends its SQL Server session)",
        ));
    }
    if sink
        .send(QueryEvent::Started {
            session_id: conn.spid,
        })
        .await
        .is_err()
    {
        return Outcome::Cancelled;
    }
    let (spid, dead) = (conn.spid, conn.dead.clone());
    let (result, killed) = {
        let work = stream(
            conn,
            &stmt,
            opts.batch_rows.max(1),
            sink,
            ctl,
            stop_rx.clone(),
        );
        tokio::pin!(work);
        let timer = async {
            match opts.timeout {
                Some(limit) => tokio::time::sleep(limit).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(timer);
        let mut killed = false;
        let mut timer_done = false;
        let result = loop {
            tokio::select! {
                r = &mut work => break r,
                _ = stopped(&mut stop_rx), if !killed => {
                    killed = true;
                    dead.store(true, Ordering::SeqCst);
                    let _ = inner.kill(spid).await;
                }
                _ = &mut timer, if !timer_done && !killed => {
                    timer_done = true;
                    ctl.stop_with(TIMED_OUT);
                }
            }
        };
        (result, killed)
    };
    match &result {
        Err(StreamErr::Db(tiberius::error::Error::Server(_))) => {}
        // Transport failure, or stopped mid-stream with unread data left.
        Err(_) => dead.store(true, Ordering::SeqCst),
        Ok(_) => {}
    }
    if let Lease::Pooled(c, _) = &mut lease
        && let Some(c) = c.take()
    {
        inner.release(c);
    }
    drop(lease);
    match result {
        Ok(rows) if !killed => Outcome::Done { rows },
        Ok(_) | Err(StreamErr::Stopped) => stop_outcome(ctl),
        Err(StreamErr::Db(_)) if killed => stop_outcome(ctl),
        Err(StreamErr::Db(e)) => Outcome::Failed(map_error(&e, false)),
    }
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
) -> Result<(), StreamErr> {
    if *stop.borrow() {
        return Err(StreamErr::Stopped);
    }
    tokio::select! {
        sent = sink.send(event) => match sent {
            Ok(()) => Ok(()),
            Err(_) => {
                ctl.stop_with(CANCELLED);
                Err(StreamErr::Stopped)
            }
        },
        _ = stopped(stop) => Err(StreamErr::Stopped),
    }
}

async fn stream(
    conn: &mut Conn,
    stmt: &Stmt,
    batch_rows: usize,
    sink: &db_core::EventSink,
    ctl: &Ctl,
    mut stop: watch::Receiver<bool>,
) -> Result<u64, StreamErr> {
    let mut total = 0;
    match stmt {
        Stmt::Text(sql) => {
            for b in split::batches(sql) {
                if *stop.borrow() {
                    return Err(StreamErr::Stopped);
                }
                total += one_batch(conn, &b, &[], batch_rows, sink, ctl, &mut stop).await?;
            }
        }
        Stmt::Prepared(sql, params) => {
            let sql = split::bind_placeholders(sql);
            total += one_batch(conn, &sql, params, batch_rows, sink, ctl, &mut stop).await?;
        }
    }
    Ok(total)
}

async fn one_batch(
    conn: &mut Conn,
    sql: &str,
    params: &[Param],
    batch_rows: usize,
    sink: &db_core::EventSink,
    ctl: &Ctl,
    stop: &mut watch::Receiver<bool>,
) -> Result<u64, StreamErr> {
    let binds: Vec<Bind> = params.iter().map(Bind::from).collect();
    let refs: Vec<&dyn tiberius::ToSql> = binds.iter().map(|b| b as &dyn tiberius::ToSql).collect();
    let mut items = if params.is_empty() {
        conn.client.simple_query(sql).await?
    } else {
        conn.client.query(sql, &refs).await?
    };
    let mut builder: Option<BatchBuilder> = None;
    let mut ncols = 0;
    let mut total = 0u64;
    let mut any_result = false;
    loop {
        let pending = builder.as_ref().is_some_and(|b| b.rows() > 0);
        let next = if pending {
            tokio::select! {
                m = items.try_next() => m?,
                _ = tokio::time::sleep(PARTIAL_FLUSH) => {
                    let batch = builder.as_mut().unwrap().finish();
                    send(sink, ctl, stop, QueryEvent::Batch(batch)).await?;
                    continue;
                }
            }
        } else {
            items.try_next().await?
        };
        let Some(item) = next else { break };
        match item {
            QueryItem::Metadata(meta) => {
                if let Some(b) = builder.as_mut()
                    && b.rows() > 0
                {
                    send(sink, ctl, stop, QueryEvent::Batch(b.finish())).await?;
                }
                let cols = meta.columns();
                if cols.is_empty() {
                    builder = None;
                    continue;
                }
                any_result = true;
                ncols = cols.len();
                let metas = cols
                    .iter()
                    .map(|c| ColumnMeta {
                        name: c.name().to_string(),
                        type_name: type_name(c.column_type()).to_string(),
                        nullable: None,
                        origin: None,
                    })
                    .collect();
                send(sink, ctl, stop, QueryEvent::Columns(metas)).await?;
                builder = Some(BatchBuilder::new(ncols));
            }
            QueryItem::Row(row) => {
                let Some(b) = builder.as_mut() else { continue };
                for cell in row.into_iter().take(ncols) {
                    match cell_value(&cell) {
                        None => b.push(None),
                        Some(Value::Bytes(x)) => b.push(Some(&x)),
                        Some(Value::Text(t)) => b.push(Some(t.as_bytes())),
                    }
                }
                b.end_row();
                total += 1;
                if b.rows() >= batch_rows {
                    let batch = b.finish();
                    send(sink, ctl, stop, QueryEvent::Batch(batch)).await?;
                }
            }
        }
    }
    drop(items);
    if let Some(b) = builder.as_mut()
        && b.rows() > 0
    {
        send(sink, ctl, stop, QueryEvent::Batch(b.finish())).await?;
    }
    if !any_result {
        // No result set: INSERT / UPDATE / DDL. @@ROWCOUNT survives into the next batch.
        let n = scalar_u64(&mut conn.client, "SELECT CAST(@@ROWCOUNT AS bigint)").await?;
        send(sink, ctl, stop, QueryEvent::Affected { rows: n }).await?;
    }
    Ok(total)
}

/// A bound value: text as NVARCHAR, bytes as VARBINARY, NULL as an NVARCHAR
/// NULL (the server converts it to the target type).
enum Bind {
    Null,
    Text(String),
    Bytes(Vec<u8>),
}

impl From<&Param> for Bind {
    fn from(p: &Param) -> Self {
        match p {
            Param::Null => Bind::Null,
            Param::Text(s) => Bind::Text(s.clone()),
            Param::Bytes(b) => Bind::Bytes(b.clone()),
        }
    }
}

impl tiberius::ToSql for Bind {
    fn to_sql(&self) -> ColumnData<'_> {
        match self {
            Bind::Null => ColumnData::String(None),
            Bind::Text(s) => ColumnData::String(Some(s.as_str().into())),
            Bind::Bytes(b) => ColumnData::Binary(Some(b.as_slice().into())),
        }
    }
}

enum Value {
    Text(String),
    Bytes(Vec<u8>),
}

/// A value as the grid shows it. `None` is NULL.
fn cell_value(d: &ColumnData<'static>) -> Option<Value> {
    use ColumnData as D;
    let text = Value::Text;
    match d {
        D::U8(v) => v.map(|v| text(v.to_string())),
        D::I16(v) => v.map(|v| text(v.to_string())),
        D::I32(v) => v.map(|v| text(v.to_string())),
        D::I64(v) => v.map(|v| text(v.to_string())),
        D::F32(v) => v.map(|v| text(v.to_string())),
        D::F64(v) => v.map(|v| text(v.to_string())),
        D::Bit(v) => v.map(|v| text(if v { "1" } else { "0" }.to_string())),
        D::String(v) => v.as_ref().map(|s| text(s.to_string())),
        D::Guid(v) => v.map(|g| text(g.to_string().to_uppercase())),
        D::Binary(v) => v.as_ref().map(|b| Value::Bytes(b.to_vec())),
        D::Numeric(v) => v.map(|n| text(n.to_string())),
        D::Xml(v) => v.as_ref().map(|x| text(x.to_string())),
        D::DateTime(_) | D::SmallDateTime(_) | D::DateTime2(_) => {
            chrono::NaiveDateTime::from_sql(d)
                .ok()
                .flatten()
                .map(|v| text(v.format("%Y-%m-%d %H:%M:%S%.f").to_string()))
        }
        D::Date(_) => chrono::NaiveDate::from_sql(d)
            .ok()
            .flatten()
            .map(|v| text(v.format("%Y-%m-%d").to_string())),
        D::Time(_) => chrono::NaiveTime::from_sql(d)
            .ok()
            .flatten()
            .map(|v| text(v.format("%H:%M:%S%.f").to_string())),
        D::DateTimeOffset(_) => chrono::DateTime::<chrono::FixedOffset>::from_sql(d)
            .ok()
            .flatten()
            .map(|v| text(v.format("%Y-%m-%d %H:%M:%S%.f %:z").to_string())),
    }
}

/// SQL Server's own type names for the column header.
fn type_name(t: tiberius::ColumnType) -> &'static str {
    use tiberius::ColumnType as T;
    match t {
        T::Null => "NULL",
        T::Bit | T::Bitn => "BIT",
        T::Int1 => "TINYINT",
        T::Int2 => "SMALLINT",
        T::Int4 | T::Intn => "INT",
        T::Int8 => "BIGINT",
        T::Float4 => "REAL",
        T::Float8 | T::Floatn => "FLOAT",
        T::Money | T::Money4 => "MONEY",
        T::Decimaln => "DECIMAL",
        T::Numericn => "NUMERIC",
        T::Guid => "UNIQUEIDENTIFIER",
        T::Datetime | T::Datetimen => "DATETIME",
        T::Datetime4 => "SMALLDATETIME",
        T::Daten => "DATE",
        T::Timen => "TIME",
        T::Datetime2 => "DATETIME2",
        T::DatetimeOffsetn => "DATETIMEOFFSET",
        T::BigVarBin => "VARBINARY",
        T::BigBinary => "BINARY",
        T::BigVarChar => "VARCHAR",
        T::BigChar => "CHAR",
        T::NVarchar => "NVARCHAR",
        T::NChar => "NCHAR",
        T::Xml => "XML",
        T::Udt => "UDT",
        T::Text => "TEXT",
        T::Image => "IMAGE",
        T::NText => "NTEXT",
        T::SSVariant => "SQL_VARIANT",
    }
}

// ---- Driver ----

pub(crate) fn quote_identifier(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

impl Driver for MssqlDriver {
    fn kind(&self) -> &'static str {
        "sqlserver"
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

fn kill_session(inner: &Arc<Inner>, spid: u64) -> QueryHandle {
    struct Noop;
    impl QueryControl for Noop {
        fn cancel(&self) {}
    }
    let (sink, handle) = channel(2, Arc::new(Noop));
    let inner = inner.clone();
    inner.rt.clone().spawn(async move {
        let outcome = match inner.kill(spid).await {
            Ok(()) => Outcome::Done { rows: 0 },
            Err(e) => Outcome::Failed(e),
        };
        let _ = sink.send(QueryEvent::Finished(outcome)).await;
    });
    handle
}

/// A [`Driver`] pinned to one server connection, for a transaction
/// (`BEGIN TRAN`, statements, `COMMIT` / `ROLLBACK`), session settings or
/// `#temp` tables. Statements run one at a time in the order they were
/// started. Cancelling a statement ends the session (see the crate docs), so
/// an open transaction is rolled back. Dropping the session closes the
/// connection, which rolls back an unfinished transaction.
pub struct MssqlSession {
    inner: Arc<Inner>,
    conn: Arc<tokio::sync::Mutex<Option<Conn>>>,
    spid: u64,
    _slot: OwnedSemaphorePermit,
}

impl MssqlDriver {
    /// Takes a connection from the pool and pins a [`MssqlSession`] to it.
    pub fn session(&self) -> impl Future<Output = Result<MssqlSession, DbError>> + Send + 'static {
        let inner = self.inner.clone();
        let task = self.inner.rt.spawn(async move {
            let (conn, slot) = inner.acquire().await?;
            let spid = conn.spid;
            Ok(MssqlSession {
                inner,
                conn: Arc::new(tokio::sync::Mutex::new(Some(conn))),
                spid,
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

impl MssqlSession {
    /// The server session id (`@@SPID`, what `Started` reports).
    pub fn session_id(&self) -> u64 {
        self.spid
    }
}

impl Drop for MssqlSession {
    fn drop(&mut self) {
        let (conn, inner, spid) = (self.conn.clone(), self.inner.clone(), self.spid);
        self.inner.rt.spawn(async move {
            // A statement still running holds the lock: end the session on the
            // server so it lets go, then close the connection.
            let taken = match conn.try_lock() {
                Ok(mut g) => g.take(),
                Err(_) => {
                    let _ = inner.kill(spid).await;
                    conn.lock().await.take()
                }
            };
            drop(taken);
        });
    }
}

impl Driver for MssqlSession {
    fn kind(&self) -> &'static str {
        "sqlserver"
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
    fn brackets_quote_names() {
        assert_eq!(quote_identifier("a]b"), "[a]]b]");
        assert_eq!(quote_identifier("dbo"), "[dbo]");
    }
}
