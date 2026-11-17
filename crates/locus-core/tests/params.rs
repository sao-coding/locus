//! Query parameters end to end: placeholders detected in the statement as the
//! user typed it, bound through `Driver::execute` on SQLite (temp file) and
//! the local MariaDB (`BENCH_DSN`, own `wb_test_params` table, dropped after).

use db_core::{Cell, Driver, Outcome, Param, QueryEvent, QueryOptions};
use locus_core::driver::AnyDriver;
use locus_core::params::{ParamType, ParamValue, bind, detect, names};
use locus_core::store::{DbKind, SavedConnection, Tls};
use std::collections::HashMap;

async fn run(d: &dyn Driver, sql: &str, params: Vec<Param>) -> Vec<Vec<String>> {
    let mut h = if params.is_empty() {
        d.query(sql.into(), QueryOptions::default())
    } else {
        d.execute(sql.into(), params, QueryOptions::default())
    };
    let mut rows = Vec::new();
    while let Some(ev) = h.next().await {
        match ev {
            QueryEvent::Batch(b) => {
                for r in 0..b.rows() {
                    rows.push(
                        (0..b.columns())
                            .map(|c| match b.cell(r, c) {
                                Cell::Text(t) => t.to_string(),
                                Cell::Null => "NULL".into(),
                                other => format!("{other:?}"),
                            })
                            .collect(),
                    );
                }
            }
            QueryEvent::Finished(Outcome::Failed(e)) => panic!("{sql}: {e:?}"),
            _ => {}
        }
    }
    rows
}

fn v(ty: ParamType, text: &str) -> ParamValue {
    ParamValue {
        ty,
        text: text.into(),
    }
}

/// Named and repeated placeholders, a quoted lookalike and a NULL value.
async fn scenario(d: &AnyDriver, kind: DbKind, table: &str) {
    run(
        d,
        &format!("INSERT INTO {table} (id, name) VALUES (1, 'a:b'), (2, 'it''s'), (3, NULL)"),
        vec![],
    )
    .await;
    // Named, used twice: one value row, two bound values.
    let sql = format!("SELECT id FROM {table} WHERE id = :x OR id = :x + 1 ORDER BY id");
    let phs = detect(&sql, kind);
    assert_eq!(names(&phs), ["x"]);
    let mut vals = HashMap::new();
    vals.insert("x".to_string(), v(ParamType::Number, "1"));
    let (send, bound) = bind(&sql, &phs, &vals);
    assert_eq!(bound.len(), 2);
    assert_eq!(
        run(d, &send, bound).await,
        vec![vec!["1".to_string()], vec!["2".to_string()]]
    );

    // The ':b' inside a string is not a placeholder; the quote in 'it''s' stays data.
    let sql = format!("SELECT id FROM {table} WHERE name = 'a:b' AND id = ?");
    let phs = detect(&sql, kind);
    assert_eq!(phs.len(), 1);
    let mut vals = HashMap::new();
    vals.insert(phs[0].name.clone(), v(ParamType::Number, "1"));
    let (send, bound) = bind(&sql, &phs, &vals);
    assert_eq!(run(d, &send, bound).await, vec![vec!["1".to_string()]]);

    // A value that looks like SQL is data.
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE name = :n");
    let phs = detect(&sql, kind);
    let mut vals = HashMap::new();
    vals.insert("n".to_string(), v(ParamType::Text, "x' OR '1'='1"));
    let (send, bound) = bind(&sql, &phs, &vals);
    assert_eq!(run(d, &send, bound).await, vec![vec!["0".to_string()]]);

    // NULL: `IS NULL` with a NULL-typed value finds the row.
    let sql = format!("SELECT id FROM {table} WHERE (name IS NULL) = (:n IS NULL) AND id = 3");
    let phs = detect(&sql, kind);
    let mut vals = HashMap::new();
    vals.insert("n".to_string(), v(ParamType::Null, "ignored"));
    let (send, bound) = bind(&sql, &phs, &vals);
    assert_eq!(bound, vec![Param::Null]);
    assert_eq!(run(d, &send, bound).await, vec![vec!["3".to_string()]]);

    // A date typed value reaches the server as text and compares as a date.
    let sql = "SELECT :d = '2024-02-29'";
    let phs = detect(sql, kind);
    let mut vals = HashMap::new();
    vals.insert("d".to_string(), v(ParamType::Date, "2024-02-29"));
    let (send, bound) = bind(sql, &phs, &vals);
    assert_eq!(run(d, &send, bound).await, vec![vec!["1".to_string()]]);
}

#[tokio::test]
async fn sqlite_params() {
    let p = std::env::temp_dir().join(format!("wb-params-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&p);
    db_sqlite::create_database(&p).unwrap();
    let c = SavedConnection {
        env: Default::default(),
        kind: DbKind::Sqlite,
        id: "t".into(),
        name: "t".into(),
        host: String::new(),
        port: 0,
        user: String::new(),
        database: Some(p.display().to_string()),
        tls: Tls::Disabled,
        color: None,
        icon: None,
        read_only: false,
        ssh: None,
    };
    let rt = tokio::runtime::Handle::current();
    let d = AnyDriver::connect(&rt, &c, String::new(), Default::default())
        .await
        .unwrap();
    run(
        &d,
        "CREATE TABLE wb_test_params (id INTEGER PRIMARY KEY, name TEXT)",
        vec![],
    )
    .await;
    scenario(&d, DbKind::Sqlite, "wb_test_params").await;
    // SQLite also takes `@name` and `$name`.
    let sql = "SELECT @a + $b";
    let phs = detect(sql, DbKind::Sqlite);
    assert_eq!(names(&phs), ["a", "b"]);
    let mut vals = HashMap::new();
    vals.insert("a".to_string(), v(ParamType::Number, "2"));
    vals.insert("b".to_string(), v(ParamType::Number, "3"));
    let (send, bound) = bind(sql, &phs, &vals);
    assert_eq!(run(&d, &send, bound).await, vec![vec!["5".to_string()]]);
    drop(d);
    let _ = std::fs::remove_file(&p);
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

#[tokio::test]
async fn mariadb_params() {
    let (mut c, pw) = locus_core::store::parse_dsn(&dsn()).expect("BENCH_DSN");
    c.tls = Tls::Disabled;
    let rt = tokio::runtime::Handle::current();
    let d = AnyDriver::connect(&rt, &c, pw, Default::default())
        .await
        .expect("local MariaDB: set BENCH_DSN and start the server");
    run(&d, "DROP TABLE IF EXISTS wb_test_params", vec![]).await;
    run(
        &d,
        "CREATE TABLE wb_test_params (id INT PRIMARY KEY, name VARCHAR(40))",
        vec![],
    )
    .await;
    scenario(&d, DbKind::Mysql, "wb_test_params").await;
    // `?` style, with a number bound as text.
    let sql = "SELECT id FROM wb_test_params WHERE id > ? ORDER BY id";
    let phs = detect(sql, DbKind::Mysql);
    let mut vals = HashMap::new();
    vals.insert("?1".to_string(), v(ParamType::Number, "1"));
    let (send, bound) = bind(sql, &phs, &vals);
    assert_eq!(
        run(&d, &send, bound).await,
        vec![vec!["2".to_string()], vec!["3".to_string()]]
    );
    run(&d, "DROP TABLE wb_test_params", vec![]).await;
    assert!(
        run(
            &d,
            "SELECT COUNT(*) FROM information_schema.tables WHERE table_name = 'wb_test_params'",
            vec![]
        )
        .await
            == vec![vec!["0".to_string()]]
    );
}
