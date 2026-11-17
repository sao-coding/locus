//! Oracle stored code through the sidecar: create, compile (valid and
//! invalid), read ALL_SOURCE / ALL_ERRORS / ALL_OBJECTS, then drop and check
//! nothing is left. Ignored by default:
//! `cargo test -p db-sidecar-client --test routines_live -- --ignored`
//!
//! WRITES, but only on its own throwaway package `WB_TEST_PKG` in the
//! connected user's schema: it refuses to run when an object of that name
//! already exists, and always drops it at the end (also after a failure),
//! then asserts `ALL_OBJECTS` has no row for it. Same ORACLE_TEST_*
//! environment as `oracle_live.rs`.
mod common;
use common::*;
use db_core::dictionary::Row;
use db_core::routine::{self, Part};
use db_core::{Cell, ConnectOptions, DbObjectKind, Driver, Outcome, Param, QueryEvent, TlsMode};
use db_sidecar_client::SidecarDriver;

const NAME: &str = "WB_TEST_PKG";

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

async fn oracle() -> SidecarDriver {
    let opts = ConnectOptions {
        host: env("ORACLE_TEST_HOST"),
        port: env("ORACLE_TEST_PORT").parse().expect("ORACLE_TEST_PORT"),
        user: env("ORACLE_TEST_USER"),
        password: env("ORACLE_TEST_PASSWORD"),
        database: Some(env("ORACLE_TEST_SERVICE")),
        tls: TlsMode::Disabled,
    };
    SidecarDriver::spawn_driver(
        &tokio::runtime::Handle::current(),
        sidecar_exe(),
        "oracle",
        opts,
    )
    .await
    .unwrap_or_else(|e| panic!("sidecar did not connect: {:?} {}", e.kind, e.message))
}

async fn rows(d: &SidecarDriver, sql: &str, params: Vec<Param>) -> Vec<Row> {
    let mut h = d.execute(sql.into(), params, opts());
    let (mut cols, mut out) = (0, Vec::new());
    while let Some(ev) = h.next().await {
        match ev {
            QueryEvent::Columns(c) => cols = c.len(),
            QueryEvent::Batch(b) => {
                for r in 0..b.rows() {
                    out.push(
                        (0..cols)
                            .map(|c| match b.cell(r, c) {
                                Cell::Null => None,
                                Cell::Text(s) => Some(s.to_string()),
                                Cell::Bytes(x) => Some(String::from_utf8_lossy(x).into_owned()),
                            })
                            .collect(),
                    );
                }
            }
            QueryEvent::Finished(Outcome::Failed(e)) => panic!("{sql}: {}", e.message),
            QueryEvent::Finished(_) => break,
            _ => {}
        }
    }
    out
}

/// Runs DDL; `Err(message)` on failure.
async fn ddl(d: &SidecarDriver, sql: &str) -> Result<(), String> {
    let mut h = d.query(sql.into(), opts());
    let (_, o) = drain(&mut h).await;
    match o {
        Outcome::Failed(e) => Err(e.message),
        _ => Ok(()),
    }
}

async fn objects_left(d: &SidecarDriver, owner: &str) -> usize {
    rows(
        d,
        "SELECT OBJECT_TYPE FROM ALL_OBJECTS WHERE OWNER = ? AND OBJECT_NAME = ? AND ROWNUM <= 10",
        vec![Param::Text(owner.into()), Param::Text(NAME.into())],
    )
    .await
    .len()
}

#[tokio::test]
#[ignore = "needs a live Oracle server (ORACLE_TEST_* environment); writes WB_TEST_PKG and drops it"]
async fn oracle_package_compile_errors_and_cleanup() {
    let d = oracle().await;
    let owner = rows(&d, "SELECT USER FROM DUAL", vec![]).await[0][0]
        .clone()
        .expect("user");
    assert_eq!(
        objects_left(&d, &owner).await,
        0,
        "{owner}.{NAME} already exists: not touching it"
    );

    let result = tokio::spawn({
        let owner = owner.clone();
        async move {
            let d = oracle().await;
            let spec = "CREATE OR REPLACE PACKAGE WB_TEST_PKG AS\n  FUNCTION two RETURN NUMBER;\nEND WB_TEST_PKG;\n/\n";
            let body_ok = "CREATE OR REPLACE PACKAGE BODY WB_TEST_PKG AS\n  FUNCTION two RETURN NUMBER IS\n  BEGIN\n    RETURN 2;\n  END;\nEND WB_TEST_PKG;\n/";
            let body_bad = "CREATE OR REPLACE PACKAGE BODY WB_TEST_PKG AS\n  FUNCTION two RETURN NUMBER IS\n  BEGIN\n    RETURN no_such_thing;\n  END;\nEND WB_TEST_PKG;";
            // The save path: one statement per part, `/` removed, END; kept.
            for text in [spec, body_ok] {
                let plan = routine::save_plan(db_core::dictionary::Dialect::Oracle, false, &owner, DbObjectKind::Package, NAME, text);
                assert_eq!(plan.statements.len(), 1);
                ddl(&d, &plan.statements[0]).await.expect("valid version compiles");
            }
            let st = routine::oracle_status_stmt(&owner, DbObjectKind::Package, NAME).unwrap();
            let status = rows(&d, &st.sql, st.params).await;
            assert_eq!(status.len(), 2, "{status:?}");
            assert!(status.iter().all(|r| r[1].as_deref() == Some("VALID")), "{status:?}");

            // Source read back as an editable statement.
            let s = routine::oracle_source_stmt(&owner, DbObjectKind::Package, NAME, Part::Body).unwrap();
            let src = routine::oracle_source_text(&rows(&d, &s.sql, s.params).await, &owner);
            assert!(src.starts_with(&format!("CREATE OR REPLACE PACKAGE BODY \"{owner}\".WB_TEST_PKG AS")), "{src}");
            // ...which compiles as is (session schema = owner here, qualified anyway).
            ddl(&d, &routine::save_plan(db_core::dictionary::Dialect::Oracle, false, &owner, DbObjectKind::Package, NAME, &src).statements[0]).await.expect("round trip compiles");

            // Invalid version: stored with errors, ALL_ERRORS says where.
            let plan = routine::save_plan(db_core::dictionary::Dialect::Oracle, false, &owner, DbObjectKind::Package, NAME, body_bad);
            let err = ddl(&d, &plan.statements[0]).await.expect_err("invalid body must report an error");
            assert!(routine::is_compiled_with_errors(&err), "{err}");
            let e = routine::oracle_errors_stmt(&owner, DbObjectKind::Package, NAME, Part::Body).unwrap();
            let errors = routine::parse_errors(&rows(&d, &e.sql, e.params).await);
            println!("errors: {errors:?}");
            assert!(errors.iter().any(|e| e.line == 4 && !e.warning), "{errors:?}");
            let status = rows(&d, &st_sql(&owner).0, st_sql(&owner).1).await;
            assert!(status.iter().any(|r| r[0].as_deref() == Some("PACKAGE BODY") && r[1].as_deref() == Some("INVALID")), "{status:?}");
        }
    })
    .await;

    // Always clean up, then check nothing is left.
    let _ = ddl(&d, "DROP PACKAGE WB_TEST_PKG").await;
    assert_eq!(
        objects_left(&d, &owner).await,
        0,
        "WB_TEST_PKG still exists after DROP"
    );
    println!("cleanup: {owner}.{NAME} dropped, ALL_OBJECTS has no row for it");
    if let Err(e) = result {
        std::panic::resume_unwind(e.into_panic());
    }
}

fn st_sql(owner: &str) -> (String, Vec<Param>) {
    let s = routine::oracle_status_stmt(owner, DbObjectKind::Package, NAME).unwrap();
    (s.sql, s.params)
}
