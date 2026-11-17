//! Integration tests on a temp file: create, insert, stream 100k rows in
//! batches, cancel a long query, catalog incl. foreign keys, apply edits.

use db_core::{Cell, Driver, ErrorKind, Outcome, Param, QueryEvent, QueryOptions};
use db_sqlite::SqliteDriver;
use std::path::PathBuf;
use std::time::Duration;

struct Tmp(PathBuf);
impl Tmp {
    fn new(tag: &str) -> Tmp {
        let p = std::env::temp_dir().join(format!("wb-sqlite-{tag}-{}.db", std::process::id()));
        for ext in ["", "-journal", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", p.display()));
        }
        db_sqlite::create_database(&p).unwrap();
        Tmp(p)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[derive(Default, Debug)]
struct Run {
    batches: Vec<usize>,
    cols: Vec<String>,
    first_row: Vec<String>,
    affected: Vec<u64>,
    outcome: Option<String>,
    rows: u64,
}

async fn run(d: &dyn Driver, sql: &str, params: Vec<Param>, opts: QueryOptions) -> Run {
    let mut h = if params.is_empty() {
        d.query(sql.into(), opts)
    } else {
        d.execute(sql.into(), params, opts)
    };
    let mut r = Run::default();
    while let Some(ev) = h.next().await {
        match ev {
            QueryEvent::Columns(c) => r.cols = c.into_iter().map(|c| c.name).collect(),
            QueryEvent::Batch(b) => {
                if r.first_row.is_empty() && b.rows() > 0 {
                    r.first_row = (0..b.columns())
                        .map(|c| match b.cell(0, c) {
                            Cell::Null => "NULL".into(),
                            Cell::Text(t) => t.into(),
                            Cell::Bytes(x) => format!("{x:?}"),
                        })
                        .collect();
                }
                r.batches.push(b.rows());
            }
            QueryEvent::Affected { rows } => r.affected.push(rows),
            QueryEvent::Finished(o) => {
                r.outcome = Some(match o {
                    Outcome::Done { rows } => {
                        r.rows = rows;
                        "done".into()
                    }
                    Outcome::Cancelled => "cancelled".into(),
                    Outcome::Failed(e) => format!("failed {:?} {}", e.kind, e.message),
                })
            }
            _ => {}
        }
    }
    r
}

fn opts() -> QueryOptions {
    QueryOptions::default()
}

async fn connect(t: &Tmp) -> SqliteDriver {
    SqliteDriver::connect(&t.0, false).await.unwrap()
}

const ENDLESS_COUNT: &str =
    "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c) SELECT count(*) FROM c";

#[tokio::test]
async fn create_insert_select() {
    let t = Tmp::new("basic");
    let d = connect(&t).await;
    let r = run(&d, "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, score REAL, data BLOB); INSERT INTO t VALUES (1,'a',1,x'ff'),(2,NULL,2.5,NULL)", vec![], opts()).await;
    assert_eq!(r.outcome.as_deref(), Some("done"));
    assert_eq!(r.affected, vec![0, 2]);
    let r = run(
        &d,
        "SELECT id, name, score, data FROM t ORDER BY id",
        vec![],
        opts(),
    )
    .await;
    assert_eq!(r.cols, ["id", "name", "score", "data"]);
    assert_eq!(r.first_row, ["1", "a", "1.0", "[255]"]);
    assert_eq!(r.rows, 2);
    let r = run(&d, "SELEC nope", vec![], opts()).await;
    assert!(r.outcome.unwrap().starts_with("failed Query"));
    // Missing file and a file that is not a database are connect errors.
    let e = SqliteDriver::connect(t.0.with_extension("missing"), false)
        .await
        .err()
        .unwrap();
    assert_eq!(e.kind, ErrorKind::Connect);
    let junk = t.0.with_extension("junk");
    std::fs::write(
        &junk,
        b"this is not a database, but long enough to look like a header....................",
    )
    .unwrap();
    let e = SqliteDriver::connect(&junk, false).await.err().unwrap();
    assert_eq!(e.kind, ErrorKind::Connect);
    let _ = std::fs::remove_file(junk);
    assert!(
        db_sqlite::create_database(&t.0).is_err(),
        "never overwrite an existing file"
    );
}

#[tokio::test]
async fn streams_100k_rows_in_batches() {
    let t = Tmp::new("stream");
    let d = connect(&t).await;
    let r = run(&d, "CREATE TABLE n(i INTEGER); WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 100000) INSERT INTO n SELECT x FROM c", vec![], opts()).await;
    assert_eq!(r.outcome.as_deref(), Some("done"));
    let r = run(&d, "SELECT i FROM n", vec![], opts()).await;
    assert_eq!(r.rows, 100_000);
    assert_eq!(r.batches.len(), 100);
    assert!(r.batches.iter().all(|b| *b == 1000));
    let r = run(
        &d,
        "SELECT i FROM n",
        vec![],
        QueryOptions {
            batch_rows: 333,
            ..opts()
        },
    )
    .await;
    assert_eq!(r.batches.iter().sum::<usize>(), 100_000);
    assert_eq!(*r.batches.last().unwrap(), 100_000 % 333);
}

#[tokio::test]
async fn cancel_a_long_query_and_timeout() {
    let t = Tmp::new("cancel");
    let d = connect(&t).await;
    // No first row for a very long time: only sqlite3_interrupt can stop it.
    let mut h = d.query(ENDLESS_COUNT.into(), opts());
    let started = std::time::Instant::now();
    assert!(matches!(h.next().await, Some(QueryEvent::Started { .. })));
    tokio::time::sleep(Duration::from_millis(300)).await;
    h.cancel();
    let mut outcome = None;
    while let Some(ev) = h.next().await {
        if let QueryEvent::Finished(o) = ev {
            outcome = Some(o);
        }
    }
    assert!(matches!(outcome, Some(Outcome::Cancelled)), "{outcome:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
    let r = run(
        &d,
        ENDLESS_COUNT,
        vec![],
        QueryOptions {
            timeout: Some(Duration::from_millis(200)),
            ..opts()
        },
    )
    .await;
    assert!(r.outcome.unwrap().starts_with("failed Timeout"));
    assert_eq!(run(&d, "SELECT 1", vec![], opts()).await.rows, 1);
}

#[tokio::test]
async fn dropped_handle_stops_an_endless_stream() {
    let t = Tmp::new("pressure");
    let d = connect(&t).await;
    let mut h = d.query(
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c) SELECT x FROM c".into(),
        opts(),
    );
    for _ in 0..5 {
        h.next().await;
    }
    drop(h);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(run(&d, "SELECT 1", vec![], opts()).await.rows, 1);
}

#[tokio::test]
async fn catalog_tables_columns_indexes_foreign_keys() {
    let t = Tmp::new("catalog");
    let d = connect(&t).await;
    let ddl = "CREATE TABLE parent(id INTEGER PRIMARY KEY, code TEXT NOT NULL UNIQUE);
        CREATE TABLE child(a INTEGER, b TEXT, parent_id INTEGER REFERENCES parent(id), pcode TEXT, PRIMARY KEY (a, b), FOREIGN KEY (pcode) REFERENCES parent(code));
        CREATE TABLE pk_ref(x INTEGER REFERENCES parent);
        CREATE INDEX child_parent ON child(parent_id);
        CREATE VIEW v AS SELECT id FROM parent;
        CREATE TRIGGER trg AFTER INSERT ON parent BEGIN SELECT 1; END;";
    assert_eq!(
        run(&d, ddl, vec![], opts()).await.outcome.as_deref(),
        Some("done")
    );
    let cat = d.catalog().unwrap();
    assert_eq!(cat.schemas().await.unwrap(), ["main"]);
    assert_eq!(cat.current_schema().await.unwrap().as_deref(), Some("main"));
    let objs = cat.objects("main").await.unwrap();
    let names: Vec<_> = objs.iter().map(|o| (o.name.as_str(), o.kind)).collect();
    use db_core::ObjectKind::*;
    assert_eq!(
        names,
        [
            ("child", Table),
            ("parent", Table),
            ("pk_ref", Table),
            ("v", View)
        ]
    );
    let cols = cat.columns("main", "child").await.unwrap();
    let c: Vec<_> = cols
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                c.type_name.as_str(),
                c.nullable,
                c.primary_key,
            )
        })
        .collect();
    assert_eq!(
        c,
        [
            ("a", "INTEGER", false, true),
            ("b", "TEXT", false, true),
            ("parent_id", "INTEGER", true, false),
            ("pcode", "TEXT", true, false)
        ]
    );
    let pcols = cat.columns("main", "parent").await.unwrap();
    assert!(!pcols[1].primary_key && !pcols[1].nullable);
    let fks = cat.foreign_keys("main").await.unwrap();
    let f: Vec<_> = fks
        .iter()
        .map(|f| {
            (
                f.table.as_str(),
                f.columns.clone(),
                f.ref_table.as_str(),
                f.ref_columns.clone(),
            )
        })
        .collect();
    assert_eq!(f.len(), 3, "{f:?}");
    assert!(f.contains(&(
        "child",
        vec!["parent_id".into()],
        "parent",
        vec!["id".into()]
    )));
    assert!(f.contains(&("child", vec!["pcode".into()], "parent", vec!["code".into()])));
    assert!(
        f.contains(&("pk_ref", vec!["x".into()], "parent", vec!["id".into()])),
        "implicit primary key target: {f:?}"
    );
    let idx = cat
        .children("main", "child", db_core::DbObjectKind::Index, 100)
        .await
        .unwrap();
    assert_eq!(idx.len(), 1, "{idx:?}");
    assert_eq!(idx[0].detail.as_deref(), Some("parent_id"));
    let trg = cat
        .list_objects("main", db_core::DbObjectKind::Trigger, 100)
        .await
        .unwrap();
    assert_eq!(trg[0].parent.as_deref(), Some("parent"));
    let ddl = cat
        .ddl("main", db_core::DbObjectKind::View, "v")
        .await
        .unwrap();
    assert!(ddl.starts_with("CREATE VIEW v"));
    assert!(
        cat.server_info(10)
            .await
            .unwrap()
            .iter()
            .any(|i| i.name == "version")
    );
    // Foreign keys are enforced.
    let r = run(
        &d,
        "INSERT INTO child(a,b,parent_id) VALUES (1,'x',999)",
        vec![],
        opts(),
    )
    .await;
    assert!(r.outcome.unwrap().contains("FOREIGN KEY"));
}

#[tokio::test]
async fn result_columns_know_their_table() {
    let t = Tmp::new("origin");
    let d = connect(&t).await;
    run(
        &d,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1,'a')",
        vec![],
        opts(),
    )
    .await;
    let mut h = d.query("SELECT id AS k, upper(name) FROM t".into(), opts());
    let mut seen = false;
    while let Some(ev) = h.next().await {
        if let QueryEvent::Columns(c) = ev {
            let o = c[0].origin.as_ref().unwrap();
            assert_eq!(
                (o.table.as_str(), o.column.as_str(), o.schema.as_str()),
                ("t", "id", "main")
            );
            assert!(c[1].origin.is_none());
            seen = true;
        }
    }
    assert!(seen);
}

#[tokio::test]
async fn apply_edits_in_a_transaction_with_rowid() {
    let t = Tmp::new("apply");
    let d = connect(&t).await;
    run(
        &d,
        "CREATE TABLE t(name TEXT, n INTEGER); INSERT INTO t VALUES ('a',1),('b',2)",
        vec![],
        opts(),
    )
    .await;
    let s = d.session().await.unwrap();
    // The Apply path sends START TRANSACTION.
    assert_eq!(
        run(&s, "START TRANSACTION", vec![], opts())
            .await
            .outcome
            .as_deref(),
        Some("done")
    );
    let r = run(
        &s,
        "UPDATE t SET n = ? WHERE rowid = ?",
        vec![Param::Text("20".into()), Param::Text("2".into())],
        opts(),
    )
    .await;
    assert_eq!(r.affected, [1]);
    run(&s, "ROLLBACK", vec![], opts()).await;
    assert_eq!(
        run(&d, "SELECT n FROM t WHERE name='b'", vec![], opts())
            .await
            .first_row,
        ["2"]
    );
    run(&s, "START TRANSACTION", vec![], opts()).await;
    run(
        &s,
        "UPDATE t SET n = ? WHERE rowid = ?",
        vec![Param::Text("20".into()), Param::Text("2".into())],
        opts(),
    )
    .await;
    run(
        &s,
        "INSERT INTO t(name, n) VALUES (?, ?)",
        vec![Param::Text("c".into()), Param::Null],
        opts(),
    )
    .await;
    run(
        &s,
        "DELETE FROM t WHERE rowid = ?",
        vec![Param::Text("1".into())],
        opts(),
    )
    .await;
    assert_eq!(
        run(&s, "COMMIT", vec![], opts()).await.outcome.as_deref(),
        Some("done")
    );
    let r = run(&d, "SELECT name, n FROM t ORDER BY rowid", vec![], opts()).await;
    assert_eq!(r.rows, 2);
    assert_eq!(r.first_row, ["b", "20"]);
}

#[tokio::test]
async fn read_only_modes() {
    let t = Tmp::new("ro");
    let d = connect(&t).await;
    run(&d, "CREATE TABLE t(a)", vec![], opts()).await;
    // Production guard: PRAGMA query_only.
    d.set_read_only(true);
    let r = run(&d, "INSERT INTO t VALUES (1)", vec![], opts()).await;
    assert!(r.outcome.unwrap().contains("readonly"));
    assert_eq!(
        run(&d, "SELECT count(*) FROM t", vec![], opts())
            .await
            .first_row,
        ["0"]
    );
    d.set_read_only(false);
    assert_eq!(
        run(&d, "INSERT INTO t VALUES (1)", vec![], opts())
            .await
            .outcome
            .as_deref(),
        Some("done")
    );
    // Opened read-only: the Production guard cannot turn writes on.
    let ro = SqliteDriver::connect(&t.0, true).await.unwrap();
    assert!(
        run(&ro, "INSERT INTO t VALUES (2)", vec![], opts())
            .await
            .outcome
            .unwrap()
            .starts_with("failed")
    );
    ro.set_read_only(false);
    assert!(
        run(&ro, "INSERT INTO t VALUES (2)", vec![], opts())
            .await
            .outcome
            .unwrap()
            .starts_with("failed")
    );
    assert_eq!(
        run(&ro, "SELECT count(*) FROM t", vec![], opts())
            .await
            .first_row,
        ["1"]
    );
}

#[tokio::test]
async fn structure_of_a_table() {
    let t = Tmp::new("structure");
    let d = connect(&t).await;
    let ddl = "CREATE TABLE parent(id INTEGER PRIMARY KEY, code TEXT NOT NULL UNIQUE, qty INT DEFAULT 5 CHECK (qty >= 0),
          CONSTRAINT code_len CHECK (length(code) < 20));
        CREATE TABLE child(a INTEGER, b TEXT, parent_id INTEGER REFERENCES parent(id) ON DELETE CASCADE ON UPDATE SET NULL, PRIMARY KEY (a, b));
        CREATE INDEX child_parent ON child(parent_id);
        CREATE TRIGGER trg AFTER INSERT ON parent BEGIN SELECT 1; END;";
    assert_eq!(
        run(&d, ddl, vec![], opts()).await.outcome.as_deref(),
        Some("done")
    );
    let cat = d.catalog().unwrap();

    let p = cat.structure("main", "parent", 100).await.unwrap();
    let c: Vec<_> = p
        .columns
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                c.nullable,
                c.primary_key,
                c.auto_increment,
                c.default.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        c,
        [
            ("id", false, true, true, None),
            ("code", false, false, false, None),
            ("qty", true, false, false, Some("5"))
        ]
    );
    assert_eq!(p.indexes.len(), 1);
    assert!(p.indexes[0].unique && p.indexes[0].columns == ["code"]);
    let checks: Vec<_> = p
        .checks
        .iter()
        .map(|c| (c.name.as_str(), c.expression.as_str()))
        .collect();
    assert_eq!(
        checks,
        [("check_1", "qty >= 0"), ("code_len", "length(code) < 20")]
    );
    assert_eq!(p.triggers.len(), 1);
    assert_eq!(
        (p.triggers[0].name.as_str(), p.triggers[0].detail.as_deref()),
        ("trg", Some("AFTER INSERT"))
    );
    assert!(p.foreign_keys.is_empty());
    assert_eq!(p.referenced_by.len(), 1);
    assert_eq!(
        (
            p.referenced_by[0].table.as_str(),
            p.referenced_by[0].on_delete.as_str(),
            p.referenced_by[0].on_update.as_str()
        ),
        ("child", "CASCADE", "SET NULL")
    );

    let ch = cat.structure("main", "child", 100).await.unwrap();
    assert_eq!(ch.columns.iter().filter(|c| c.primary_key).count(), 2);
    assert!(ch.columns.iter().all(|c| !c.auto_increment));
    let names: Vec<_> = ch
        .indexes
        .iter()
        .map(|i| (i.primary, i.columns.clone()))
        .collect();
    assert_eq!(
        names,
        [
            (true, vec!["a".to_string(), "b".to_string()]),
            (false, vec!["parent_id".to_string()])
        ]
    );
    assert_eq!(ch.foreign_keys.len(), 1);
    assert_eq!(
        (
            ch.foreign_keys[0].ref_table.as_str(),
            ch.foreign_keys[0].ref_columns.clone()
        ),
        ("parent", vec!["id".to_string()])
    );
    assert!(ch.referenced_by.is_empty());

    let ddl = cat
        .ddl("main", db_core::DbObjectKind::Table, "parent")
        .await
        .unwrap();
    assert!(ddl.starts_with("CREATE TABLE parent"));
}
