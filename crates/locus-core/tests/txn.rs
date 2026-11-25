//! Manual transactions on a pinned session (`locus_core::txn`) against
//! SQLite (temp file, always runs) and the local MariaDB (`BENCH_DSN`)
//! BENCH_DSN of a local MariaDB).
//! Every MariaDB test uses its own `wb_test_txn_*` table and drops it.

use db_core::{Cell, Driver, Outcome, Param, QueryEvent, QueryOptions};
use locus_core::apply::{Statement, apply_in_tx};
use locus_core::driver::AnyDriver;
use locus_core::store::{DbKind, SavedConnection, Tls};
use locus_core::txn::{TxSession, control};
use std::sync::Arc;
use std::time::Duration;

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

async fn ok(d: &dyn Driver, sql: &str) {
    control(d, sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e:?}"));
}

fn conn(kind: DbKind, host: &str, port: u16, user: &str, db: Option<String>) -> SavedConnection {
    SavedConnection {
        env: Default::default(),
        kind,
        id: format!("t-{kind:?}"),
        name: "t".into(),
        host: host.into(),
        port,
        user: user.into(),
        database: db,
        tls: Tls::Disabled,
        color: None,
        icon: None,
        read_only: false,
        ssh: None,
        mcp: None,
    }
}

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

struct Tmp(std::path::PathBuf);
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn sqlite(tag: &str) -> (Arc<AnyDriver>, Tmp) {
    let p = std::env::temp_dir().join(format!("wb-txn-{tag}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&p);
    db_sqlite::create_database(&p).unwrap();
    let c = conn(DbKind::Sqlite, "", 0, "", Some(p.display().to_string()));
    let rt = tokio::runtime::Handle::current();
    (
        Arc::new(
            AnyDriver::connect(&rt, &c, String::new(), Default::default())
                .await
                .unwrap(),
        ),
        Tmp(p),
    )
}

/// Begin + insert + rollback leaves no row; begin + insert + commit keeps it,
/// and another connection does not see it before the commit.
async fn rollback_and_commit(d: &AnyDriver, table: &str) {
    let s = TxSession::open(d, "c").await.unwrap();
    let tx = &*s.driver;
    s.begin().await.unwrap();
    ok(tx, &format!("INSERT INTO {table} (id) VALUES (1)")).await;
    assert_eq!(
        scalar(tx, &format!("SELECT COUNT(*) FROM {table}")).await,
        "1",
        "the session sees its own row"
    );
    s.rollback().await.unwrap();
    assert_eq!(
        scalar(d, &format!("SELECT COUNT(*) FROM {table}")).await,
        "0",
        "rollback left a row"
    );

    s.begin().await.unwrap();
    ok(tx, &format!("INSERT INTO {table} (id) VALUES (2)")).await;
    if !d.is_sqlite() {
        assert_eq!(
            scalar(d, &format!("SELECT COUNT(*) FROM {table}")).await,
            "0",
            "uncommitted row visible elsewhere"
        );
    }
    s.commit().await.unwrap();
    assert_eq!(
        scalar(d, &format!("SELECT COUNT(*) FROM {table} WHERE id = 2")).await,
        "1",
        "commit lost the row"
    );
}

/// Apply inside an open transaction commits nothing; a failing Apply undoes
/// only its own statements (savepoint) and the transaction's earlier work stays.
async fn apply_joins(d: &AnyDriver, table: &str) {
    let s = TxSession::open(d, "c").await.unwrap();
    let tx = &*s.driver;
    s.begin().await.unwrap();
    ok(tx, &format!("INSERT INTO {table} (id) VALUES (10)")).await;
    let ins = |id: &str| Statement {
        sql: format!("INSERT INTO {table} (id) VALUES (?)"),
        params: vec![Param::Text(id.into())],
        expect_one: true,
        check: None,
    };
    apply_in_tx(tx, false, vec![ins("11")]).await.unwrap();
    // The second statement hits the primary key: only this Apply is undone.
    assert!(
        apply_in_tx(tx, false, vec![ins("12"), ins("10")])
            .await
            .is_err()
    );
    assert_eq!(
        scalar(
            tx,
            &format!("SELECT COUNT(*) FROM {table} WHERE id IN (10, 11, 12)")
        )
        .await,
        "2"
    );
    if !d.is_sqlite() {
        assert_eq!(
            scalar(d, &format!("SELECT COUNT(*) FROM {table} WHERE id >= 10")).await,
            "0",
            "Apply committed on its own"
        );
    }
    s.rollback().await.unwrap();
    assert_eq!(
        scalar(d, &format!("SELECT COUNT(*) FROM {table} WHERE id >= 10")).await,
        "0"
    );
}

/// Cancelling a running statement keeps the transaction open; its earlier
/// row is still there and commits.
async fn cancel_keeps_tx(d: &AnyDriver, table: &str, slow: &str) {
    let s = TxSession::open(d, "c").await.unwrap();
    let tx = &*s.driver;
    s.begin().await.unwrap();
    ok(tx, &format!("INSERT INTO {table} (id) VALUES (20)")).await;
    let mut h = tx.query(slow.into(), QueryOptions::default());
    let c = h.canceller();
    let t = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        c.cancel();
    });
    let mut out = None;
    while let Some(ev) = h.next().await {
        if let QueryEvent::Finished(o) = ev {
            out = Some(o);
        }
    }
    t.await.unwrap();
    assert!(matches!(out, Some(Outcome::Cancelled)), "{out:?}");
    assert_eq!(
        scalar(tx, &format!("SELECT COUNT(*) FROM {table} WHERE id = 20")).await,
        "1",
        "cancel ended the transaction"
    );
    s.commit().await.unwrap();
    assert_eq!(
        scalar(d, &format!("SELECT COUNT(*) FROM {table} WHERE id = 20")).await,
        "1"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_manual_transactions() {
    let (d, _f) = sqlite("tx").await;
    ok(&*d, "CREATE TABLE t (id INTEGER PRIMARY KEY)").await;
    rollback_and_commit(&d, "t").await;
    apply_joins(&d, "t").await;
    let slow =
        "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r) SELECT COUNT(*) FROM r";
    cancel_keeps_tx(&d, "t", slow).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mariadb_manual_transactions() {
    let d = mariadb().await;
    let table = format!("wb_test_txn_{}", std::process::id());
    ok(&*d, &format!("DROP TABLE IF EXISTS {table}")).await;
    ok(
        &*d,
        &format!("CREATE TABLE {table} (id INT PRIMARY KEY) ENGINE=InnoDB"),
    )
    .await;
    rollback_and_commit(&d, &table).await;
    apply_joins(&d, &table).await;
    cancel_keeps_tx(&d, &table, "SELECT SLEEP(30)").await;
    ok(&*d, &format!("DROP TABLE {table}")).await;
}

/// MariaDB commits the transaction on DDL (what the pill reports).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mariadb_ddl_commits() {
    let d = mariadb().await;
    let table = format!("wb_test_txn_ddl_{}", std::process::id());
    ok(&*d, &format!("DROP TABLE IF EXISTS {table}")).await;
    ok(&*d, &format!("CREATE TABLE {table} (id INT PRIMARY KEY)")).await;
    let s = TxSession::open(&d, "c").await.unwrap();
    s.begin().await.unwrap();
    ok(&*s.driver, &format!("INSERT INTO {table} VALUES (1)")).await;
    ok(&*s.driver, &format!("CREATE TABLE {table}_x (a INT)")).await;
    s.rollback().await.unwrap();
    assert_eq!(
        scalar(&*d, &format!("SELECT COUNT(*) FROM {table}")).await,
        "1",
        "DDL did not commit"
    );
    ok(&*d, &format!("DROP TABLE {table}, {table}_x")).await;
}
