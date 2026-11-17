//! Back-pressure memory check. It measures the whole process's working set,
//! so it lives in its own test binary: another test streaming a big result
//! in the same process would grow the number and fail it.

mod common;

use common::*;
use db_core::Driver;
use db_core::{Cell, Outcome, QueryEvent, QueryOptions};
use db_mysql::MysqlDriver;
use std::time::{Duration, Instant};

fn start(driver: &MysqlDriver, sql: &str, opts: QueryOptions) -> (db_core::QueryHandle, u64) {
    let mut h = driver.query(sql.into(), opts);
    match h.blocking_next() {
        Some(QueryEvent::Started { session_id }) => (h, session_id),
        other => panic!("expected Started, got {other:?}"),
    }
}

fn read_batches(h: &mut db_core::QueryHandle, n: usize) {
    let mut seen = 0;
    while seen < n {
        match h.blocking_next() {
            Some(QueryEvent::Columns(_)) => {}
            Some(QueryEvent::Batch(_)) => seen += 1,
            other => panic!("expected data, got {other:?}"),
        }
    }
}

fn next_query_ms(driver: &MysqlDriver) -> f64 {
    let t = Instant::now();
    let mut h = driver.query("SELECT 1".into(), QueryOptions::default());
    let got = collect(&mut h);
    let elapsed = ms(t.elapsed());
    assert!(
        matches!(got.outcome, Outcome::Done { rows: 1 }),
        "{:?}",
        got.outcome
    );
    assert_eq!(got.rows.cell(0, 0), Cell::Text("1"));
    elapsed
}

#[test]
fn back_pressure_keeps_memory_flat() {
    let test = "back_pressure";
    let driver = connect();
    let obs = Observer::new();
    obs.print_resolution(test);
    // Warm up allocator and pool so the baseline is steady.
    let _ = next_query_ms(&driver);
    let (mut h, id) = start(&driver, BIG_QUERY, QueryOptions::default());
    read_batches(&mut h, 1);

    // Stop reading. Give the pipeline half a second to fill its window.
    std::thread::sleep(Duration::from_millis(500));
    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
    let at_pause = working_set_bytes();
    let mut peak = at_pause;
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(250));
        peak = peak.max(working_set_bytes());
    }
    let after_3s = working_set_bytes();
    println!(
        "[{test}] working set: at pause {:.1} MB, peak during the 2.5 s pause {:.1} MB, after {:.1} MB (growth {:+.2} MB)",
        mb(at_pause),
        mb(peak),
        mb(after_3s),
        mb(peak) - mb(at_pause)
    );
    assert_eq!(
        obs.running(id),
        1,
        "the statement must still be blocked on the server, not drained"
    );
    assert!(
        peak.saturating_sub(at_pause) < 4 * 1024 * 1024,
        "memory grew {:.1} MB while nobody was reading",
        mb(peak - at_pause)
    );

    // Reading resumes where it stopped, then cancel.
    read_batches(&mut h, 2);
    let watcher = obs.watch_gone(id);
    let t0 = Instant::now();
    h.cancel();
    let outcome = finish(&mut h);
    assert!(matches!(outcome, Outcome::Cancelled), "{outcome:?}");
    println!(
        "[{test}] cancel after the pause: Finished(Cancelled) in {:.1} ms",
        ms(t0.elapsed())
    );
    join_gone(watcher);
}
