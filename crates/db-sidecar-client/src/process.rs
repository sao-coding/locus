//! One running sidecar process: spawn + hello/ready handshake, a writer task
//! that owns stdin, a reader task that owns stdout and routes frames to the
//! per-query streams, and a reaper task that owns the `Child`.

use crate::proto::{self, ReadyInfo};
use db_core::{ConnectOptions, DbError, ErrorKind, EventSink, Outcome, QueryEvent, TlsMode};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{ChildStdout, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;

/// How long the sidecar may take to connect and say ready.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) enum Out {
    Frame(Vec<u8>),
    CloseStdin,
}

/// Routing state of one query inside a sidecar process.
pub(crate) struct Entry {
    pub sink: EventSink,
    pub columns: AtomicUsize,
    /// Server session id from the `started` frame; 0 = not started yet.
    pub session: AtomicU64,
}

impl Entry {
    pub fn new(sink: EventSink) -> Arc<Self> {
        Arc::new(Self {
            sink,
            columns: AtomicUsize::new(0),
            session: AtomicU64::new(0),
        })
    }
}

/// Called by the reader when the pipe closed without an orderly shutdown,
/// with the session ids of the queries that were running.
pub(crate) type OnCrash = Arc<dyn Fn(Arc<Proc>, Vec<u64>) + Send + Sync>;

pub(crate) struct Proc {
    pub pid: u32,
    pub info: ReadyInfo,
    tx: mpsc::UnboundedSender<Out>,
    /// `None` once the pipe is closed: nothing new can be registered.
    queries: Mutex<Option<HashMap<u32, Arc<Entry>>>>,
    closing: AtomicBool,
    exited: watch::Receiver<bool>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
    pub batch_frames: AtomicU64,
}

/// Outcome of an orderly shutdown.
#[derive(Clone, Copy, Debug)]
pub struct ShutdownReport {
    /// The sidecar did not exit within the grace period and was terminated.
    pub forced: bool,
    pub elapsed: Duration,
}

fn start_error(e: impl std::fmt::Display) -> DbError {
    DbError::new(
        ErrorKind::Internal,
        format!("cannot start the sidecar: {e}"),
    )
}

/// Starts the sidecar and waits for its ready frame. Credentials travel in
/// environment variables, never on the command line.
pub(crate) async fn spawn_proc(
    exe: &Path,
    driver: &str,
    opts: &ConnectOptions,
    on_crash: OnCrash,
) -> Result<Arc<Proc>, DbError> {
    let mut cmd = Command::new(exe);
    cmd.env("SIDECAR_DRIVER", driver)
        .env("SIDECAR_HOST", &opts.host)
        .env("SIDECAR_PORT", opts.port.to_string())
        .env("SIDECAR_USER", &opts.user)
        .env("SIDECAR_PASSWORD", &opts.password)
        .env(
            "SIDECAR_TLS",
            match opts.tls {
                TlsMode::Require => "require",
                TlsMode::RequireUnverified => "require-unverified",
                TlsMode::Disabled => "disabled",
            },
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    match &opts.database {
        Some(d) => cmd.env("SIDECAR_DATABASE", d),
        None => cmd.env_remove("SIDECAR_DATABASE"),
    };
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    // No kill_on_drop: a sidecar killed with the host would not get to run
    // its KILL QUERY cleanup (measured in the POC).
    let mut child = cmd.spawn().map_err(start_error)?;
    let pid = child.id().ok_or_else(|| start_error("no process id"))?;
    let stdin = child.stdin.take().ok_or_else(|| start_error("no stdin"))?;
    let mut stdout = BufReader::with_capacity(
        1 << 16,
        child
            .stdout
            .take()
            .ok_or_else(|| start_error("no stdout"))?,
    );

    let mut buf = Vec::new();
    // The hello frame comes before the sidecar reads its settings, so a
    // sidecar of another build is stopped before it connects anywhere.
    let hello = timeout(HANDSHAKE_TIMEOUT, proto::read_frame(&mut stdout, &mut buf)).await;
    let mismatch = match hello {
        Ok(Ok(Some((t, _)))) => proto::check_first_frame(t, &buf).err(),
        Ok(Ok(None)) => Some(DbError::new(
            ErrorKind::Internal,
            "the sidecar exited before its hello frame",
        )),
        Ok(Err(e)) => Some(DbError::new(
            ErrorKind::Internal,
            format!("sidecar handshake failed: {e}"),
        )),
        Err(_) => Some(DbError::new(
            ErrorKind::Connect,
            "the sidecar did not start in time",
        )),
    };
    if let Some(mut e) = mismatch {
        let _ = child.kill().await;
        e.message = format!("{}: {}", exe.display(), e.message);
        return Err(e);
    }

    let ready = timeout(HANDSHAKE_TIMEOUT, proto::read_frame(&mut stdout, &mut buf)).await;
    let failure = match ready {
        Ok(Ok(Some((proto::READY, _)))) => match proto::decode_ready(&buf) {
            Ok(info) => {
                let (exit_tx, exit_rx) = watch::channel(false);
                let (kill_tx, kill_rx) = oneshot::channel();
                let (tx, rx) = mpsc::unbounded_channel();
                let proc = Arc::new(Proc {
                    pid,
                    info,
                    tx,
                    queries: Mutex::new(Some(HashMap::new())),
                    closing: AtomicBool::new(false),
                    exited: exit_rx,
                    kill: Mutex::new(Some(kill_tx)),
                    batch_frames: AtomicU64::new(0),
                });
                tokio::spawn(writer(stdin, rx));
                tokio::spawn(reaper(child, kill_rx, exit_tx));
                tokio::spawn(reader(proc.clone(), stdout, on_crash));
                return Ok(proc);
            }
            Err(e) => e,
        },
        Ok(Ok(Some((proto::ERROR, _)))) => proto::decode_error(&buf),
        Ok(Ok(Some((t, _)))) => DbError::new(
            ErrorKind::Internal,
            format!("unexpected frame {t} instead of ready"),
        ),
        Ok(Ok(None)) => DbError::new(
            ErrorKind::Internal,
            "the sidecar exited before it was ready",
        ),
        Ok(Err(e)) => DbError::new(
            ErrorKind::Internal,
            format!("sidecar handshake failed: {e}"),
        ),
        Err(_) => DbError::new(
            ErrorKind::Connect,
            "the sidecar did not become ready in time",
        ),
    };
    // Handshake failed. The sidecar exits on its own after an error frame;
    // give it a moment, then make sure it is gone.
    drop(stdin);
    if timeout(Duration::from_secs(2), child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
    Err(failure)
}

/// How long `sidecar --hello` may take (a first start can wait for a virus scan).
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs `exe --hello` and checks that it is this host's protocol and build.
/// Gives the sidecar no connection settings or credentials; a protocol 1
/// sidecar, which ignores `--hello`, stops at once on the missing host.
pub(crate) fn probe(exe: &Path) -> Result<proto::HelloInfo, DbError> {
    use std::io::Read;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--hello")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for v in [
        "SIDECAR_DRIVER",
        "SIDECAR_HOST",
        "SIDECAR_PORT",
        "SIDECAR_USER",
        "SIDECAR_PASSWORD",
        "SIDECAR_DATABASE",
        "SIDECAR_TLS",
    ] {
        cmd.env_remove(v);
    }
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
    let fail = |e: &dyn std::fmt::Display| {
        DbError::new(
            ErrorKind::Internal,
            format!("{}: cannot check the sidecar: {e}", exe.display()),
        )
    };
    let mut child = cmd.spawn().map_err(|e| fail(&e))?;
    let mut out = child.stdout.take().ok_or_else(|| fail(&"no stdout"))?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = (&mut out).take(64 << 10).read_to_end(&mut b);
        let _ = tx.send(b);
    });
    let bytes = rx.recv_timeout(PROBE_TIMEOUT);
    let _ = child.kill();
    let _ = child.wait();
    let bytes = bytes.map_err(|_| fail(&"no answer to --hello"))?;
    let (t, p) = proto::first_frame(&bytes).ok_or_else(|| fail(&"no hello frame"))?;
    proto::check_first_frame(t, p).map_err(|mut e| {
        e.message = format!("{}: {}", exe.display(), e.message);
        e
    })
}

async fn writer(mut stdin: tokio::process::ChildStdin, mut rx: mpsc::UnboundedReceiver<Out>) {
    while let Some(m) = rx.recv().await {
        match m {
            Out::Frame(b) => {
                if stdin.write_all(&b).await.is_err() {
                    break;
                }
            }
            Out::CloseStdin => break,
        }
    }
    // Dropping stdin is the "host is gone" signal: the sidecar cancels its
    // queries, sends KILL QUERY and exits.
    drop(stdin);
    rx.close();
}

async fn reaper(
    mut child: tokio::process::Child,
    kill_rx: oneshot::Receiver<()>,
    exited: watch::Sender<bool>,
) {
    tokio::select! {
        _ = child.wait() => {}
        Ok(()) = kill_rx => {
            let _ = child.kill().await;
        }
    }
    let _ = child.wait().await;
    let _ = exited.send(true);
}

async fn reader(proc: Arc<Proc>, mut out: BufReader<ChildStdout>, on_crash: OnCrash) {
    let mut buf = Vec::new();
    let cause = loop {
        match proto::read_frame(&mut out, &mut buf).await {
            Ok(Some((t, qid))) => proc.dispatch(t, qid, &buf).await,
            Ok(None) => break "the sidecar closed its output".to_string(),
            Err(e) => break e.to_string(),
        }
    };
    let crashed = !proc.closing.load(Ordering::SeqCst);
    let entries = proc.queries.lock().take().unwrap_or_default();
    let mut sessions = Vec::new();
    for (_, e) in entries {
        match e.session.load(Ordering::SeqCst) {
            0 => {}
            s => sessions.push(s),
        }
        let msg = if crashed {
            format!("sidecar exited unexpectedly: {cause}")
        } else {
            "sidecar was shut down".to_string()
        };
        let _ = e
            .sink
            .send(QueryEvent::Finished(Outcome::Failed(DbError::new(
                ErrorKind::Internal,
                msg,
            ))))
            .await;
    }
    if crashed {
        on_crash(proc, sessions);
    }
}

impl Proc {
    pub fn send(&self, frame: Vec<u8>) -> bool {
        self.tx.send(Out::Frame(frame)).is_ok()
    }

    /// Adds a query; `Err` when the pipe is already closed.
    pub fn register(&self, qid: u32, e: Arc<Entry>) -> Result<(), ()> {
        match self.queries.lock().as_mut() {
            Some(m) => {
                m.insert(qid, e);
                Ok(())
            }
            None => Err(()),
        }
    }

    pub fn usable(&self) -> bool {
        !self.closing.load(Ordering::SeqCst) && self.queries.lock().is_some()
    }

    /// Session ids of the queries running now.
    pub fn sessions(&self) -> Vec<u64> {
        let g = self.queries.lock();
        g.iter()
            .flat_map(|m| m.values())
            .map(|e| e.session.load(Ordering::SeqCst))
            .filter(|s| *s != 0)
            .collect()
    }

    fn take(&self, qid: u32) {
        if let Some(m) = self.queries.lock().as_mut() {
            m.remove(&qid);
        }
    }

    async fn dispatch(&self, t: u8, qid: u32, p: &[u8]) {
        let entry = self
            .queries
            .lock()
            .as_ref()
            .and_then(|m| m.get(&qid).cloned());
        let Some(e) = entry else { return }; // late frame of a finished query
        let event = match t {
            proto::STARTED => match proto::decode_u64(p) {
                Ok(id) => {
                    e.session.store(id, Ordering::SeqCst);
                    QueryEvent::Started { session_id: id }
                }
                Err(err) => return self.fail(qid, &e, err).await,
            },
            proto::COLUMNS => match proto::decode_columns(p) {
                Ok(cols) => {
                    e.columns.store(cols.len(), Ordering::SeqCst);
                    QueryEvent::Columns(cols)
                }
                Err(err) => return self.fail(qid, &e, err).await,
            },
            proto::BATCH => {
                self.batch_frames.fetch_add(1, Ordering::Relaxed);
                match proto::decode_batch(p, e.columns.load(Ordering::SeqCst)) {
                    Ok(b) => QueryEvent::Batch(b),
                    Err(err) => return self.fail(qid, &e, err).await,
                }
            }
            proto::AFFECTED => match proto::decode_u64(p) {
                Ok(rows) => QueryEvent::Affected { rows },
                Err(err) => return self.fail(qid, &e, err).await,
            },
            proto::DONE => {
                self.take(qid);
                QueryEvent::Finished(match proto::decode_u64(p) {
                    Ok(rows) => Outcome::Done { rows },
                    Err(err) => Outcome::Failed(err),
                })
            }
            proto::ERROR => {
                self.take(qid);
                QueryEvent::Finished(Outcome::Failed(proto::decode_error(p)))
            }
            proto::CANCELLED => {
                self.take(qid);
                QueryEvent::Finished(Outcome::Cancelled)
            }
            _ => return,
        };
        // Never waits in practice: the credit window bounds unread batches
        // below the channel capacity. A dropped handle just ends the send.
        let _ = e.sink.send(event).await;
    }

    /// A frame of this query could not be decoded: end it and stop it on
    /// the server.
    async fn fail(&self, qid: u32, e: &Entry, err: DbError) {
        self.take(qid);
        self.send(proto::frame(proto::CANCEL, qid, &[]));
        let _ = e
            .sink
            .send(QueryEvent::Finished(Outcome::Failed(err)))
            .await;
    }

    /// Orderly shutdown: close stdin, give the sidecar `grace` to clean up
    /// (KILL QUERY for every running statement) and exit, terminate only if
    /// it does not.
    pub async fn shutdown(&self, grace: Duration) -> ShutdownReport {
        let t0 = Instant::now();
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.tx.send(Out::CloseStdin);
        let mut exited = self.exited.clone();
        let graceful = timeout(grace, exited.wait_for(|v| *v)).await.is_ok();
        if !graceful {
            if let Some(k) = self.kill.lock().take() {
                let _ = k.send(());
            }
            let _ = timeout(Duration::from_secs(2), exited.wait_for(|v| *v)).await;
        }
        ShutdownReport {
            forced: !graceful,
            elapsed: t0.elapsed(),
        }
    }

    /// Fire-and-forget version of the above for `Drop`.
    pub fn close_stdin(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.tx.send(Out::CloseStdin);
    }
}
