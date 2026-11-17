//! Shared helpers for the sidecar integration tests. Local MariaDB only.
#![allow(dead_code)]

use db_core::{ConnectOptions, Driver, Outcome, QueryEvent, QueryHandle, QueryOptions, TlsMode};
use db_sidecar_client::SidecarDriver;
use mysql_async::{Conn, Opts, prelude::*};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

pub const BIG_Q: &str = "SELECT s.* FROM student s CROSS JOIN (SELECT 1 n UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9 UNION ALL SELECT 10) t";
pub const BIG_ROWS: u64 = 404_500;
pub const RUNS: usize = 5;

pub fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// BENCH_DSN from the environment; falls back to a local bench.env so the
/// suite also runs from a plain shell. Never printed.
pub fn dsn() -> String {
    if let Ok(v) = std::env::var("BENCH_DSN") {
        return v;
    }
    let env_file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../devenv/tools/bench.env");
    let text = std::fs::read_to_string(&env_file)
        .expect("BENCH_DSN is not set (mysql://user:pass@host:port/db of a local MariaDB)");
    text.lines()
        .find_map(|l| l.strip_prefix("BENCH_DSN="))
        .expect("no BENCH_DSN line in the local bench.env")
        .trim()
        .to_string()
}

/// mysql://user:pass@host:port/db -> options. Refuses anything but loopback.
pub fn connect_options(tls: TlsMode) -> ConnectOptions {
    let dsn = dsn();
    let rest = dsn
        .strip_prefix("mysql://")
        .expect("BENCH_DSN must be mysql://user:pass@host:port/db");
    let (cred, addr) = rest.rsplit_once('@').expect("BENCH_DSN has no @");
    let (user, password) = cred.split_once(':').expect("BENCH_DSN has no password");
    let (hostport, db) = addr.split_once('/').expect("BENCH_DSN has no database");
    let (host, port) = hostport.rsplit_once(':').expect("BENCH_DSN has no port");
    assert!(
        host == "127.0.0.1" || host == "localhost",
        "these tests only run against the local throwaway MariaDB, not {host}"
    );
    ConnectOptions {
        host: host.into(),
        port: port.parse().unwrap(),
        user: user.into(),
        password: password.into(),
        database: Some(db.into()),
        tls,
    }
}

pub fn sidecar_exe() -> PathBuf {
    std::env::var_os("SIDECAR_EXE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sidecar/bin/sidecar.exe")
        })
}

pub async fn driver() -> SidecarDriver {
    driver_with(connect_options(TlsMode::Disabled)).await
}

pub async fn driver_with(opts: ConnectOptions) -> SidecarDriver {
    let exe = sidecar_exe();
    assert!(
        exe.exists(),
        "{} is missing: run sidecar/build.ps1",
        exe.display()
    );
    SidecarDriver::spawn(&tokio::runtime::Handle::current(), exe, opts)
        .await
        .unwrap_or_else(|e| panic!("sidecar did not start: {e}"))
}

pub fn opts() -> QueryOptions {
    QueryOptions::default()
}

// ---- observer connection (mysql_async) ----

pub struct Obs(pub Conn);

impl Obs {
    pub async fn new() -> Obs {
        Obs(Conn::new(Opts::from_url(&dsn()).unwrap()).await.unwrap())
    }

    pub async fn running(&mut self, id: u64) -> bool {
        let n: Option<u64> =
            self.0.exec_first("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID=? AND COMMAND='Query'", (id,)).await.unwrap();
        n.unwrap_or(0) > 0
    }

    /// Milliseconds from `t0` until the statement is gone, or `None` if it is
    /// still running at `limit`. A leftover statement is killed.
    pub async fn gone(&mut self, id: u64, t0: Instant, limit: Duration) -> Option<f64> {
        while t0.elapsed() < limit {
            if !self.running(id).await {
                return Some(ms(t0.elapsed()));
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let _ = self.0.query_drop(format!("KILL {id}")).await;
        None
    }
}

// ---- measurements ----

pub struct Stats {
    pub p50: f64,
    pub max: f64,
    pub min: f64,
}

pub fn stats(v: &[f64]) -> Stats {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Stats {
        p50: s[(0.5 * (s.len() - 1) as f64 + 0.5) as usize],
        max: s[s.len() - 1],
        min: s[0],
    }
}

/// Prints `[test] metric: p50 .. / max .. unit (n=..)` and returns the stats.
pub fn report(test: &str, metric: &str, unit: &str, v: &[f64]) -> Stats {
    let s = stats(v);
    println!(
        "[{test}] {metric}: p50 {:.1} / max {:.1} / min {:.1} {unit} (n={}) runs={:?}",
        s.p50,
        s.max,
        s.min,
        v.len(),
        v.iter()
            .map(|x| (x * 10.0).round() / 10.0)
            .collect::<Vec<_>>()
    );
    s
}

// ---- process helpers ----

pub fn working_set_mb(pid: u32) -> f64 {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::{ProcessStatus::*, Threading::*},
    };
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid);
        assert!(!h.is_null(), "cannot open process {pid}");
        let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        GetProcessMemoryInfo(h, &mut c, c.cb);
        CloseHandle(h);
        c.WorkingSetSize as f64 / 1048576.0
    }
}

pub fn alive(pid: u32) -> bool {
    use windows_sys::Win32::{Foundation::*, System::Threading::*};
    unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if h.is_null() {
            return false;
        }
        let r = WaitForSingleObject(h, 0);
        CloseHandle(h);
        r == WAIT_TIMEOUT
    }
}

pub fn terminate(pid: u32) {
    use windows_sys::Win32::{Foundation::CloseHandle, System::Threading::*};
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        assert!(!h.is_null(), "cannot open process {pid} for termination");
        TerminateProcess(h, 1);
        CloseHandle(h);
    }
}

// ---- query helpers ----

/// Reads events until Started and returns the session id (Columns may not
/// come for a statement that is still running).
pub async fn started(h: &mut QueryHandle) -> u64 {
    match tokio::time::timeout(Duration::from_secs(10), h.next())
        .await
        .expect("no event in 10 s")
    {
        Some(QueryEvent::Started { session_id }) => session_id,
        other => panic!("expected Started, got {other:?}"),
    }
}

/// Drains to the terminal event, consuming batches. Returns (rows seen, outcome).
pub async fn drain(h: &mut QueryHandle) -> (u64, Outcome) {
    let mut rows = 0;
    loop {
        match tokio::time::timeout(Duration::from_secs(60), h.next())
            .await
            .expect("no event in 60 s")
        {
            Some(QueryEvent::Batch(b)) => rows += b.rows() as u64,
            Some(QueryEvent::Finished(o)) => return (rows, o),
            Some(_) => {}
            None => panic!("stream ended without Finished"),
        }
    }
}

pub async fn select1_ok(d: &SidecarDriver) -> bool {
    let mut h = d.query("SELECT 1".into(), opts());
    matches!(drain(&mut h).await.1, Outcome::Done { rows: 1 })
}

// ---- a minimal executor that is not tokio ----

struct ThreadWaker(std::thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

pub fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park();
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
