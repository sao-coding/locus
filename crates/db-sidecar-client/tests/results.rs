//! Several result sets and statements without one, through the Go sidecar.
mod common;
use common::*;
use db_core::{Cell, ColumnMeta, Driver, ErrorKind, Outcome, QueryEvent, QueryHandle, ResultSet};
use db_sidecar_client::SidecarDriver;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
enum Shape {
    Set(usize),
    Affected(u64),
}

struct Got {
    order: Vec<Shape>,
    sets: Vec<(Vec<ColumnMeta>, ResultSet)>,
    outcome: Outcome,
}

impl Got {
    fn text(&self, set: usize, row: usize, col: usize) -> String {
        match self.sets[set].1.cell(row, col) {
            Cell::Text(s) => s.to_string(),
            Cell::Null => "<NULL>".into(),
            Cell::Bytes(_) => "<bytes>".into(),
        }
    }
}

/// Collects every event and checks `Started -> (Columns -> Batch* | Affected)* -> Finished`.
async fn collect(h: &mut QueryHandle) -> Got {
    let mut order = vec![];
    let mut sets: Vec<(Vec<ColumnMeta>, ResultSet)> = vec![];
    let mut started = false;
    loop {
        match tokio::time::timeout(Duration::from_secs(60), h.next())
            .await
            .expect("no event in 60 s")
        {
            Some(QueryEvent::Started { .. }) => {
                assert!(!started && order.is_empty(), "Started must come first");
                started = true;
            }
            Some(QueryEvent::Columns(c)) => {
                assert!(started && !c.is_empty());
                order.push(Shape::Set(sets.len()));
                sets.push((c.clone(), ResultSet::new(c)));
            }
            Some(QueryEvent::Affected { rows }) => {
                assert!(started);
                order.push(Shape::Affected(rows));
            }
            Some(QueryEvent::Batch(b)) => {
                assert!(
                    matches!(order.last(), Some(Shape::Set(_))),
                    "Batch outside a result set"
                );
                sets.last_mut().unwrap().1.push(b);
            }
            Some(QueryEvent::Finished(outcome)) => {
                assert!(h.next().await.is_none(), "events after Finished");
                return Got {
                    order,
                    sets,
                    outcome,
                };
            }
            None => panic!("stream ended without Finished"),
        }
    }
}

async fn run(d: &SidecarDriver, sql: &str) -> Got {
    collect(&mut d.query(sql.into(), opts())).await
}

async fn run_ok(d: &SidecarDriver, sql: &str) -> Got {
    let got = run(d, sql).await;
    assert!(
        matches!(got.outcome, Outcome::Done { .. }),
        "{sql}: {:?}",
        got.outcome
    );
    got
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_result_sets_each_send_their_columns() {
    let d = driver().await;
    let got = run_ok(&d, "SELECT 1; SELECT 'a','b'").await;
    assert_eq!(got.order, [Shape::Set(0), Shape::Set(1)]);
    assert!(
        matches!(got.outcome, Outcome::Done { rows: 2 }),
        "{:?}",
        got.outcome
    );
    assert_eq!((got.sets[0].0.len(), got.sets[1].0.len()), (1, 2));
    assert_eq!(got.text(0, 0, 0), "1");
    assert_eq!(
        (got.text(1, 0, 0), got.text(1, 0, 1)),
        ("a".into(), "b".into())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn script_reports_affected_rows_per_statement() {
    let d = driver().await;
    let got = run_ok(
        &d,
        "CREATE TEMPORARY TABLE sc_tmp (a INT); INSERT INTO sc_tmp VALUES (1),(2),(3); \
         UPDATE sc_tmp SET a = a + 10 WHERE a < 3; SELECT a FROM sc_tmp ORDER BY a",
    )
    .await;
    assert_eq!(
        got.order,
        [
            Shape::Affected(0),
            Shape::Affected(3),
            Shape::Affected(2),
            Shape::Set(0)
        ]
    );
    assert!(
        matches!(got.outcome, Outcome::Done { rows: 3 }),
        "{:?}",
        got.outcome
    );
    assert_eq!(
        (got.text(0, 0, 0), got.text(0, 2, 0)),
        ("3".into(), "12".into())
    );
    let got = run_ok(&d, "SELECT 1; DO 0; SELECT 2").await;
    assert_eq!(
        got.order,
        [Shape::Set(0), Shape::Affected(0), Shape::Set(1)]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_with_two_result_sets() {
    let d = driver().await;
    run_ok(&d, "DROP PROCEDURE IF EXISTS bench.dbw_sc_two").await;
    run_ok(
        &d,
        "CREATE PROCEDURE bench.dbw_sc_two() BEGIN SELECT 1 AS x; SELECT 'a' AS y, 'b' AS z; END",
    )
    .await;
    let got = run_ok(&d, "CALL bench.dbw_sc_two()").await;
    assert_eq!(got.order, [Shape::Set(0), Shape::Set(1)]);
    assert_eq!((got.sets[0].0.len(), got.sets[1].0.len()), (1, 2));
    assert_eq!(got.text(1, 0, 1), "b");
    let got = run_ok(&d, "CALL bench.dbw_sc_two(); SELECT 3").await;
    assert_eq!(got.order, [Shape::Set(0), Shape::Set(1), Shape::Set(2)]);
    run_ok(&d, "DROP PROCEDURE bench.dbw_sc_two").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn error_in_second_statement_keeps_the_first_result() {
    let d = driver().await;
    let got = run(
        &d,
        "SELECT 1; SELECT * FROM bench.dbw_no_such_table; SELECT 3",
    )
    .await;
    assert_eq!(got.order, [Shape::Set(0)]);
    assert_eq!(got.text(0, 0, 0), "1");
    match &got.outcome {
        Outcome::Failed(e) => assert_eq!((e.kind, e.code), (ErrorKind::Query, Some(1146))),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(select1_ok(&d).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_during_second_statement() {
    let d = driver().await;
    let mut obs = Obs::new().await;
    for run in 0..3 {
        let mut h = d.query("SELECT 1; SELECT SLEEP(30)".into(), opts());
        let id = started(&mut h).await;
        // The first result set arrives while the second statement runs.
        assert!(matches!(h.next().await, Some(QueryEvent::Columns(c)) if c.len() == 1));
        assert!(matches!(h.next().await, Some(QueryEvent::Batch(b)) if b.rows() == 1));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(obs.running(id).await, "run {run}: SLEEP should be running");
        let t0 = Instant::now();
        h.cancel();
        let (_, outcome) = drain(&mut h).await;
        assert!(matches!(outcome, Outcome::Cancelled), "{outcome:?}");
        let gone = obs
            .gone(id, t0, Duration::from_secs(5))
            .await
            .expect("statement still running on the server");
        println!(
            "[cancel_during_second_statement] run {run}: cancel -> gone from PROCESSLIST {gone:.1} ms"
        );
        assert!(select1_ok(&d).await);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_across_result_sets() {
    let d = driver().await;
    let t = Instant::now();
    let q = db_core::QueryOptions {
        timeout: Some(Duration::from_millis(700)),
        ..Default::default()
    };
    let got = collect(&mut d.query("SELECT 1; SELECT SLEEP(30); SELECT 2".into(), q)).await;
    assert_eq!(got.order, [Shape::Set(0)]);
    assert!(
        matches!(got.outcome, Outcome::Failed(ref e) if e.kind == ErrorKind::Timeout),
        "{:?}",
        got.outcome
    );
    assert!(t.elapsed() < Duration::from_secs(4), "{:?}", t.elapsed());
    assert!(select1_ok(&d).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn back_pressure_across_result_sets_and_origin_is_none() {
    let d = driver().await;
    let got = run_ok(&d, &format!("SELECT 1; {BIG_Q}; SELECT 'end'")).await;
    assert_eq!(got.sets.len(), 3);
    assert_eq!(got.sets[1].1.rows(), BIG_ROWS as usize);
    assert!(
        matches!(got.outcome, Outcome::Done { rows } if rows == BIG_ROWS + 2),
        "{:?}",
        got.outcome
    );
    assert_eq!(got.text(2, 0, 0), "end");
    // go-sql-driver does not expose the origin of a column.
    assert!(got.sets[1].0.iter().all(|c| c.origin.is_none()));
    // Stop reading in the middle of the second set, then cancel.
    let mut obs = Obs::new().await;
    let mut h = d.query(format!("SELECT 1; {BIG_Q}"), opts());
    let id = started(&mut h).await;
    let (mut columns, mut batches) = (0, 0);
    while columns < 2 || batches < 3 {
        match h.next().await {
            Some(QueryEvent::Columns(_)) => columns += 1,
            Some(QueryEvent::Batch(_)) => batches += 1,
            other => panic!("{other:?}"),
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let t0 = Instant::now();
    h.cancel();
    assert!(matches!(drain(&mut h).await.1, Outcome::Cancelled));
    obs.gone(id, t0, Duration::from_secs(5))
        .await
        .expect("statement still running");
    assert!(select1_ok(&d).await);
}
