//! SQL Server through the sidecar. Ignored by default and gated by environment:
//! `cargo test -p db-sidecar-client --test mssql_live -- --ignored`
//!
//! Writes only to a `#temp` table of its own session. The password comes from
//! the environment and is never printed.
//!
//! MSSQL_TEST_HOST, MSSQL_TEST_USER, MSSQL_TEST_PASSWORD; optional
//! MSSQL_TEST_PORT (1433), MSSQL_TEST_DATABASE, MSSQL_TEST_TLS = require |
//! require-unverified (default) | disabled.
mod common;
use common::*;
use db_core::{Cell, ConnectOptions, Driver, Outcome, Param, QueryEvent, QueryHandle, TlsMode};
use db_sidecar_client::SidecarDriver;
use std::time::Duration;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

async fn mssql() -> SidecarDriver {
    let tls = match std::env::var("MSSQL_TEST_TLS").as_deref() {
        Ok("require") => TlsMode::Require,
        Ok("disabled") => TlsMode::Disabled,
        _ => TlsMode::RequireUnverified,
    };
    let opts = ConnectOptions {
        host: env("MSSQL_TEST_HOST"),
        port: std::env::var("MSSQL_TEST_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(1433),
        user: env("MSSQL_TEST_USER"),
        password: env("MSSQL_TEST_PASSWORD"),
        database: std::env::var("MSSQL_TEST_DATABASE").ok(),
        tls,
    };
    SidecarDriver::spawn_driver(
        &tokio::runtime::Handle::current(),
        sidecar_exe(),
        "mssql",
        opts,
    )
    .await
    .unwrap_or_else(|e| panic!("sidecar did not connect: {:?} {}", e.kind, e.message))
}

/// First column of every row, and the affected counts, until Finished.
async fn collect(mut h: QueryHandle) -> (Vec<String>, Vec<u64>, Outcome) {
    let (mut cells, mut affected) = (Vec::new(), Vec::new());
    loop {
        match tokio::time::timeout(Duration::from_secs(60), h.next())
            .await
            .expect("no event in 60 s")
        {
            Some(QueryEvent::Batch(b)) => {
                for r in 0..b.rows() {
                    cells.push(match b.cell(r, 0) {
                        Cell::Null => "NULL".into(),
                        Cell::Text(t) => t.to_string(),
                        Cell::Bytes(x) => format!("{x:?}"),
                    });
                }
            }
            Some(QueryEvent::Affected { rows }) => affected.push(rows),
            Some(QueryEvent::Finished(o)) => return (cells, affected, o),
            Some(_) => {}
            None => panic!("stream ended without Finished"),
        }
    }
}

#[tokio::test]
#[ignore = "needs a live SQL Server (MSSQL_TEST_* environment)"]
async fn mssql_scripts_params_catalog() {
    let d = mssql().await;
    println!("server: {:?}", d.ready_info().map(|i| i.server_version));

    let (c, _, o) = collect(d.query(
        "SELECT 1 AS a\nGO\nSELECT N'x' UNION ALL SELECT NULL\nGO\nSELECT CAST('2024-05-06T07:08:09.5' AS datetime2(1)), NEWID()".into(),
        opts(),
    ))
    .await;
    assert!(matches!(o, Outcome::Done { .. }), "{o:?}");
    assert_eq!(&c[..3], ["1", "x", "NULL"]);
    assert_eq!(c[3], "2024-05-06T07:08:09.5");

    let (c, _, _) = collect(d.execute(
        "SELECT ? + N'!'".into(),
        vec![Param::Text("hi".into())],
        opts(),
    ))
    .await;
    assert_eq!(c, ["hi!"]);

    let cat = d.catalog().expect("mssql catalog");
    let cur = cat.current_schema().await.unwrap();
    assert!(cur.is_some());
    let schemas = cat.schemas().await.unwrap();
    assert!(!schemas.is_empty());
    let objs = cat.objects(cur.as_deref().unwrap()).await.unwrap();
    println!("{cur:?}: {} tables/views", objs.len());
}

/// Cancel inside a transaction stops only the statement: the transaction and
/// the session's #temp table are still there afterwards.
#[tokio::test]
#[ignore = "needs a live SQL Server (MSSQL_TEST_* environment)"]
async fn mssql_cancel_keeps_the_transaction() {
    let d = mssql().await;
    let s = d.session().await.unwrap();
    let (_, a, o) = collect(s.query(
        "CREATE TABLE #t (v int); BEGIN TRANSACTION; INSERT INTO #t VALUES (1), (2)".into(),
        opts(),
    ))
    .await;
    assert!(matches!(o, Outcome::Done { .. }), "{o:?}");
    assert_eq!(a, [2]);

    let mut h = s.query("WAITFOR DELAY '00:00:30'".into(), opts());
    started(&mut h).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let t0 = std::time::Instant::now();
    h.cancel();
    let (_, o) = drain(&mut h).await;
    assert!(matches!(o, Outcome::Cancelled), "{o:?}");
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "cancel took {:?}",
        t0.elapsed()
    );

    let (c, _, _) =
        collect(s.query("SELECT @@TRANCOUNT; SELECT COUNT(*) FROM #t".into(), opts())).await;
    assert_eq!(c, ["1", "2"], "transaction or temp table lost after cancel");
    let (_, _, o) = collect(s.query("ROLLBACK".into(), opts())).await;
    assert!(matches!(o, Outcome::Done { .. }), "{o:?}");
}
