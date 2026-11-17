//! Multiple result sets, bound parameters, sessions, origin and the catalog.
//! Needs the local MariaDB (BENCH_DSN). Tables of these tests start with `dbw_`.

mod common;

use common::*;
use db_core::{
    Catalog, Cell, ColumnOrigin, Driver, ErrorKind, ObjectKind, Outcome, Param, QueryEvent,
    QueryOptions,
};
use db_mysql::MysqlDriver;
use std::time::{Duration, Instant};

fn run_ok(driver: &impl Driver, sql: &str) {
    let mut h = driver.query(sql.into(), QueryOptions::default());
    let got = collect(&mut h);
    assert!(
        matches!(got.outcome, Outcome::Done { .. }),
        "{sql}: {:?}",
        got.outcome
    );
}

fn text(got: &Collected, set: usize, row: usize, col: usize) -> String {
    match got.set(set).1.cell(row, col) {
        Cell::Text(s) => s.to_string(),
        Cell::Null => "<NULL>".into(),
        Cell::Bytes(b) => format!("<{} bytes: {b:?}>", b.len()),
    }
}

fn select(driver: &impl Driver, sql: &str) -> Collected {
    let mut h = driver.query(sql.into(), QueryOptions::default());
    let got = collect(&mut h);
    assert!(
        matches!(got.outcome, Outcome::Done { .. }),
        "{sql}: {:?}",
        got.outcome
    );
    got
}

fn exec(driver: &impl Driver, sql: &str, params: Vec<Param>) -> Collected {
    collect(&mut driver.execute(sql.into(), params, QueryOptions::default()))
}

// ---- multiple result sets ----

#[test]
fn two_result_sets_each_send_their_columns() {
    let driver = connect();
    let got = select(&driver, "SELECT 1; SELECT 'a','b'");
    assert_eq!(got.order, [Shape::Set(0), Shape::Set(1)]);
    assert!(
        matches!(got.outcome, Outcome::Done { rows: 2 }),
        "{:?}",
        got.outcome
    );
    assert_eq!(got.set(0).0.len(), 1);
    assert_eq!(got.set(1).0.len(), 2);
    assert_eq!(text(&got, 0, 0, 0), "1");
    assert_eq!(
        (text(&got, 1, 0, 0), text(&got, 1, 0, 1)),
        ("a".into(), "b".into())
    );
    assert_eq!(got.set(1).0[1].name, "b");
}

#[test]
fn script_reports_affected_rows_per_statement() {
    let driver = connect();
    let got = select(
        &driver,
        "CREATE TEMPORARY TABLE dbw_tmp (a INT); INSERT INTO dbw_tmp VALUES (1),(2),(3); \
         UPDATE dbw_tmp SET a = a + 10 WHERE a < 3; SELECT a FROM dbw_tmp ORDER BY a",
    );
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
        (text(&got, 0, 0, 0), text(&got, 0, 2, 0)),
        ("3".into(), "12".into())
    );
    // A result set between two statements without one keeps its place.
    let got = select(&driver, "SELECT 1; DO 0; SELECT 2");
    assert_eq!(
        got.order,
        [Shape::Set(0), Shape::Affected(0), Shape::Set(1)]
    );
}

#[test]
fn call_with_two_result_sets() {
    let driver = connect();
    run_ok(&driver, "DROP PROCEDURE IF EXISTS bench.dbw_two");
    run_ok(
        &driver,
        "CREATE PROCEDURE bench.dbw_two() BEGIN SELECT 1 AS x; SELECT 'a' AS y, 'b' AS z; END",
    );
    let got = select(&driver, "CALL bench.dbw_two()");
    // The OK packet that closes a CALL is not a statement of its own.
    assert_eq!(got.order, [Shape::Set(0), Shape::Set(1)]);
    assert_eq!(got.set(0).0.len(), 1);
    assert_eq!(got.set(1).0.len(), 2);
    assert_eq!(text(&got, 1, 0, 1), "b");
    // The connection is clean afterwards and a following statement is reported.
    let got = select(&driver, "CALL bench.dbw_two(); SELECT 3");
    assert_eq!(got.order, [Shape::Set(0), Shape::Set(1), Shape::Set(2)]);
    run_ok(&driver, "DROP PROCEDURE bench.dbw_two");
}

#[test]
fn call_without_result_set_reports_affected() {
    let driver = connect();
    run_ok(&driver, "DROP PROCEDURE IF EXISTS bench.dbw_none");
    run_ok(&driver, "CREATE PROCEDURE bench.dbw_none() BEGIN DO 1; END");
    let got = select(&driver, "CALL bench.dbw_none()");
    assert_eq!(got.order.len(), 1);
    assert!(
        matches!(got.order[0], Shape::Affected(_)),
        "{:?}",
        got.order
    );
    run_ok(&driver, "DROP PROCEDURE bench.dbw_none");
}

#[test]
fn error_in_second_statement_keeps_the_first_result() {
    let driver = connect();
    let mut h = driver.query(
        "SELECT 1; SELECT * FROM bench.dbw_no_such_table; SELECT 3".into(),
        QueryOptions::default(),
    );
    let got = collect(&mut h);
    assert_eq!(got.order, [Shape::Set(0)]);
    assert_eq!(text(&got, 0, 0, 0), "1");
    match &got.outcome {
        Outcome::Failed(e) => {
            assert_eq!(e.kind, ErrorKind::Query);
            assert_eq!(e.code, Some(1146));
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    // A failing statement after an affected-only one.
    let got = collect(&mut driver.query("DO 0; SELEC".into(), QueryOptions::default()));
    assert_eq!(got.order, [Shape::Affected(0)]);
    assert!(
        matches!(got.outcome, Outcome::Failed(ref e) if e.kind == ErrorKind::Query),
        "{:?}",
        got.outcome
    );
    // The driver is fine afterwards.
    select(&driver, "SELECT 1");
}

#[test]
fn cancel_during_second_statement() {
    let driver = connect();
    let obs = Observer::new();
    for _ in 0..3 {
        let mut h = driver.query("SELECT 1; SELECT SLEEP(30)".into(), QueryOptions::default());
        let id = match h.blocking_next() {
            Some(QueryEvent::Started { session_id }) => session_id,
            other => panic!("expected Started, got {other:?}"),
        };
        // First result set arrives, then the server is inside SLEEP.
        let mut sets = 0;
        while sets < 1 {
            match h.blocking_next() {
                Some(QueryEvent::Columns(_)) => sets += 1,
                other => panic!("expected the first result set, got {other:?}"),
            }
        }
        match h.blocking_next() {
            Some(QueryEvent::Batch(b)) => assert_eq!(b.rows(), 1),
            other => {
                panic!("expected the first batch before the second statement ends, got {other:?}")
            }
        }
        std::thread::sleep(Duration::from_millis(300));
        obs.wait_running(id);
        let watcher = obs.watch_gone(id);
        let t0 = Instant::now();
        h.cancel();
        let outcome = finish(&mut h);
        assert!(matches!(outcome, Outcome::Cancelled), "{outcome:?}");
        assert!(
            t0.elapsed() < lim(Duration::from_secs(2)),
            "cancel took {:?}",
            t0.elapsed()
        );
        join_gone(watcher);
        assert_eq!(obs.running(id), 0, "the statement is still on the server");
        select(&driver, "SELECT 1");
    }
}

#[test]
fn timeout_across_result_sets() {
    let driver = connect();
    let t = Instant::now();
    let opts = QueryOptions {
        timeout: Some(Duration::from_millis(700)),
        ..Default::default()
    };
    let got = collect(&mut driver.query("SELECT 1; SELECT SLEEP(30); SELECT 2".into(), opts));
    assert_eq!(got.order, [Shape::Set(0)]);
    assert!(
        matches!(got.outcome, Outcome::Failed(ref e) if e.kind == ErrorKind::Timeout),
        "{:?}",
        got.outcome
    );
    assert!(
        t.elapsed() < lim(Duration::from_secs(3)),
        "{:?}",
        t.elapsed()
    );
    select(&driver, "SELECT 1");
}

#[test]
fn back_pressure_and_cancel_in_the_second_result_set() {
    let driver = connect();
    let obs = Observer::new();
    // Full read of both sets.
    let got = select(&driver, &format!("SELECT 1; {BIG_QUERY}; SELECT 'end'"));
    assert_eq!(got.set_count(), 3);
    assert_eq!(got.set(1).1.rows(), BIG_ROWS as usize);
    assert!(
        matches!(got.outcome, Outcome::Done { rows } if rows == BIG_ROWS + 2),
        "{:?}",
        got.outcome
    );
    assert_eq!(text(&got, 2, 0, 0), "end");

    // Stop reading in the middle of the second set: the query waits, then the cancel stops it.
    let mut h = driver.query(format!("SELECT 1; {BIG_QUERY}"), QueryOptions::default());
    let id = match h.blocking_next() {
        Some(QueryEvent::Started { session_id }) => session_id,
        other => panic!("{other:?}"),
    };
    let (mut columns, mut batches) = (0, 0);
    while columns < 2 || batches < 3 {
        match h.blocking_next() {
            Some(QueryEvent::Columns(_)) => columns += 1,
            Some(QueryEvent::Batch(_)) => batches += 1,
            other => panic!("{other:?}"),
        }
    }
    std::thread::sleep(Duration::from_millis(500)); // the driver is blocked on the full window
    obs.wait_running(id);
    h.cancel();
    assert!(matches!(finish(&mut h), Outcome::Cancelled));
    assert_eq!(obs.running(id), 0);
    select(&driver, "SELECT 1");
}

// ---- origin ----

#[test]
fn column_origin_from_the_definition_packet() {
    let driver = connect();
    let got = select(
        &driver,
        "SELECT name AS n, CONCAT(name, 0) FROM student_real s LIMIT 1",
    );
    assert_eq!(got.columns[0].name, "n");
    assert_eq!(
        got.columns[0].origin,
        Some(ColumnOrigin {
            schema: "bench".into(),
            table: "student_real".into(),
            column: "name".into()
        })
    );
    assert_eq!(got.columns[1].origin, None);
    // Constants, aggregates and derived tables have no origin either.
    let got = select(
        &driver,
        "SELECT 1, COUNT(*), d.name FROM (SELECT name FROM student_real LIMIT 1) d",
    );
    assert_eq!(got.columns[0].origin, None);
    assert_eq!(got.columns[1].origin, None);
    println!(
        "[origin] column of a derived table: {:?}",
        got.columns[2].origin
    );
}

// ---- bound parameters ----

fn setup_exec_table(driver: &MysqlDriver, name: &str) {
    run_ok(driver, &format!("DROP TABLE IF EXISTS {name}"));
    run_ok(
        driver,
        &format!(
            "CREATE TABLE {name} (id INT PRIMARY KEY, name VARCHAR(100) CHARACTER SET utf8mb4, note TEXT CHARACTER SET utf8mb4, \
             raw BLOB)"
        ),
    );
    run_ok(
        driver,
        &format!(
            "INSERT INTO {name} (id, name, note) VALUES (1, 'a', 'x'), (2, 'b', 'y'), (3, 'c', 'z')"
        ),
    );
}

#[test]
fn execute_updates_with_cjk_and_null_parameters() {
    let driver = connect();
    setup_exec_table(&driver, "dbw_exec1");
    let got = exec(
        &driver,
        "UPDATE dbw_exec1 SET name = ?, note = ? WHERE id IN (?, ?)",
        vec![
            Param::Text("你好，世界 😀".into()),
            Param::Null,
            Param::Text("1".into()),
            Param::Text("2".into()),
        ],
    );
    assert_eq!(got.order, [Shape::Affected(2)]);
    assert!(
        matches!(got.outcome, Outcome::Done { rows: 0 }),
        "{:?}",
        got.outcome
    );
    let back = select(&driver, "SELECT id, name, note FROM dbw_exec1 ORDER BY id");
    assert_eq!(text(&back, 0, 0, 1), "你好，世界 😀");
    assert_eq!(text(&back, 0, 0, 2), "<NULL>");
    assert_eq!(text(&back, 0, 1, 1), "你好，世界 😀");
    assert_eq!(
        (text(&back, 0, 2, 1), text(&back, 0, 2, 2)),
        ("c".into(), "z".into())
    );
    run_ok(&driver, "DROP TABLE dbw_exec1");
}

#[test]
fn execute_stores_quotes_backslashes_semicolons_and_bytes_verbatim() {
    let driver = connect();
    setup_exec_table(&driver, "dbw_exec2");
    let nasty = "it's a \"test\" \\ back\\slash; DROP TABLE dbw_exec2; -- `x` \\' ''";
    let blob = vec![0u8, 255, 1, b'\'', b'\\', 0];
    let got = exec(
        &driver,
        "INSERT INTO dbw_exec2 (id, name, note, raw) VALUES (?, ?, ?, ?)",
        vec![
            Param::Text("10".into()),
            Param::Text(nasty.into()),
            Param::Text(nasty.repeat(50)),
            Param::Bytes(blob.clone()),
        ],
    );
    assert_eq!(got.order, [Shape::Affected(1)]);
    let back = select(
        &driver,
        "SELECT name, note, raw FROM dbw_exec2 WHERE id = 10",
    );
    assert_eq!(text(&back, 0, 0, 0), nasty);
    assert_eq!(text(&back, 0, 0, 1), nasty.repeat(50));
    match back.rows.cell(0, 2) {
        Cell::Bytes(b) => assert_eq!(b, &blob[..]),
        other => panic!("expected bytes, got {other:?}"),
    }
    // The table is still there: nothing was spliced into the SQL.
    select(&driver, "SELECT COUNT(*) FROM dbw_exec2");
    run_ok(&driver, "DROP TABLE dbw_exec2");
}

#[test]
fn execute_with_the_wrong_parameter_count_is_a_query_error() {
    let driver = connect();
    for (sql, params) in [
        ("SELECT ?, ?", vec![Param::Text("1".into())]),
        ("SELECT 1", vec![Param::Text("1".into())]),
        ("SELECT ?", vec![]),
    ] {
        let got = exec(&driver, sql, params);
        match got.outcome {
            Outcome::Failed(e) => assert_eq!(e.kind, ErrorKind::Query, "{sql}: {e}"),
            other => panic!("{sql}: expected Failed(Query), got {other:?}"),
        }
    }
    select(&driver, "SELECT 1");
}

#[test]
fn execute_select_returns_a_normal_result_set() {
    let driver = connect();
    let got = exec(
        &driver,
        "SELECT ?, 42, 1.50, CAST('2024-01-02' AS DATE), CAST('2024-01-02 03:04:05' AS DATETIME), CAST('-12:34:56' AS TIME), 2.5e0, NULL",
        vec![Param::Text("列".into())],
    );
    assert!(
        matches!(got.outcome, Outcome::Done { rows: 1 }),
        "{:?}",
        got.outcome
    );
    let cells: Vec<String> = (0..8).map(|c| text(&got, 0, 0, c)).collect();
    assert_eq!(
        cells,
        [
            "列",
            "42",
            "1.50",
            "2024-01-02",
            "2024-01-02 03:04:05",
            "-12:34:56",
            "2.5",
            "<NULL>"
        ]
    );
}

#[test]
fn execute_cancel_stops_the_statement_on_the_server() {
    let driver = connect();
    let obs = Observer::new();
    let mut h = driver.execute(
        "SELECT SLEEP(?)".into(),
        vec![Param::Text("30".into())],
        QueryOptions::default(),
    );
    let id = match h.blocking_next() {
        Some(QueryEvent::Started { session_id }) => session_id,
        other => panic!("{other:?}"),
    };
    obs.wait_running(id);
    h.cancel();
    assert!(matches!(finish(&mut h), Outcome::Cancelled));
    assert_eq!(obs.running(id), 0);
    // And a timeout.
    let opts = QueryOptions {
        timeout: Some(Duration::from_millis(500)),
        ..Default::default()
    };
    let got = collect(&mut driver.execute(
        "SELECT SLEEP(?)".into(),
        vec![Param::Text("30".into())],
        opts,
    ));
    assert!(
        matches!(got.outcome, Outcome::Failed(ref e) if e.kind == ErrorKind::Timeout),
        "{:?}",
        got.outcome
    );
}

// ---- session / transaction ----

#[test]
fn transaction_on_one_session_rolls_back() {
    let driver = connect();
    setup_exec_table(&driver, "dbw_tx");
    run_ok(&driver, "ALTER TABLE dbw_tx ENGINE = InnoDB");
    let session = block_on(driver.session()).expect("session");

    let first = select(&session, "START TRANSACTION");
    let sid = first.session_id;
    assert_eq!(Some(sid), session.session_id());
    let a = exec(
        &session,
        "UPDATE dbw_tx SET name = ? WHERE id = ?",
        vec![Param::Text("changed1".into()), Param::Text("1".into())],
    );
    let b = exec(
        &session,
        "UPDATE dbw_tx SET name = ? WHERE id = ?",
        vec![Param::Text("changed2".into()), Param::Text("2".into())],
    );
    assert_eq!(
        (a.order.as_slice(), b.order.as_slice()),
        (
            [Shape::Affected(1)].as_slice(),
            [Shape::Affected(1)].as_slice()
        )
    );
    assert_eq!(
        (a.session_id, b.session_id),
        (sid, sid),
        "every statement runs on the same server connection"
    );

    // Visible inside the transaction, invisible outside.
    let inside = select(&session, "SELECT name FROM dbw_tx ORDER BY id");
    assert_eq!(
        (text(&inside, 0, 0, 0), text(&inside, 0, 1, 0)),
        ("changed1".into(), "changed2".into())
    );
    let outside = select(&driver, "SELECT name FROM dbw_tx ORDER BY id");
    assert_eq!(
        (text(&outside, 0, 0, 0), text(&outside, 0, 1, 0)),
        ("a".into(), "b".into())
    );

    select(&session, "ROLLBACK");
    let after = select(&driver, "SELECT name FROM dbw_tx ORDER BY id");
    assert_eq!(
        (
            text(&after, 0, 0, 0),
            text(&after, 0, 1, 0),
            text(&after, 0, 2, 0)
        ),
        ("a".into(), "b".into(), "c".into())
    );

    // Commit makes it visible; a dropped session with an open transaction rolls back.
    select(&session, "START TRANSACTION");
    exec(
        &session,
        "UPDATE dbw_tx SET name = ? WHERE id = 3",
        vec![Param::Text("kept".into())],
    );
    select(&session, "COMMIT");
    assert_eq!(
        text(
            &select(&driver, "SELECT name FROM dbw_tx WHERE id = 3"),
            0,
            0,
            0
        ),
        "kept"
    );
    select(&session, "START TRANSACTION");
    exec(
        &session,
        "UPDATE dbw_tx SET name = ? WHERE id = 3",
        vec![Param::Text("lost".into())],
    );
    drop(session);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        text(
            &select(&driver, "SELECT name FROM dbw_tx WHERE id = 3"),
            0,
            0,
            0
        ),
        "kept"
    );
    let obs = Observer::new();
    assert!(
        !obs.session_exists(sid),
        "the session's connection should be closed"
    );
    run_ok(&driver, "DROP TABLE dbw_tx");
}

#[test]
fn session_cancel_keeps_the_connection() {
    let driver = connect();
    let session = block_on(driver.session()).expect("session");
    select(&session, "SET @dbw_var = 7");
    let mut h = session.query("SELECT SLEEP(30)".into(), QueryOptions::default());
    std::thread::sleep(Duration::from_millis(200));
    h.cancel();
    assert!(matches!(finish(&mut h), Outcome::Cancelled));
    // Same connection, session variable still there.
    assert_eq!(text(&select(&session, "SELECT @dbw_var"), 0, 0, 0), "7");
}

// ---- catalog ----

fn cat(driver: &MysqlDriver) -> &dyn Catalog {
    driver.catalog().expect("mysql has a catalog")
}

#[test]
fn catalog_schemas_objects_and_current_schema() {
    let driver = connect();
    let schemas = block_on(cat(&driver).schemas()).unwrap();
    assert!(
        schemas.iter().any(|s| s == "bench") && schemas.iter().any(|s| s == "information_schema"),
        "{schemas:?}"
    );
    assert_eq!(
        block_on(cat(&driver).current_schema()).unwrap().as_deref(),
        Some("bench")
    );

    run_ok(&driver, "DROP VIEW IF EXISTS dbw_view");
    run_ok(
        &driver,
        "CREATE VIEW dbw_view AS SELECT name FROM student_real",
    );
    let objects = block_on(cat(&driver).objects("bench")).unwrap();
    let kind = |n: &str| objects.iter().find(|o| o.name == n).map(|o| o.kind);
    assert_eq!(kind("dbw_view"), Some(ObjectKind::View));
    assert_eq!(kind("student_real"), Some(ObjectKind::Table));
    assert_eq!(kind("student"), Some(ObjectKind::Table));
    let names: Vec<_> = objects.iter().map(|o| o.name.to_lowercase()).collect();
    assert!(
        names.windows(2).all(|w| w[0] <= w[1]),
        "not sorted: {names:?}"
    );
    let cols = block_on(cat(&driver).columns("bench", "dbw_view")).unwrap();
    assert_eq!(cols.len(), 1);
    run_ok(&driver, "DROP VIEW dbw_view");

    // Unknown schema / object: empty, not an error. Names are parameters.
    assert!(
        block_on(cat(&driver).objects("no_such_schema_dbw"))
            .unwrap()
            .is_empty()
    );
    assert!(
        block_on(cat(&driver).columns("bench", "no_such_table_dbw"))
            .unwrap()
            .is_empty()
    );
    assert!(
        block_on(cat(&driver).objects("bench' OR '1'='1"))
            .unwrap()
            .is_empty()
    );
    assert!(
        block_on(cat(&driver).foreign_keys("no_such_schema_dbw"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn catalog_columns_of_student_real() {
    let driver = connect();
    let header: Vec<String> = student_tsv()
        .lines()
        .next()
        .unwrap()
        .split('\t')
        .map(str::to_string)
        .collect();
    let cols = block_on(cat(&driver).columns("bench", "student_real")).unwrap();
    assert_eq!(cols.len(), 23);
    assert_eq!(
        cols.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
        header
    );
    let desc = cols.iter().find(|c| c.name == "desc").unwrap();
    assert!(desc.type_name.to_lowercase().contains("text"), "{desc:?}");
    println!(
        "[catalog] student_real: {:?}",
        cols.iter()
            .map(|c| (&c.name, &c.type_name, c.nullable, c.primary_key))
            .collect::<Vec<_>>()
    );
}

#[test]
fn catalog_primary_key_and_nullable() {
    let driver = connect();
    run_ok(&driver, "DROP TABLE IF EXISTS dbw_cols");
    run_ok(
        &driver,
        "CREATE TABLE dbw_cols (a INT NOT NULL, b VARCHAR(191) NULL, c DECIMAL(10,2) NOT NULL, PRIMARY KEY (c, a))",
    );
    let cols = block_on(cat(&driver).columns("bench", "dbw_cols")).unwrap();
    let got: Vec<_> = cols
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
        got,
        [
            ("a", "int(11)", false, true),
            ("b", "varchar(191)", true, false),
            ("c", "decimal(10,2)", false, true)
        ]
    );
    run_ok(&driver, "DROP TABLE dbw_cols");
}

#[test]
fn catalog_works_while_a_query_streams() {
    let driver = connect();
    let mut h = driver.query(BIG_QUERY.into(), QueryOptions::default());
    // Let the driver fill its window and block on the connection.
    for _ in 0..3 {
        loop {
            if let Some(QueryEvent::Batch(_)) = h.blocking_next() {
                break;
            }
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    let t = Instant::now();
    let objects = block_on(cat(&driver).objects("bench")).unwrap();
    assert!(!objects.is_empty());
    assert_eq!(
        block_on(cat(&driver).columns("bench", "student_real"))
            .unwrap()
            .len(),
        23
    );
    println!(
        "[catalog] two catalog calls while a query streams: {:?}",
        t.elapsed()
    );
    assert!(t.elapsed() < lim(Duration::from_secs(2)));
    h.cancel();
    assert!(matches!(finish(&mut h), Outcome::Cancelled));
}

#[test]
fn catalog_foreign_keys_group_multi_column_keys_in_key_order() {
    let driver = connect();
    run_ok(&driver, "DROP TABLE IF EXISTS dbw_fk_child");
    run_ok(&driver, "DROP TABLE IF EXISTS dbw_fk_parent");
    run_ok(
        &driver,
        "CREATE TABLE dbw_fk_parent (a INT NOT NULL, b INT NOT NULL, PRIMARY KEY (b, a), UNIQUE KEY (a)) ENGINE=InnoDB",
    );
    run_ok(
        &driver,
        "CREATE TABLE dbw_fk_child (id INT PRIMARY KEY, x INT NOT NULL, y INT NOT NULL, \
         CONSTRAINT dbw_fk FOREIGN KEY (y, x) REFERENCES dbw_fk_parent (b, a), \
         CONSTRAINT dbw_fk0 FOREIGN KEY (x) REFERENCES dbw_fk_parent (a)) ENGINE=InnoDB",
    );
    let keys: Vec<_> = block_on(cat(&driver).foreign_keys("bench"))
        .unwrap()
        .into_iter()
        .filter(|k| k.table.starts_with("dbw_fk_"))
        .collect();
    run_ok(&driver, "DROP TABLE dbw_fk_child");
    run_ok(&driver, "DROP TABLE dbw_fk_parent");
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert_eq!(keys[0].name, "dbw_fk");
    assert_eq!(keys[0].table, "dbw_fk_child");
    assert_eq!(keys[0].columns, ["y", "x"]);
    assert_eq!(keys[0].ref_schema, "bench");
    assert_eq!(keys[0].ref_table, "dbw_fk_parent");
    assert_eq!(keys[0].ref_columns, ["b", "a"]);
    assert_eq!(keys[1].name, "dbw_fk0");
    assert_eq!(keys[1].columns, ["x"]);
    assert_eq!(keys[1].ref_columns, ["a"]);
}
