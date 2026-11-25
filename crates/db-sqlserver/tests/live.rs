//! Against a real SQL Server: `cargo test -p db-sqlserver -- --ignored` with
//! MSSQL_TEST_HOST / PORT / USER / PASSWORD (and optional MSSQL_TEST_DATABASE) set.
//! Read only apart from a #temp table.

use db_core::{Cell, ConnectOptions, Driver, Outcome, Param, QueryEvent, QueryOptions, TlsMode};
use db_sqlserver::MssqlDriver;

fn opts() -> ConnectOptions {
    let env = |n: &str| std::env::var(n).unwrap_or_else(|_| panic!("{n} is not set"));
    ConnectOptions {
        host: env("MSSQL_TEST_HOST"),
        port: std::env::var("MSSQL_TEST_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(1433),
        user: env("MSSQL_TEST_USER"),
        password: env("MSSQL_TEST_PASSWORD"),
        database: std::env::var("MSSQL_TEST_DATABASE").ok(),
        tls: TlsMode::RequireUnverified,
    }
}

async fn run(d: &dyn Driver, sql: &str, params: Option<Vec<Param>>) -> (Vec<String>, Vec<u64>) {
    let mut h = match params {
        Some(p) => d.execute(sql.into(), p, QueryOptions::default()),
        None => d.query(sql.into(), QueryOptions::default()),
    };
    let (mut cells, mut affected) = (Vec::new(), Vec::new());
    while let Some(ev) = h.next().await {
        match ev {
            QueryEvent::Batch(b) => {
                for r in 0..b.rows() {
                    if let Cell::Text(t) = b.cell(r, 0) {
                        cells.push(t.to_string());
                    }
                }
            }
            QueryEvent::Affected { rows } => affected.push(rows),
            QueryEvent::Finished(Outcome::Failed(e)) => panic!("{}", e.message),
            QueryEvent::Finished(_) => break,
            _ => {}
        }
    }
    (cells, affected)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live SQL Server"]
async fn scripts_params_and_session() {
    let rt = tokio::runtime::Handle::current();
    let d = MssqlDriver::connect(&rt, opts()).await.unwrap();
    let (c, _) = run(
        &d,
        "SELECT 1 AS a\nGO\nSELECT 'x' UNION ALL SELECT 'y'",
        None,
    )
    .await;
    assert_eq!(c, ["1", "x", "y"]);
    let (c, _) = run(&d, "SELECT ? + N'!'", Some(vec![Param::Text("hi".into())])).await;
    assert_eq!(c, ["hi!"]);
    let s = d.session().await.unwrap();
    let (_, a) = run(
        &s,
        "CREATE TABLE #t (v int); INSERT INTO #t VALUES (1), (2)",
        None,
    )
    .await;
    assert_eq!(a, [2]);
    let (c, _) = run(&s, "SELECT COUNT(*) FROM #t", None).await;
    assert_eq!(c, ["2"]);
    let cat = d.catalog().unwrap();
    assert!(cat.current_schema().await.unwrap().is_some());
    assert!(!cat.schemas().await.unwrap().is_empty());
}
