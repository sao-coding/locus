//! Oracle through the sidecar. Ignored by default and gated by environment:
//! `cargo test -p db-sidecar-client --test oracle_live -- --ignored`
//!
//! READ ONLY: only SELECT statements and data-dictionary reads, every one with a row
//! cap (<= 1000) and filtered by one owner. Nothing is written, no PL/SQL,
//! no admin statements. The password comes from the environment and is never
//! printed.
//!
//! ORACLE_TEST_HOST, ORACLE_TEST_PORT, ORACLE_TEST_SERVICE, ORACLE_TEST_USER,
//! ORACLE_TEST_PASSWORD; optional ORACLE_TEST_TLS = require | require-unverified
//! | disabled (default disabled: plain TCP), ORACLE_TEST_OWNER (default: the
//! user).
mod common;
use common::*;
use db_core::{ConnectOptions, Driver, Outcome, QueryEvent, TlsMode};
use db_sidecar_client::SidecarDriver;
use std::time::Duration;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

async fn oracle() -> SidecarDriver {
    let tls = match std::env::var("ORACLE_TEST_TLS").as_deref() {
        Ok("require") => TlsMode::Require,
        Ok("require-unverified") => TlsMode::RequireUnverified,
        _ => TlsMode::Disabled,
    };
    let opts = ConnectOptions {
        host: env("ORACLE_TEST_HOST"),
        port: env("ORACLE_TEST_PORT").parse().expect("ORACLE_TEST_PORT"),
        user: env("ORACLE_TEST_USER"),
        password: env("ORACLE_TEST_PASSWORD"),
        database: Some(env("ORACLE_TEST_SERVICE")),
        tls,
    };
    let d = SidecarDriver::spawn_driver(
        &tokio::runtime::Handle::current(),
        sidecar_exe(),
        "oracle",
        opts,
    )
    .await
    .unwrap_or_else(|e| panic!("sidecar did not connect: {:?} {}", e.kind, e.message));
    d.set_catalog_row_cap(1000);
    d
}

#[tokio::test]
#[ignore = "needs a live Oracle server (ORACLE_TEST_* environment)"]
async fn oracle_select_catalog_and_cancel() {
    let d = oracle().await;
    println!("server: {:?}", d.ready_info().map(|i| i.server_version));

    // Types and NULL, one row.
    let mut h = d.query(
        "SELECT 1 AS N, 'a' AS S, SYSDATE AS D, HEXTORAW('FF00') AS R, CAST(NULL AS VARCHAR2(1)) AS X FROM DUAL WHERE ROWNUM <= 1".into(),
        opts(),
    );
    let sid = started(&mut h).await;
    assert_ne!(sid, 0);
    let (rows, o) = drain(&mut h).await;
    assert!(matches!(o, Outcome::Done { rows: 1 }) && rows == 1, "{o:?}");

    // Catalog of one owner, capped at 1000 rows per read.
    let owner = std::env::var("ORACLE_TEST_OWNER")
        .unwrap_or_else(|_| env("ORACLE_TEST_USER").to_uppercase());
    let cat = d.catalog().expect("oracle catalog");
    let cur = cat.current_schema().await.unwrap();
    println!("current schema: {cur:?}");
    let schemas = cat.schemas().await.unwrap();
    assert!(!schemas.is_empty());
    let objs = cat.objects(&owner).await.unwrap();
    println!("{owner}: {} tables/views (capped at 1000)", objs.len());
    if let Some(o) = objs.first() {
        let cols = cat.columns(&owner, &o.name).await.unwrap();
        println!(
            "{}.{}: {} columns, e.g. {:?}",
            owner,
            o.name,
            cols.len(),
            cols.first()
        );
        assert!(!cols.is_empty());
    }
    cat.foreign_keys(&owner).await.unwrap();
    assert_eq!(d.quote_identifier("a\"b"), "\"a\"\"b\"");

    // Cancel: a statement that reads no data (DUAL, row cap) with the cancel
    // frame sent right away. Ends cancelled or, if it was faster, done.
    let mut h = d.query("SELECT 1 FROM DUAL WHERE ROWNUM <= 1".into(), opts());
    started(&mut h).await;
    h.cancel();
    let (_, o) = tokio::time::timeout(Duration::from_secs(10), drain(&mut h))
        .await
        .expect("cancel did not end the query");
    assert!(
        matches!(o, Outcome::Cancelled | Outcome::Done { .. }),
        "{o:?}"
    );
    let mut h2 = d.query("SELECT 1 FROM DUAL".into(), opts());
    while let Some(ev) = h2.next().await {
        if matches!(ev, QueryEvent::Finished(_)) {
            break;
        }
    }
    d.shutdown().await;
}
