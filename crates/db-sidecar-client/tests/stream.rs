mod common;
use common::*;
use db_core::{Cell, Driver, Outcome, QueryEvent, ResultSet, TlsMode};
use db_sidecar_client::SidecarDriver;
use mysql_async::prelude::*;
use std::collections::HashMap;
use std::time::Instant;

const COLS: [&str; 17] = [
    "id",
    "student_no",
    "name",
    "gender",
    "birthday",
    "email",
    "phone",
    "class_id",
    "department",
    "enroll_year",
    "gpa",
    "credits",
    "address",
    "note",
    "profile",
    "created_at",
    "updated_at",
];

/// All of bench.student as text, read through the observer connection.
async fn expected() -> HashMap<String, Vec<Option<String>>> {
    let mut obs = Obs::new().await;
    let select = COLS
        .iter()
        .map(|c| format!("CAST({c} AS CHAR)"))
        .collect::<Vec<_>>()
        .join(",");
    let rows: Vec<mysql_async::Row> = obs
        .0
        .query(format!("SELECT {select} FROM student"))
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        40_450,
        "bench.student must hold the 40,450 synthetic rows"
    );
    rows.into_iter()
        .map(|mut r| {
            let cells: Vec<Option<String>> = (0..COLS.len())
                .map(|i| r.take::<Option<String>, _>(i).unwrap())
                .collect();
            (cells[0].clone().unwrap(), cells)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_stream() {
    let want = expected().await;
    let d = driver().await;
    let info = d.ready_info().unwrap();
    println!(
        "[full_stream] sidecar ready: driver={} server={}",
        info.driver, info.server_version
    );
    assert_eq!(info.driver, "mysql");
    assert_eq!(d.kind(), "sidecar");

    let (mut rates, mut times) = (vec![], vec![]);
    for run in 0..RUNS {
        let t0 = Instant::now();
        let mut h = d.query(BIG_Q.into(), opts());
        // Event order: Started -> Columns -> Batch* -> Finished.
        assert!(
            matches!(h.next().await, Some(QueryEvent::Started { session_id }) if session_id > 0)
        );
        let Some(QueryEvent::Columns(cols)) = h.next().await else {
            panic!("expected Columns second")
        };
        assert_eq!(
            cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            COLS
        );
        assert_eq!(cols[0].nullable, Some(false), "id is NOT NULL");
        assert_eq!(cols[13].nullable, Some(true), "note is nullable");
        assert!(cols.iter().all(|c| !c.type_name.is_empty()));
        if run == 0 {
            println!(
                "[full_stream] columns: {:?}",
                cols.iter()
                    .map(|c| format!("{} {} {:?}", c.name, c.type_name, c.nullable))
                    .collect::<Vec<_>>()
            );
        }
        let mut rs = ResultSet::new(cols);
        let mut rows = 0u64;
        let outcome = loop {
            match h.next().await.expect("stream ended without Finished") {
                QueryEvent::Batch(b) => {
                    assert!(b.rows() > 0 && b.rows() <= 1000);
                    rows += b.rows() as u64;
                    if run == 0 {
                        rs.push(b);
                    }
                }
                QueryEvent::Finished(o) => break o,
                e => panic!("unexpected event {e:?}"),
            }
        };
        let el = t0.elapsed();
        assert!(h.next().await.is_none(), "nothing may follow Finished");
        assert!(
            matches!(outcome, Outcome::Done { rows: n } if n == BIG_ROWS),
            "{outcome:?}"
        );
        assert_eq!(rows, BIG_ROWS);
        rates.push(rows as f64 / el.as_secs_f64());
        times.push(ms(el));

        if run == 0 {
            // Every cell of every row against the table, and each student 10 times.
            assert_eq!(rs.rows() as u64, BIG_ROWS);
            let mut seen: HashMap<&str, u32> = HashMap::new();
            for r in 0..rs.rows() {
                let Cell::Text(id) = rs.cell(r, 0) else {
                    panic!("row {r}: id is not text")
                };
                let exp = &want[id];
                for (c, e) in exp.iter().enumerate() {
                    match (rs.cell(r, c), e) {
                        (Cell::Null, None) => {}
                        (Cell::Text(got), Some(e)) if got == e => {}
                        (got, e) => panic!(
                            "row {r} col {} ({id}): got {got:?}, table has {e:?}",
                            COLS[c]
                        ),
                    }
                }
                *seen.entry(id).or_default() += 1;
            }
            assert_eq!(seen.len(), 40_450);
            assert!(seen.values().all(|n| *n == 10));
            let nulls = (0..rs.rows())
                .filter(|r| rs.cell(*r, 13) == Cell::Null)
                .count();
            println!(
                "[full_stream] checked {} rows x 17 cells against the table; {nulls} NULL notes",
                rs.rows()
            );
            println!(
                "[full_stream] result set holds {:.1} MB in columnar batches",
                rs.bytes() as f64 / 1048576.0
            );
        }
    }
    let r = report("full_stream", "throughput", "rows/s", &rates);
    report("full_stream", "time", "ms", &times);
    println!(
        "[full_stream] POC reference: ~991K rows/s (release builds; debug builds decode slower)"
    );
    assert!(
        r.min > 100_000.0,
        "throughput collapsed: {:.0} rows/s",
        r.min
    );
    assert!(select1_ok(&d).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_and_null_cells_survive() {
    let d = driver().await;
    let mut h = d.query(
        "SELECT '' AS e, NULL AS n, 'x' AS s, CAST(X'FF00FE' AS BINARY(3)) AS b".into(),
        opts(),
    );
    let mut rs = None;
    loop {
        match h.next().await.unwrap() {
            QueryEvent::Columns(c) => rs = Some(ResultSet::new(c)),
            QueryEvent::Batch(b) => rs.as_mut().unwrap().push(b),
            QueryEvent::Finished(o) => {
                assert!(matches!(o, Outcome::Done { rows: 1 }), "{o:?}");
                break;
            }
            _ => {}
        }
    }
    let rs = rs.unwrap();
    assert_eq!(rs.cell(0, 0), Cell::Text(""));
    assert_eq!(rs.cell(0, 1), Cell::Null);
    assert_eq!(rs.cell(0, 2), Cell::Text("x"));
    assert_eq!(rs.cell(0, 3), Cell::Bytes(&[0xff, 0x00, 0xfe]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_error_is_finished_failed() {
    let d = driver().await;
    let mut h = d.query("SELEC nonsense".into(), opts());
    let (_, o) = drain(&mut h).await;
    let Outcome::Failed(e) = o else {
        panic!("expected Failed, got {o:?}")
    };
    assert_eq!(e.kind, db_core::ErrorKind::Query);
    assert_eq!(e.code, Some(1064), "{e}");
    assert!(
        select1_ok(&d).await,
        "the connection must stay usable after an SQL error"
    );
}

/// The futures and handles work without a tokio context: spawn on a runtime
/// handle, drive everything from a plain thread executor.
#[test]
fn awaitable_from_a_non_tokio_executor() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let d: SidecarDriver = block_on(SidecarDriver::spawn(
        rt.handle(),
        sidecar_exe(),
        connect_options(TlsMode::Disabled),
    ))
    .expect("spawn");
    let mut h = d.query("SELECT 1, 'two'".into(), opts());
    let mut got = None;
    while let Some(e) = block_on(h.next()) {
        match e {
            QueryEvent::Batch(b) => {
                got = Some((b.cell(0, 0).clone_text(), b.cell(0, 1).clone_text()))
            }
            QueryEvent::Finished(o) => assert!(matches!(o, Outcome::Done { rows: 1 }), "{o:?}"),
            _ => {}
        }
    }
    assert_eq!(got, Some(("1".into(), "two".into())));
    let report = block_on(d.shutdown()).expect("a running sidecar");
    assert!(!report.forced);
}

trait CloneText {
    fn clone_text(&self) -> String;
}
impl CloneText for Cell<'_> {
    fn clone_text(&self) -> String {
        match self {
            Cell::Text(s) => s.to_string(),
            other => format!("{other:?}"),
        }
    }
}
