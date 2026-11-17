//! Catalog, bound parameters and pinned sessions through the Go sidecar,
//! against the local MariaDB only. Tables of these tests start with
//! `dbw_sc_`.
mod common;
use common::*;
use db_core::{Cell, Driver, ErrorKind, Outcome, Param, QueryEvent};
use db_mysql::MysqlDriver;
use db_sidecar_client::SidecarDriver;
use mysql_async::prelude::*;
use std::time::{Duration, Instant};

/// The tests share the `bench` schema (the catalog test lists all of it): one at a time.
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Runs a statement and returns its first result set as text cells
/// (`None` = NULL) plus the outcome.
async fn rows(
    d: &impl Driver,
    sql: &str,
    params: Option<Vec<Param>>,
) -> (Vec<Vec<Option<String>>>, Outcome, Option<u64>) {
    let mut h = match params {
        Some(p) => d.execute(sql.into(), p, opts()),
        None => d.query(sql.into(), opts()),
    };
    let mut out = Vec::new();
    let mut affected = None;
    loop {
        match tokio::time::timeout(Duration::from_secs(30), h.next())
            .await
            .expect("no event in 30 s")
        {
            Some(QueryEvent::Batch(b)) => {
                for r in 0..b.rows() {
                    out.push(
                        (0..b.columns())
                            .map(|c| match b.cell(r, c) {
                                Cell::Null => None,
                                Cell::Text(s) => Some(s.to_string()),
                                Cell::Bytes(x) => Some(format!("{x:02x?}")),
                            })
                            .collect(),
                    );
                }
            }
            Some(QueryEvent::Affected { rows }) => affected = Some(rows),
            Some(QueryEvent::Finished(o)) => return (out, o, affected),
            Some(_) => {}
            None => panic!("stream ended without Finished"),
        }
    }
}

async fn ok(d: &impl Driver, sql: &str) {
    let (_, o, _) = rows(d, sql, None).await;
    assert!(matches!(o, Outcome::Done { .. }), "{sql}: {o:?}");
}

async fn one(d: &impl Driver, sql: &str) -> String {
    rows(d, sql, None).await.0[0][0]
        .clone()
        .unwrap_or_else(|| "<NULL>".into())
}

#[tokio::test]
async fn catalog_equals_db_mysql() {
    let _g = LOCK.lock().await;
    let side = driver().await;
    let direct = MysqlDriver::connect(
        &tokio::runtime::Handle::current(),
        connect_options(db_core::TlsMode::Disabled),
    )
    .await
    .unwrap();
    ok(&side, "DROP TABLE IF EXISTS bench.dbw_sc_child").await;
    ok(&side, "DROP TABLE IF EXISTS bench.dbw_sc_parent").await;
    ok(&side, "CREATE TABLE bench.dbw_sc_parent (id INT NOT NULL, k VARCHAR(20) NOT NULL, PRIMARY KEY (id, k)) ENGINE=InnoDB").await;
    ok(&side, "CREATE TABLE bench.dbw_sc_child (cid INT PRIMARY KEY, pid INT NOT NULL, pk VARCHAR(20) NOT NULL, note TEXT NULL, CONSTRAINT dbw_sc_fk FOREIGN KEY (pid, pk) REFERENCES dbw_sc_parent (id, k)) ENGINE=InnoDB").await;
    ok(
        &side,
        "CREATE OR REPLACE VIEW bench.dbw_sc_view AS SELECT cid FROM dbw_sc_child",
    )
    .await;

    let (a, b) = (
        side.catalog().expect("sidecar catalog"),
        direct.catalog().unwrap(),
    );
    assert_eq!(
        a.current_schema().await.unwrap(),
        b.current_schema().await.unwrap()
    );
    assert_eq!(a.current_schema().await.unwrap().as_deref(), Some("bench"));
    assert_eq!(a.schemas().await.unwrap(), b.schemas().await.unwrap());
    let objs = a.objects("bench").await.unwrap();
    assert_eq!(objs, b.objects("bench").await.unwrap());
    assert!(
        objs.iter()
            .any(|o| o.name == "dbw_sc_view" && o.kind == db_core::ObjectKind::View)
    );
    let mut columns = 0;
    for o in &objs {
        let (ca, cb) = (
            a.columns("bench", &o.name).await.unwrap(),
            b.columns("bench", &o.name).await.unwrap(),
        );
        assert_eq!(ca, cb, "columns of {}", o.name);
        columns += ca.len();
    }
    let fks = a.foreign_keys("bench").await.unwrap();
    assert_eq!(fks, b.foreign_keys("bench").await.unwrap());
    let fk = fks.iter().find(|k| k.name == "dbw_sc_fk").expect("fk");
    assert_eq!(
        (fk.columns.clone(), fk.ref_columns.clone()),
        (
            vec!["pid".into(), "pk".into()],
            vec!["id".into(), "k".into()]
        )
    );
    let pk = a.columns("bench", "dbw_sc_parent").await.unwrap();
    assert!(pk.iter().all(|c| c.primary_key));
    println!(
        "[catalog] bench: {} objects, {columns} columns, {} foreign keys: identical to db-mysql",
        objs.len(),
        fks.len()
    );
    // quoting follows the dialect
    assert_eq!(side.quote_identifier("a`b"), "`a``b`");

    ok(&side, "DROP VIEW bench.dbw_sc_view").await;
    ok(&side, "DROP TABLE bench.dbw_sc_child").await;
    ok(&side, "DROP TABLE bench.dbw_sc_parent").await;
    side.shutdown().await;
}

#[tokio::test]
async fn execute_binds_values() {
    let _g = LOCK.lock().await;
    let d = driver().await;
    ok(&d, "DROP TABLE IF EXISTS bench.dbw_sc_exec").await;
    ok(&d, "CREATE TABLE bench.dbw_sc_exec (id INT PRIMARY KEY, name VARCHAR(100) NULL, b BLOB NULL) ENGINE=InnoDB").await;

    let hostile = "x'); DROP TABLE bench.dbw_sc_exec; -- \\ \" ?";
    let (_, o, aff) = rows(
        &d,
        "INSERT INTO bench.dbw_sc_exec (id, name, b) VALUES (?, ?, ?)",
        Some(vec![
            Param::Text("1".into()),
            Param::Text(hostile.into()),
            Param::Bytes(vec![0xff, 0, 0xfe]),
        ]),
    )
    .await;
    assert!(matches!(o, Outcome::Done { .. }), "{o:?}");
    assert_eq!(aff, Some(1));
    let (_, o, aff) = rows(
        &d,
        "INSERT INTO bench.dbw_sc_exec (id, name, b) VALUES (?, ?, ?)",
        Some(vec![Param::Text("2".into()), Param::Null, Param::Null]),
    )
    .await;
    assert!(
        matches!(o, Outcome::Done { .. }) && aff == Some(1),
        "{o:?} {aff:?}"
    );

    // The hostile text is stored as data, the table is intact.
    let (r, _, _) = rows(
        &d,
        "SELECT name, b FROM bench.dbw_sc_exec WHERE id = ?",
        Some(vec![Param::Text("1".into())]),
    )
    .await;
    assert_eq!(r[0][0].as_deref(), Some(hostile));
    assert_eq!(r[0][1].as_deref(), Some("[ff, 00, fe]"));
    let (r, _, _) = rows(
        &d,
        "SELECT name, b FROM bench.dbw_sc_exec WHERE id = ?",
        Some(vec![Param::Text("2".into())]),
    )
    .await;
    assert_eq!(r[0], vec![None, None]);

    // UPDATE reports the count; NULL and empty text are different values.
    let (_, _, aff) = rows(
        &d,
        "UPDATE bench.dbw_sc_exec SET name = ? WHERE id = ?",
        Some(vec![Param::Text(String::new()), Param::Text("2".into())]),
    )
    .await;
    assert_eq!(aff, Some(1));
    assert_eq!(
        one(&d, "SELECT COUNT(*) FROM bench.dbw_sc_exec WHERE name = ''").await,
        "1"
    );
    let (_, _, aff) = rows(
        &d,
        "DELETE FROM bench.dbw_sc_exec WHERE id = ?",
        Some(vec![Param::Text("2".into())]),
    )
    .await;
    assert_eq!(aff, Some(1));
    // A server error comes back as a normal failure.
    let (_, o, _) = rows(
        &d,
        "INSERT INTO bench.dbw_sc_exec (id) VALUES (?)",
        Some(vec![Param::Text("1".into())]),
    )
    .await;
    match o {
        Outcome::Failed(e) => assert_eq!((e.kind, e.code), (ErrorKind::Query, Some(1062))),
        o => panic!("duplicate key: {o:?}"),
    }
    ok(&d, "DROP TABLE bench.dbw_sc_exec").await;
    d.shutdown().await;
}

async fn gone(obs: &mut Obs, id: u64) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < lim(Duration::from_secs(5)) {
        let n: Option<u64> = obs
            .0
            .exec_first(
                "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID=?",
                (id,),
            )
            .await
            .unwrap();
        if n.unwrap_or(0) == 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test]
async fn dropped_session_rolls_back_and_closes_its_connection() {
    let _g = LOCK.lock().await;
    let d = driver().await;
    let mut obs = Obs::new().await;
    ok(&d, "DROP TABLE IF EXISTS bench.dbw_sc_tx").await;
    ok(
        &d,
        "CREATE TABLE bench.dbw_sc_tx (id INT PRIMARY KEY, v VARCHAR(10)) ENGINE=InnoDB",
    )
    .await;
    let ins = |id: &str| {
        (
            String::from("INSERT INTO bench.dbw_sc_tx (id, v) VALUES (?, 'x')"),
            vec![Param::Text(id.into())],
        )
    };

    // 1) rolled back on drop
    let s = d.session().await.expect("session");
    let sid = s.session_id();
    assert_ne!(sid, 0);
    ok(&s, "START TRANSACTION").await;
    let (sql, p) = ins("1");
    assert!(matches!(
        rows(&s, &sql, Some(p)).await.1,
        Outcome::Done { .. }
    ));
    assert_eq!(
        one(&s, "SELECT CONNECTION_ID()").await,
        sid.to_string(),
        "statements run on one connection"
    );
    assert_eq!(
        one(&s, "SELECT COUNT(*) FROM bench.dbw_sc_tx").await,
        "1",
        "the session sees its own row"
    );
    assert_eq!(
        one(&d, "SELECT COUNT(*) FROM bench.dbw_sc_tx").await,
        "0",
        "other connections do not"
    );
    drop(s);
    assert!(
        gone(&mut obs, sid).await,
        "the session's connection is still on the server"
    );
    assert_eq!(
        one(&d, "SELECT COUNT(*) FROM bench.dbw_sc_tx").await,
        "0",
        "rolled back"
    );

    // 2) a committed transaction survives the drop
    let s = d.session().await.unwrap();
    let sid2 = s.session_id();
    assert_ne!(sid, sid2);
    ok(&s, "START TRANSACTION").await;
    let (sql, p) = ins("2");
    rows(&s, &sql, Some(p)).await;
    ok(&s, "COMMIT").await;
    drop(s);
    assert!(gone(&mut obs, sid2).await);
    assert_eq!(one(&d, "SELECT COUNT(*) FROM bench.dbw_sc_tx").await, "1");

    // 3) cancelling a statement keeps the session and its open transaction
    let s = d.session().await.unwrap();
    let sid3 = s.session_id();
    ok(&s, "START TRANSACTION").await;
    let (sql, p) = ins("3");
    rows(&s, &sql, Some(p)).await;
    let mut h = s.query("SELECT SLEEP(10)".into(), opts());
    started(&mut h).await;
    let t0 = Instant::now();
    h.cancel();
    let (_, o) = drain(&mut h).await;
    assert!(matches!(o, Outcome::Cancelled), "{o:?}");
    assert!(
        t0.elapsed() < lim(Duration::from_secs(3)),
        "cancel took {:?}",
        t0.elapsed()
    );
    assert_eq!(
        one(&s, "SELECT CONNECTION_ID()").await,
        sid3.to_string(),
        "same connection after cancel"
    );
    assert_eq!(
        one(&s, "SELECT COUNT(*) FROM bench.dbw_sc_tx WHERE id = 3").await,
        "1",
        "transaction still open"
    );
    drop(s);
    assert!(gone(&mut obs, sid3).await);
    assert_eq!(
        one(&d, "SELECT COUNT(*) FROM bench.dbw_sc_tx").await,
        "1",
        "row 3 rolled back"
    );

    // 4) dropping the session while a statement runs ends the statement
    let s = d.session().await.unwrap();
    let sid4 = s.session_id();
    let mut h = s.query("SELECT SLEEP(20)".into(), opts());
    started(&mut h).await;
    drop(s);
    let t0 = Instant::now();
    assert!(
        gone(&mut obs, sid4).await,
        "connection still there after drop"
    );
    assert!(t0.elapsed() < lim(Duration::from_secs(4)));
    let (_, o) = drain(&mut h).await;
    assert!(
        matches!(o, Outcome::Cancelled | Outcome::Failed(_)),
        "{o:?}"
    );

    ok(&d, "DROP TABLE bench.dbw_sc_tx").await;
    assert!(select1_ok(&d).await, "the pool still works");
    let _: &SidecarDriver = &d;
    d.shutdown().await;
}

#[tokio::test]
async fn extended_catalog_equals_db_mysql() {
    use db_core::DbObjectKind as K;
    let _g = LOCK.lock().await;
    let side = driver().await;
    let direct = MysqlDriver::connect(
        &tokio::runtime::Handle::current(),
        connect_options(db_core::TlsMode::Disabled),
    )
    .await
    .unwrap();
    for s in [
        "DROP TRIGGER IF EXISTS bench.dbw_sc_trg",
        "DROP PROCEDURE IF EXISTS bench.dbw_sc_proc",
        "DROP FUNCTION IF EXISTS bench.dbw_sc_fn",
        "DROP VIEW IF EXISTS bench.dbw_sc_xview",
        "DROP TABLE IF EXISTS bench.dbw_sc_xt",
        "CREATE TABLE bench.dbw_sc_xt (id INT PRIMARY KEY, a VARCHAR(10), b INT, UNIQUE KEY dbw_sc_xu (a, b)) ENGINE=InnoDB",
        "CREATE VIEW bench.dbw_sc_xview AS SELECT id FROM bench.dbw_sc_xt",
        "CREATE PROCEDURE bench.dbw_sc_proc(IN p1 INT, OUT p2 VARCHAR(5)) BEGIN SET p2 = 'x'; END",
        "CREATE FUNCTION bench.dbw_sc_fn(n INT) RETURNS INT DETERMINISTIC RETURN n + 1",
        "CREATE TRIGGER bench.dbw_sc_trg BEFORE INSERT ON bench.dbw_sc_xt FOR EACH ROW SET NEW.b = 1",
    ] {
        ok(&side, s).await;
    }
    let (a, b) = (side.catalog().unwrap(), direct.catalog().unwrap());
    assert_eq!(a.supported_kinds(), b.supported_kinds());
    let mut lists = 0;
    for kind in a.supported_kinds() {
        let (x, y) = (
            a.list_objects("bench", kind, 1000).await.unwrap(),
            b.list_objects("bench", kind, 1000).await.unwrap(),
        );
        assert_eq!(x, y, "{kind:?}");
        lists += x.len();
    }
    assert_eq!(a.list_objects("bench", K::Table, 1).await.unwrap().len(), 1);
    for kind in [K::Index, K::Trigger] {
        assert_eq!(
            a.children("bench", "dbw_sc_xt", kind, 100).await.unwrap(),
            b.children("bench", "dbw_sc_xt", kind, 100).await.unwrap()
        );
    }
    let tr = a
        .children("bench", "dbw_sc_xt", K::Trigger, 100)
        .await
        .unwrap();
    assert_eq!(tr[0].detail.as_deref(), Some("BEFORE INSERT"));
    for (kind, name) in [(K::Procedure, "dbw_sc_proc"), (K::Function, "dbw_sc_fn")] {
        assert_eq!(
            a.routine_params("bench", kind, name, 100).await.unwrap(),
            b.routine_params("bench", kind, name, 100).await.unwrap()
        );
    }
    for (kind, name) in [
        (K::Table, "dbw_sc_xt"),
        (K::View, "dbw_sc_xview"),
        (K::Procedure, "dbw_sc_proc"),
        (K::Function, "dbw_sc_fn"),
        (K::Trigger, "dbw_sc_trg"),
    ] {
        let (x, y) = (
            a.ddl("bench", kind, name).await.unwrap(),
            b.ddl("bench", kind, name).await.unwrap(),
        );
        assert_eq!(x, y, "{kind:?}");
        assert!(x.contains(name));
    }
    assert!(a.ddl("bench", K::Index, "dbw_sc_xu").await.is_err());
    assert_eq!(
        a.principals(100).await.unwrap(),
        b.principals(100).await.unwrap()
    );
    let (ia, ib) = (
        a.server_info(2000).await.unwrap(),
        b.server_info(2000).await.unwrap(),
    );
    // session variables of two different connections can differ (ids, counters): compare the rest
    let stable = |v: &[db_core::InfoItem]| {
        v.iter()
            .filter(|i| i.group != "session variable")
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(stable(&ia), stable(&ib));
    assert!(
        ia.iter().any(|i| i.group == "session variable")
            && ia
                .iter()
                .any(|i| i.group == "server" && i.name == "version")
    );
    println!(
        "[catalog-ext] sidecar == db-mysql: {lists} listed objects, {} info items",
        ia.len()
    );

    for s in [
        "DROP TRIGGER bench.dbw_sc_trg",
        "DROP PROCEDURE bench.dbw_sc_proc",
        "DROP FUNCTION bench.dbw_sc_fn",
        "DROP VIEW bench.dbw_sc_xview",
        "DROP TABLE bench.dbw_sc_xt",
    ] {
        ok(&side, s).await;
    }
    side.shutdown().await;
}
