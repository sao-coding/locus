//! Server-side streaming export against the local MariaDB (`BENCH_DSN`, or
//! the BENCH_DSN of a local MariaDB).
//! One-time setup (root on the local throwaway server): `GRANT ALL ON wbexport.* TO bench@127.0.0.1`.
//! Needs `bench.student_real` (40,450 rows). It builds `wbexport.big`
//! (about 526k rows) and drops the whole `wbexport` schema afterwards.
//!
//! One test function runs everything: it measures the process working set,
//! so no other test may allocate in the same binary meanwhile.

use db_core::{Cell, Driver, Outcome, QueryEvent, QueryOptions};
use locus_core::driver::AnyDriver;
use locus_core::export::{Format, Spec};
use locus_core::export_stream::{Cancel, End, Progress, export_query};
use locus_core::store::Tls;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn dsn() -> String {
    std::env::var("BENCH_DSN").unwrap_or_else(|_| {
        let f = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../devenv/tools/bench.env");
        let text = std::fs::read_to_string(&f)
            .expect("BENCH_DSN is not set (mysql://user:pass@host:port/db of a local MariaDB)");
        text.lines()
            .find_map(|l| l.strip_prefix("BENCH_DSN="))
            .expect("no BENCH_DSN line")
            .trim()
            .to_string()
    })
}

async fn mariadb() -> Arc<AnyDriver> {
    let (mut c, pw) = locus_core::store::parse_dsn(&dsn()).expect("BENCH_DSN");
    c.tls = Tls::Disabled;
    let rt = tokio::runtime::Handle::current();
    Arc::new(
        AnyDriver::connect(&rt, &c, pw, Default::default())
            .await
            .expect("local MariaDB: set BENCH_DSN and start the server"),
    )
}

async fn scalar(d: &dyn Driver, sql: &str) -> String {
    let mut h = d.query(sql.into(), QueryOptions::default());
    let mut v = String::new();
    while let Some(ev) = h.next().await {
        match ev {
            QueryEvent::Batch(b) if b.rows() > 0 => {
                if let Cell::Text(t) = b.cell(0, 0) {
                    v = t.to_string();
                }
            }
            QueryEvent::Finished(Outcome::Failed(e)) => panic!("{sql}: {e:?}"),
            _ => {}
        }
    }
    v
}

async fn exec(d: &dyn Driver, sql: &str) {
    let mut h = d.query(sql.into(), QueryOptions::default());
    while let Some(ev) = h.next().await {
        if let QueryEvent::Finished(Outcome::Failed(e)) = ev {
            panic!("{sql}: {e:?}");
        }
    }
}

fn working_set() -> u64 {
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

const MB: f64 = 1024.0 * 1024.0;

struct Run {
    result: Result<End, String>,
    /// Peak growth of the working set over its start value, MB.
    growth: f64,
    secs: f64,
    progress: Arc<Progress>,
}

/// One export on its own thread while the working set is sampled; with
/// `stop_at_rows` the export is cancelled once that many rows were written.
async fn run(
    d: &Arc<AnyDriver>,
    sql: &str,
    format: Format,
    path: &std::path::Path,
    stop_at_rows: Option<u64>,
) -> Run {
    let session = d.session().await.expect("session");
    let progress = Arc::new(Progress::default());
    let cancel = Arc::new(Cancel::default());
    let base = working_set();
    let done = Arc::new(AtomicBool::new(false));
    let (p2, c2, d2, sql2, path2) = (
        progress.clone(),
        cancel.clone(),
        done.clone(),
        sql.to_string(),
        path.to_path_buf(),
    );
    let t0 = Instant::now();
    let worker = std::thread::spawn(move || {
        let mut spec = Spec::new(vec![]);
        spec.table = "student_copy".into();
        let r = export_query(&*session, sql2, format, spec, &path2, &p2, &c2);
        d2.store(true, Ordering::SeqCst);
        r
    });
    let mut peak = base;
    while !done.load(Ordering::SeqCst) {
        peak = peak.max(working_set());
        if let Some(n) = stop_at_rows
            && progress.rows.load(Ordering::Relaxed) >= n
        {
            cancel.cancel();
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    let result = tokio::task::spawn_blocking(move || worker.join().unwrap())
        .await
        .unwrap();
    Run {
        result,
        growth: peak.saturating_sub(base) as f64 / MB,
        secs: t0.elapsed().as_secs_f64(),
        progress,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streams_500k_rows_flat_and_cancels() {
    let d = mariadb().await;
    exec(&*d, "DROP DATABASE IF EXISTS wbexport").await;
    exec(&*d, "CREATE DATABASE wbexport CHARACTER SET utf8mb4").await;
    let seq = (1..=13)
        .map(|i| format!("SELECT {i} AS n"))
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    exec(&*d, &format!("CREATE TABLE wbexport.big AS SELECT a.*, k.n AS copy_no FROM bench.student_real a CROSS JOIN ({seq}) k")).await;
    let outcome = tokio::spawn(body(d.clone())).await;
    // Always drop the schema, also after a failed assertion.
    exec(&*d, "DROP DATABASE IF EXISTS wbexport").await;
    assert_eq!(
        scalar(
            &*d,
            "SELECT COUNT(*) FROM information_schema.schemata WHERE schema_name='wbexport'"
        )
        .await,
        "0"
    );
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
}

async fn body(d: Arc<AnyDriver>) {
    let d = &d;
    let total: u64 = scalar(&**d, "SELECT COUNT(*) FROM wbexport.big")
        .await
        .parse()
        .unwrap();
    assert!(total >= 500_000, "{total}");
    let dir = std::env::temp_dir().join(format!("wbexport-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sql = "SELECT * FROM wbexport.big";

    // Warm up the allocator and the pool so the baseline is steady.
    let r = run(
        d,
        "SELECT * FROM wbexport.big LIMIT 20000",
        Format::Csv,
        &dir.join("warm.csv"),
        None,
    )
    .await;
    assert_eq!(r.result.unwrap(), End::Done(20_000));

    for (fmt, name, limit_mb) in [
        (Format::Csv, "out.csv", 48.0),
        (Format::Xlsx, "out.xlsx", 96.0),
    ] {
        let path = dir.join(name);
        let r = run(d, sql, fmt, &path, None).await;
        assert_eq!(r.result.unwrap(), End::Done(total), "{name}");
        assert_eq!(r.progress.rows.load(Ordering::Relaxed), total);
        let size = std::fs::metadata(&path).unwrap().len();
        println!(
            "[export_stream] {name}: {total} rows, {:.1} MB file, {:.1} s ({:.0} rows/s), working set grew at most {:.1} MB",
            size as f64 / MB,
            r.secs,
            total as f64 / r.secs,
            r.growth
        );
        assert!(
            r.growth < limit_mb,
            "{name}: working set grew {:.1} MB",
            r.growth
        );
        assert!(size > 10_000_000, "{name}: {size}");
    }
    // The xlsx is a real zip with one sheet holding every row.
    let out = std::process::Command::new("tar")
        .arg("-tf")
        .arg(dir.join("out.xlsx"))
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&out.stdout);
    assert!(
        listing.contains("xl/worksheets/sheet1.xml") && !listing.contains("sheet2.xml"),
        "{listing}"
    );

    // Cancel mid-way: stops the server-side statement and removes the partial file.
    let path = dir.join("cancel.csv");
    let r = run(d, sql, Format::Csv, &path, Some(30_000)).await;
    assert_eq!(r.result.unwrap(), End::Cancelled);
    assert!(!path.exists(), "partial file must be removed");
    let got = r.progress.rows.load(Ordering::Relaxed);
    assert!((30_000..total).contains(&got), "{got}");
    println!(
        "[export_stream] cancel: stopped after {got} rows in {:.1} s",
        r.secs
    );
    let mut gone = false;
    for _ in 0..50 {
        let n: u64 = scalar(&**d, "SELECT COUNT(*) FROM information_schema.processlist WHERE info LIKE 'SELECT * FROM wbexport.big%'").await.parse().unwrap();
        if n == 0 {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        gone,
        "the statement is still running on the server after cancel"
    );

    // Cancel while the server is still working on the first row.
    let path = dir.join("sleep.csv");
    let session = d.session().await.unwrap();
    let cancel = Arc::new(Cancel::default());
    let c2 = cancel.clone();
    let p2 = path.clone();
    let t0 = Instant::now();
    let worker = std::thread::spawn(move || {
        export_query(
            &*session,
            "SELECT SLEEP(30)".into(),
            Format::Csv,
            Spec::new(vec![]),
            &p2,
            &Progress::default(),
            &c2,
        )
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    cancel.cancel();
    // Joining on a runtime worker would park the task the cancel just woke.
    let r = tokio::task::spawn_blocking(move || worker.join().unwrap())
        .await
        .unwrap();
    assert_eq!(r.unwrap(), End::Cancelled);
    assert!(
        t0.elapsed() < slack(Duration::from_secs(10)),
        "cancel took {:?}",
        t0.elapsed()
    );
    assert!(!path.exists());

    // A statement that is not a query fails cleanly and leaves no file.
    let path = dir.join("none.csv");
    let session = d.session().await.unwrap();
    let p3 = path.clone();
    let r = tokio::task::spawn_blocking(move || {
        export_query(
            &*session,
            "SET @x = 1".into(),
            Format::Csv,
            Spec::new(vec![]),
            &p3,
            &Progress::default(),
            &Cancel::default(),
        )
    })
    .await
    .unwrap();
    assert!(r.is_err() && !path.exists(), "{r:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A timing limit scaled by `LOCUS_TEST_SLACK` (default 1; CI raises it
/// because shared runners are slower and noisier).
fn slack(d: std::time::Duration) -> std::time::Duration {
    let k = std::env::var("LOCUS_TEST_SLACK")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .map_or(1.0, |v| v.max(1.0));
    d.mul_f64(k)
}
