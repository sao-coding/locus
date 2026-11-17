//! Shared helpers for the integration tests. Needs a local MariaDB (`BENCH_DSN`)
//! with the `student` fixture table loaded.
#![allow(dead_code)]

use db_core::{ColumnMeta, ConnectOptions, Outcome, QueryEvent, QueryHandle, ResultSet, TlsMode};
use db_mysql::MysqlDriver;
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Opts, OptsBuilder};
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};
use tokio::runtime::{Handle, Runtime};

pub const BIG_QUERY: &str = "SELECT a.* FROM student_real a CROSS JOIN (SELECT 1 AS n UNION ALL SELECT 2 UNION ALL SELECT 3 \
     UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 \
     UNION ALL SELECT 9 UNION ALL SELECT 10) k";
pub const BIG_ROWS: u64 = 404_500;
pub const SLEEP_QUERY: &str = "SELECT SLEEP(30)";

pub fn rt() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap()
    })
}

pub fn handle() -> &'static Handle {
    rt().handle()
}

/// A minimal executor that is not tokio: the driver's futures must work on
/// any executor (GPUI's, in the app).
pub fn block_on<F: Future>(fut: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let mut fut = std::pin::pin!(fut);
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::thread::park(),
        }
    }
}

/// `BENCH_DSN=mysql://user:pass@host:port/db`; the password is never printed.
pub struct Dsn {
    pub user: String,
    pub password: String,
    pub host: String,
    pub port: u16,
    pub database: String,
}

pub fn dsn() -> Dsn {
    let raw = std::env::var("BENCH_DSN").unwrap_or_else(|_| {
        panic!(
            "BENCH_DSN is not set. Set it in the environment \
             (mysql://user:pass@host:port/db) and start the local MariaDB"
        )
    });
    let rest = raw
        .strip_prefix("mysql://")
        .expect("BENCH_DSN must start with mysql://");
    let (creds, addr) = rest
        .rsplit_once('@')
        .expect("BENCH_DSN needs user:pass@host");
    let (user, password) = creds.split_once(':').expect("BENCH_DSN needs user:pass");
    let (hostport, database) = addr.split_once('/').unwrap_or((addr, ""));
    let (host, port) = hostport.split_once(':').unwrap_or((hostport, "3306"));
    Dsn {
        user: user.into(),
        password: password.into(),
        host: host.into(),
        port: port.parse().expect("BENCH_DSN port"),
        database: database.into(),
    }
}

pub fn connect_options(tls: TlsMode) -> ConnectOptions {
    let d = dsn();
    ConnectOptions {
        host: d.host,
        port: d.port,
        user: d.user,
        password: d.password,
        database: Some(d.database),
        tls,
    }
}

pub fn connect() -> MysqlDriver {
    // Plain text: the local throwaway server has no TLS.
    block_on(MysqlDriver::connect(
        handle(),
        connect_options(TlsMode::Disabled),
    ))
    .expect("connect to BENCH_DSN")
}

pub struct SetData {
    pub columns: Vec<ColumnMeta>,
    pub rows: ResultSet,
    pub batch_sizes: Vec<usize>,
}

/// One result of a call, in the order it arrived.
#[derive(Debug, PartialEq, Eq)]
pub enum Shape {
    /// A result set (the n-th, counting from 0).
    Set(usize),
    /// A statement without a result set.
    Affected(u64),
}

/// Everything one query sent, with the event order checked
/// (`Started -> (Columns -> Batch* | Affected)* -> Finished`).
/// `columns` / `rows` / `batch_sizes` are those of the first result set (the
/// only one for a single statement); `order` lists every result in arrival
/// order and `set(i)` reaches any result set.
pub struct Collected {
    pub session_id: u64,
    pub columns: Vec<ColumnMeta>,
    pub rows: ResultSet,
    pub batch_sizes: Vec<usize>,
    more: Vec<SetData>,
    pub order: Vec<Shape>,
    pub outcome: Outcome,
}

impl Collected {
    pub fn set_count(&self) -> usize {
        self.order
            .iter()
            .filter(|s| matches!(s, Shape::Set(_)))
            .count()
    }

    /// Columns and rows of the i-th result set.
    pub fn set(&self, i: usize) -> (&Vec<ColumnMeta>, &ResultSet) {
        if i == 0 {
            (&self.columns, &self.rows)
        } else {
            (&self.more[i - 1].columns, &self.more[i - 1].rows)
        }
    }

    /// The `Affected` counts, in order.
    pub fn affected(&self) -> Vec<u64> {
        self.order
            .iter()
            .filter_map(|s| {
                if let Shape::Affected(n) = s {
                    Some(*n)
                } else {
                    None
                }
            })
            .collect()
    }
}

pub fn collect(h: &mut QueryHandle) -> Collected {
    let mut session_id = None;
    let mut sets: Vec<SetData> = vec![];
    let mut order = vec![];
    loop {
        match h.blocking_next().expect("stream ended without Finished") {
            QueryEvent::Started { session_id: id } => {
                assert!(
                    session_id.is_none() && order.is_empty(),
                    "Started must come first"
                );
                session_id = Some(id);
            }
            QueryEvent::Columns(c) => {
                assert!(session_id.is_some(), "Columns must follow Started");
                assert!(
                    !c.is_empty(),
                    "a result set has columns; a statement without one sends Affected"
                );
                order.push(Shape::Set(sets.len()));
                sets.push(SetData {
                    rows: ResultSet::new(c.clone()),
                    columns: c,
                    batch_sizes: vec![],
                });
            }
            QueryEvent::Affected { rows } => {
                assert!(session_id.is_some(), "Affected must follow Started");
                order.push(Shape::Affected(rows));
            }
            QueryEvent::Batch(b) => {
                assert!(
                    matches!(order.last(), Some(Shape::Set(_))),
                    "Batch outside a result set"
                );
                let cur = sets.last_mut().unwrap();
                cur.batch_sizes.push(b.rows());
                cur.rows.push(b);
            }
            QueryEvent::Finished(outcome) => {
                assert!(h.blocking_next().is_none(), "events after Finished");
                let first = if sets.is_empty() {
                    SetData {
                        columns: vec![],
                        rows: ResultSet::default(),
                        batch_sizes: vec![],
                    }
                } else {
                    sets.remove(0)
                };
                return Collected {
                    session_id: session_id.unwrap_or(0),
                    columns: first.columns,
                    rows: first.rows,
                    batch_sizes: first.batch_sizes,
                    more: sets,
                    order,
                    outcome,
                };
            }
        }
    }
}

/// Reads events until `Finished`, discarding everything else.
pub fn finish(h: &mut QueryHandle) -> Outcome {
    loop {
        if let QueryEvent::Finished(o) = h.blocking_next().expect("stream ended without Finished") {
            return o;
        }
    }
}

/// A separate connection that watches the server's process list.
pub struct Observer {
    conn: Arc<tokio::sync::Mutex<Conn>>,
}

impl Observer {
    pub fn new() -> Self {
        let d = dsn();
        let opts: Opts = OptsBuilder::default()
            .ip_or_hostname(d.host)
            .tcp_port(d.port)
            .user(Some(d.user))
            .pass(Some(d.password))
            .db_name(Some(d.database))
            .into();
        let conn = block_on(async {
            handle()
                .spawn(Conn::new(opts))
                .await
                .unwrap()
                .expect("observer connection")
        });
        Self {
            conn: Arc::new(tokio::sync::Mutex::new(conn)),
        }
    }

    /// Number of statements `id` is running (0 or 1).
    pub fn running(&self, id: u64) -> u64 {
        let conn = self.conn.clone();
        block_on(async move {
            handle()
                .spawn(async move { running_count(&mut *conn.lock().await, id).await })
                .await
                .unwrap()
        })
    }

    pub fn wait_running(&self, id: u64) {
        let start = Instant::now();
        while self.running(id) == 0 {
            assert!(
                start.elapsed() < lim(Duration::from_secs(5)),
                "session {id} never started its statement"
            );
        }
    }

    /// Starts polling now; the task returns the instant the statement was
    /// first seen gone (`None` after 5 s).
    pub fn watch_gone(&self, id: u64) -> tokio::task::JoinHandle<Option<Instant>> {
        let conn = self.conn.clone();
        handle().spawn(async move {
            let mut conn = conn.lock().await;
            let start = Instant::now();
            while start.elapsed() < lim(Duration::from_secs(5)) {
                if running_count(&mut conn, id).await == 0 {
                    return Some(Instant::now());
                }
            }
            None
        })
    }

    /// Whether any session with this id is left (killed sessions vanish).
    pub fn session_exists(&self, id: u64) -> bool {
        let conn = self.conn.clone();
        block_on(async move {
            handle()
                .spawn(async move {
                    let n: Option<u64> = conn
                        .lock()
                        .await
                        .query_first(format!(
                            "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = {id}"
                        ))
                        .await
                        .unwrap();
                    n.unwrap_or(0) > 0
                })
                .await
                .unwrap()
        })
    }
}

async fn running_count(conn: &mut Conn, id: u64) -> u64 {
    let n: Option<u64> = conn
        .query_first(format!("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = {id} AND COMMAND <> 'Sleep'"))
        .await
        .expect("observer query");
    n.unwrap_or(0)
}

pub fn join_gone(task: tokio::task::JoinHandle<Option<Instant>>) -> Instant {
    block_on(task)
        .expect("observer task")
        .expect("statement was still running on the server 5 s after the stop")
}

pub fn working_set_bytes() -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: plain FFI call with a correctly sized, zeroed out-struct.
    unsafe {
        let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        assert!(GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) != 0);
        c.WorkingSetSize as u64
    }
}

pub fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Prints `name: p50 / max` over the samples (milliseconds, or `unit`).
pub fn report(test: &str, name: &str, samples: &[f64], unit: &str) -> (f64, f64) {
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = s[s.len() / 2];
    let max = *s.last().unwrap();
    println!(
        "[{test}] {name}: p50 {p50:.1} {unit}, max {max:.1} {unit}  (n={}, samples: {})",
        s.len(),
        fmt(samples)
    );
    (p50, max)
}

fn fmt(samples: &[f64]) -> String {
    samples
        .iter()
        .map(|v| format!("{v:.1}"))
        .collect::<Vec<_>>()
        .join(" ")
}

impl Observer {
    /// The PROCESSLIST query itself is slow on this server (about 15 ms), so
    /// "statement gone" timings have this resolution.
    pub fn print_resolution(&self, test: &str) {
        let samples: Vec<f64> = (0..5)
            .map(|_| {
                let t = Instant::now();
                self.running(0);
                ms(t.elapsed())
            })
            .collect();
        report(
            test,
            "observer: one PROCESSLIST poll (resolution of the 'gone' timings)",
            &samples,
            "ms",
        );
    }
}

/// Multiplier for every timing limit, from `LOCUS_TEST_SLACK` (default 1, never
/// below 1). CI sets it because shared runners are slower and noisier than a
/// developer machine; the tests still check the same behaviour, only the
/// allowed latency grows.
pub fn slack() -> f64 {
    std::env::var("LOCUS_TEST_SLACK")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .map_or(1.0, |v| v.max(1.0))
}

/// `ms` scaled by `slack()`.
pub fn lim_ms(ms: f64) -> f64 {
    ms * slack()
}

/// `d` scaled by `slack()`.
pub fn lim(d: Duration) -> Duration {
    d.mul_f64(slack())
}

/// The masked TSV the `student_real` table was loaded from. `LOCUS_STUDENT_TSV`
/// overrides the default `devenv/fixtures-real/student.tsv` (CI generates a
/// synthetic one with `scripts/ci/gen-fixtures.ps1`).
pub fn student_tsv() -> String {
    let path = std::env::var("LOCUS_STUDENT_TSV").unwrap_or_else(|_| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../devenv/fixtures-real/student.tsv"
        )
        .to_string()
    });
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("read {path}: {e} (masked fixture is not in the repo: run scripts/ci/gen-fixtures.ps1 to make a synthetic one)")
    })
}
