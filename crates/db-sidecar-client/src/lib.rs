//! Client for the Go sidecar (`sidecar/`), implementing [`db_core::Driver`].
//!
//! The sidecar is a child process that speaks binary frames on stdin /
//! stdout (`sidecar/PROTOCOL.md`). Rules measured in the POC (SUMMARY §9):
//!
//! * cancel = cancel frame; the sidecar stops the statement on the server
//!   with `KILL QUERY` from a second connection;
//! * credits bound the batches in flight, so a consumer that stops reading
//!   stops the sidecar;
//! * the host keeps the server session id of every running query;
//! * shutdown closes stdin first and gives the sidecar time to clean up;
//!   a Job Object with KILL_ON_JOB_CLOSE is deliberately **not** used (it
//!   kills the sidecar before it can send KILL);
//! * if the sidecar dies, its running queries finish with `Failed(Internal)`
//!   and a fresh sidecar kills the saved sessions.
//!
//! All I/O runs on the tokio runtime handed to [`SidecarDriver::spawn`], so
//! the futures and handles can be driven from any executor (GPUI's included).

mod catalog;
mod process;
mod proto;

use db_core::{
    ConnectOptions, DEFAULT_WINDOW, DbError, Driver, ErrorKind, Outcome, Param, QueryControl,
    QueryEvent, QueryHandle, QueryOptions, channel, finished,
};
use parking_lot::Mutex;
use process::{Entry, OnCrash, Proc};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

pub use process::ShutdownReport;
pub use proto::{BUILD, HelloInfo, ReadyInfo};

/// Checks a `sidecar.exe` without connecting anywhere: runs it with
/// `--hello` and compares its protocol and build with this host's
/// ([`BUILD`]). Blocking; takes one process start.
pub fn probe(exe: &std::path::Path) -> Result<HelloInfo, DbError> {
    process::probe(exe)
}

/// How long [`SidecarDriver::shutdown`] waits for the sidecar to clean up and
/// exit before it terminates the process. The POC needed about 250 ms.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

/// Events one query can have unread: started, columns, `DEFAULT_WINDOW`
/// batches and finished. The channel is this big so the single reader task
/// never waits for a slow consumer (credits already bound the batches).
const STREAM_CAPACITY: usize = DEFAULT_WINDOW + 3;

/// A connection to the database through a Go sidecar process.
pub struct SidecarDriver {
    inner: Arc<Inner>,
}

struct Inner {
    rt: tokio::runtime::Handle,
    exe: PathBuf,
    /// `SIDECAR_DRIVER`: `mysql` or `oracle`.
    driver: String,
    opts: ConnectOptions,
    /// Data-dictionary reads of `Catalog` stop after this many rows (Oracle).
    catalog_row_cap: AtomicU32,
    current: Mutex<Option<Arc<Proc>>>,
    /// Serialises sidecar starts.
    start: tokio::sync::Mutex<()>,
    next_qid: AtomicU32,
    shut: AtomicBool,
    me: Weak<Inner>,
}

impl SidecarDriver {
    /// Starts the sidecar and waits until it has connected to the database.
    /// Connect, auth and TLS failures come back as the matching [`ErrorKind`].
    /// The work runs on `rt`; the returned future can be awaited on any
    /// executor.
    pub fn spawn(
        rt: &tokio::runtime::Handle,
        exe: PathBuf,
        opts: ConnectOptions,
    ) -> impl Future<Output = Result<SidecarDriver, DbError>> + Send + 'static {
        Self::spawn_driver(rt, exe, "mysql", opts)
    }

    /// Like spawn for another database: `driver` is the
    /// sidecar's `SIDECAR_DRIVER` (mysql, oracle). For Oracle,
    /// `opts.database` is the service name.
    pub fn spawn_driver(
        rt: &tokio::runtime::Handle,
        exe: PathBuf,
        driver: &str,
        opts: ConnectOptions,
    ) -> impl Future<Output = Result<SidecarDriver, DbError>> + Send + 'static {
        let rt2 = rt.clone();
        let driver = driver.to_string();
        let job = rt.spawn(async move {
            let inner = Arc::new_cyclic(|me| Inner {
                rt: rt2,
                exe,
                driver,
                opts,
                catalog_row_cap: AtomicU32::new(catalog::DEFAULT_ROW_CAP),
                current: Mutex::new(None),
                start: tokio::sync::Mutex::new(()),
                next_qid: AtomicU32::new(1),
                shut: AtomicBool::new(false),
                me: me.clone(),
            });
            inner.ensure_proc().await?;
            Ok(SidecarDriver { inner })
        });
        async move {
            job.await.unwrap_or_else(|e| {
                Err(DbError::new(
                    ErrorKind::Internal,
                    format!("sidecar start task failed: {e}"),
                ))
            })
        }
    }

    /// Orderly shutdown: closes stdin, waits up to [`SHUTDOWN_GRACE`] for the
    /// sidecar to kill its running statements and exit, and terminates it only
    /// after that. `None` if no sidecar is running.
    pub fn shutdown(&self) -> impl Future<Output = Option<ShutdownReport>> + Send + 'static {
        self.inner.shut.store(true, Ordering::SeqCst);
        let proc = self.inner.current.lock().take();
        let job = self.inner.rt.spawn(async move {
            match proc {
                Some(p) => Some(p.shutdown(SHUTDOWN_GRACE).await),
                None => None,
            }
        });
        async move { job.await.unwrap_or(None) }
    }

    /// Caps the rows of every data-dictionary read the catalog makes on
    /// Oracle (default 20 000; MySQL reads are not capped).
    pub fn set_catalog_row_cap(&self, rows: u32) {
        self.inner
            .catalog_row_cap
            .store(rows.max(1), Ordering::Relaxed);
    }

    /// Pins one database connection for a run of statements that must stay
    /// together (a transaction: START TRANSACTION, statements, COMMIT /
    /// ROLLBACK). Dropping the session closes the connection, which rolls
    /// back anything unfinished.
    pub fn session(
        &self,
    ) -> impl Future<Output = Result<SidecarSession, DbError>> + Send + 'static {
        let inner = self.inner.clone();
        let job = self
            .inner
            .rt
            .spawn(async move { inner.open_session().await });
        async move {
            job.await.unwrap_or_else(|e| {
                Err(DbError::new(
                    ErrorKind::Internal,
                    format!("session task failed: {e}"),
                ))
            })
        }
    }

    /// Process id of the running sidecar, if any.
    pub fn sidecar_pid(&self) -> Option<u32> {
        self.inner.current.lock().as_ref().map(|p| p.pid)
    }

    /// What the sidecar reported when it became ready.
    pub fn ready_info(&self) -> Option<ReadyInfo> {
        self.inner.current.lock().as_ref().map(|p| p.info.clone())
    }

    /// Server session ids of the queries running now (the ids the host would
    /// kill after a sidecar crash).
    pub fn running_sessions(&self) -> Vec<u64> {
        self.inner
            .current
            .lock()
            .as_ref()
            .map(|p| p.sessions())
            .unwrap_or_default()
    }

    /// Batch frames the current sidecar has sent so far. For tests that check
    /// back-pressure.
    pub fn batch_frames_received(&self) -> u64 {
        self.inner
            .current
            .lock()
            .as_ref()
            .map(|p| p.batch_frames.load(Ordering::Relaxed))
            .unwrap_or(0)
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // No orderly shutdown was requested: still close stdin first so the
        // sidecar cleans up, and terminate it only if it hangs.
        if let Some(p) = self.current.get_mut().take() {
            p.close_stdin();
            drop(self.rt.spawn(async move {
                p.shutdown(SHUTDOWN_GRACE).await;
            }));
        }
    }
}

impl Inner {
    /// The running sidecar, starting one if there is none.
    async fn ensure_proc(&self) -> Result<Arc<Proc>, DbError> {
        let usable = |g: &Option<Arc<Proc>>| g.as_ref().filter(|p| p.usable()).cloned();
        if let Some(p) = usable(&self.current.lock()) {
            return Ok(p);
        }
        let _start = self.start.lock().await;
        if let Some(p) = usable(&self.current.lock()) {
            return Ok(p);
        }
        if self.shut.load(Ordering::SeqCst) {
            return Err(DbError::new(
                ErrorKind::Internal,
                "the sidecar driver is shut down",
            ));
        }
        let me = self.me.clone();
        let on_crash: OnCrash = Arc::new(move |proc, sessions| {
            if let Some(inner) = me.upgrade() {
                inner.crashed(proc, sessions);
            }
        });
        let p = process::spawn_proc(&self.exe, &self.driver, &self.opts, on_crash).await?;
        *self.current.lock() = Some(p.clone());
        Ok(p)
    }

    /// The sidecar process died without being asked to. Its queries already
    /// finished with `Failed(Internal)`; start a fresh sidecar and kill the
    /// sessions they were running in, because the server keeps executing an
    /// orphaned statement (about 4.9 s for SLEEP in the POC).
    fn crashed(self: Arc<Self>, proc: Arc<Proc>, sessions: Vec<u64>) {
        {
            let mut cur = self.current.lock();
            if cur.as_ref().is_some_and(|c| Arc::ptr_eq(c, &proc)) {
                *cur = None;
            }
        }
        if sessions.is_empty() || self.shut.load(Ordering::SeqCst) {
            return;
        }
        let rt = self.rt.clone();
        rt.spawn(async move {
            for id in sessions {
                for attempt in 0..3 {
                    let mut h = self.clone().kill_session_handle(id);
                    let mut failed = false;
                    while let Some(ev) = h.next().await {
                        if let QueryEvent::Finished(Outcome::Failed(_)) = ev {
                            failed = true;
                        }
                    }
                    if !failed {
                        break;
                    }
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            }
        });
    }

    fn start_request(
        self: Arc<Self>,
        build: impl FnOnce(u32) -> Result<Vec<u8>, DbError> + Send + 'static,
        initial_credit: bool,
        pin: Option<Arc<Proc>>,
    ) -> QueryHandle {
        let qid = self.next_qid.fetch_add(1, Ordering::Relaxed);
        let ctl = Arc::new(Ctl {
            qid,
            state: Mutex::new(CtlState::default()),
        });
        let (sink, handle) = channel(STREAM_CAPACITY, ctl.clone());
        let rt = self.rt.clone();
        rt.spawn(async move { self.run(qid, ctl, sink, build, initial_credit, pin).await });
        handle
    }

    async fn run(
        self: Arc<Self>,
        qid: u32,
        ctl: Arc<Ctl>,
        sink: db_core::EventSink,
        build: impl FnOnce(u32) -> Result<Vec<u8>, DbError>,
        initial_credit: bool,
        pin: Option<Arc<Proc>>,
    ) {
        let request = match build(qid) {
            Ok(r) => r,
            Err(e) => return fail(sink, e).await,
        };
        let entry = Entry::new(sink);
        // A sidecar that died between ensure_proc and register is replaced once.
        for attempt in 0..2 {
            // A pinned session lives in one sidecar process only.
            let proc = match &pin {
                Some(p) => p.clone(),
                None => match self.ensure_proc().await {
                    Ok(p) => p,
                    Err(e) => return fail_entry(&entry, e).await,
                },
            };
            // Attach under the control lock so a concurrent cancel() either
            // sees the process (and sends the cancel frame) or is seen here.
            enum Step {
                Cancelled,
                Retry,
                Sent,
            }
            let step = {
                let mut st = ctl.state.lock();
                if st.cancelled {
                    Step::Cancelled
                } else if proc.register(qid, entry.clone()).is_err() {
                    Step::Retry
                } else {
                    st.proc = Some(proc.clone());
                    Step::Sent
                }
            };
            match step {
                Step::Cancelled => {
                    let _ = entry
                        .sink
                        .send(QueryEvent::Finished(Outcome::Cancelled))
                        .await;
                    return;
                }
                Step::Retry if attempt == 1 || pin.is_some() => {
                    return fail_entry(
                        &entry,
                        DbError::new(ErrorKind::Internal, "the sidecar is not running"),
                    )
                    .await;
                }
                Step::Retry => continue,
                Step::Sent => {}
            }
            proc.send(request);
            if initial_credit {
                proc.send(proto::credit_frame(qid, DEFAULT_WINDOW as u32));
            }
            return;
        }
    }

    fn kill_session_handle(self: Arc<Self>, id: u64) -> QueryHandle {
        self.start_request(
            move |qid| Ok(proto::frame(proto::KILL_SESSION, qid, &id.to_le_bytes())),
            false,
            None,
        )
    }
}

async fn fail(sink: db_core::EventSink, e: DbError) {
    let _ = sink.send(QueryEvent::Finished(Outcome::Failed(e))).await;
}

async fn fail_entry(entry: &Entry, e: DbError) {
    let _ = entry
        .sink
        .send(QueryEvent::Finished(Outcome::Failed(e)))
        .await;
}

#[derive(Default)]
struct CtlState {
    proc: Option<Arc<Proc>>,
    cancelled: bool,
}

/// The switch a [`QueryHandle`] holds. Works before the query reached a
/// sidecar (the query is then dropped instead of sent).
struct Ctl {
    qid: u32,
    state: Mutex<CtlState>,
}

impl QueryControl for Ctl {
    fn cancel(&self) {
        let mut st = self.state.lock();
        st.cancelled = true;
        if let Some(p) = &st.proc {
            p.send(proto::frame(proto::CANCEL, self.qid, &[]));
        }
    }

    fn batch_consumed(&self) {
        if let Some(p) = &self.state.lock().proc {
            p.send(proto::credit_frame(self.qid, 1));
        }
    }
}

impl Driver for SidecarDriver {
    fn kind(&self) -> &'static str {
        "sidecar"
    }

    fn query(&self, sql: String, opts: QueryOptions) -> QueryHandle {
        let timeout_ms = opts
            .timeout
            .map_or(0, |t| t.as_millis().clamp(1, u32::MAX as u128) as u32);
        let batch_rows = opts.batch_rows.min(u32::MAX as usize) as u32;
        if sql.len() + 8 > proto::MAX_OUTBOUND {
            return finished(Outcome::Failed(DbError::new(
                ErrorKind::Query,
                "the statement is too large",
            )));
        }
        self.inner.clone().start_request(
            move |qid| Ok(proto::query_frame(qid, timeout_ms, batch_rows, &sql)),
            true,
            None,
        )
    }

    fn execute(&self, sql: String, params: Vec<Param>, opts: QueryOptions) -> QueryHandle {
        self.inner.clone().run_statement(0, None, sql, params, opts)
    }

    fn kill_session(&self, session_id: u64) -> QueryHandle {
        self.inner.clone().kill_session_handle(session_id)
    }

    fn quote_identifier(&self, name: &str) -> String {
        quote(&self.inner.driver, name)
    }

    fn catalog(&self) -> Option<&dyn db_core::Catalog> {
        catalog::Dialect::of(&self.inner.driver).map(|_| self as &dyn db_core::Catalog)
    }
}

/// Identifier quoting by database: MySQL backticks, SQL Server brackets,
/// Oracle (and anything else) double quotes.
fn quote(driver: &str, name: &str) -> String {
    match driver {
        "mysql" => format!("`{}`", name.replace('`', "``")),
        "mssql" => format!("[{}]", name.replace(']', "]]")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

/// A [Driver] pinned to one database connection of the sidecar, from
/// [SidecarDriver::session]. Statements run one at a time in the order
/// they were started; cancel, timeout and back-pressure work as on the
/// driver; a cancelled statement leaves the transaction open (MySQL). Dropping
/// the session cancels what is running, closes the connection and so rolls
/// back an unfinished transaction.
pub struct SidecarSession {
    inner: Arc<Inner>,
    proc: Arc<Proc>,
    id: u32,
    server_session: u64,
}

impl SidecarSession {
    /// The database's own id for this connection (MySQL connection id,
    /// Oracle SID): what Started reports for its statements.
    pub fn session_id(&self) -> u64 {
        self.server_session
    }
}

impl Drop for SidecarSession {
    fn drop(&mut self) {
        // The sidecar cancels a running statement first, then closes (and
        // discards) the connection. A dead sidecar needs no message.
        self.proc
            .send(proto::frame(proto::CLOSE_SESSION, self.id, &[]));
    }
}

impl Driver for SidecarSession {
    fn kind(&self) -> &'static str {
        "sidecar"
    }

    fn query(&self, sql: String, opts: QueryOptions) -> QueryHandle {
        self.inner
            .clone()
            .run_statement(self.id, Some(self.proc.clone()), sql, Vec::new(), opts)
    }

    fn execute(&self, sql: String, params: Vec<Param>, opts: QueryOptions) -> QueryHandle {
        self.inner
            .clone()
            .run_statement(self.id, Some(self.proc.clone()), sql, params, opts)
    }

    fn kill_session(&self, session_id: u64) -> QueryHandle {
        self.inner.clone().kill_session_handle(session_id)
    }

    fn quote_identifier(&self, name: &str) -> String {
        quote(&self.inner.driver, name)
    }
}

impl Inner {
    /// One run frame: session = 0 runs on any pooled connection.
    fn run_statement(
        self: Arc<Self>,
        session: u32,
        pin: Option<Arc<Proc>>,
        sql: String,
        params: Vec<Param>,
        opts: QueryOptions,
    ) -> QueryHandle {
        let timeout_ms = opts
            .timeout
            .map_or(0, |t| t.as_millis().clamp(1, u32::MAX as u128) as u32);
        let batch_rows = opts.batch_rows.min(u32::MAX as usize) as u32;
        self.start_request(
            move |qid| {
                proto::run_frame(qid, session, timeout_ms, batch_rows, &sql, &params).ok_or_else(
                    || {
                        DbError::new(
                            ErrorKind::Query,
                            "the statement or its parameters are too large",
                        )
                    },
                )
            },
            true,
            pin,
        )
    }

    async fn open_session(self: Arc<Self>) -> Result<SidecarSession, DbError> {
        let proc = self.ensure_proc().await?;
        let qid = self.next_qid.fetch_add(1, Ordering::Relaxed);
        let ctl = Arc::new(Ctl {
            qid,
            state: Mutex::new(CtlState::default()),
        });
        let (sink, mut handle) = channel(STREAM_CAPACITY, ctl);
        proc.register(qid, Entry::new(sink))
            .map_err(|_| DbError::new(ErrorKind::Internal, "the sidecar is not running"))?;
        proc.send(proto::frame(proto::OPEN_SESSION, qid, &[]));
        let mut server_session = 0;
        loop {
            match handle.next().await {
                Some(QueryEvent::Started { session_id }) => server_session = session_id,
                Some(QueryEvent::Finished(Outcome::Done { .. })) => break,
                Some(QueryEvent::Finished(Outcome::Failed(e))) => return Err(e),
                Some(QueryEvent::Finished(Outcome::Cancelled)) | None => {
                    return Err(DbError::new(
                        ErrorKind::Internal,
                        "opening the session was interrupted",
                    ));
                }
                Some(_) => {}
            }
        }
        Ok(SidecarSession {
            inner: self,
            proc,
            id: qid,
            server_session,
        })
    }
}
