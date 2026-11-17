//! Live tests against the user's PostgreSQL test server (see tests/common).
//! READ ONLY on real objects: SELECT statements and catalog reads, every one capped at
//! 1000 rows; table contents are never printed (counts and column names
//! only). Writes happen only to `CREATE TEMP TABLE` inside a pinned session.
mod common;

use common::*;
use db_core::{DbObjectKind as K, Driver, ErrorKind, Outcome, Param, QueryEvent};
use std::time::{Duration, Instant};

macro_rules! live {
    () => {
        match connect().await {
            Some(d) => d,
            None => return,
        }
    };
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn select_types_scripts_and_errors() {
    let d = live!();
    println!(
        "server: {}",
        scalar(&d, "SELECT current_setting('server_version')").await
    );

    // text format values, NULL, bytea, several statements with one Columns / Affected each
    let g = ok(&d, "SELECT 1 AS a, 'x'::text AS b, NULL::int AS c, '\\x00ff10'::bytea AS d, now() IS NOT NULL AS e; SELECT 2; SELECT * FROM generate_series(1, 3) LIMIT 1000").await;
    assert_eq!(g.sets.len(), 3);
    assert_eq!(
        g.sets[0][0],
        vec![
            Some("1".into()),
            Some("x".into()),
            None,
            Some("bytes:3".into()),
            Some("t".into())
        ]
    );
    assert_eq!(g.columns[0][0].type_name, "int4");
    assert_eq!(g.sets[2].len(), 3);
    assert_ne!(g.session, 0);

    // a statement without a result set reports Affected, order is kept
    let g = ok(&d, "SELECT 1; SET LOCAL statement_timeout = 0; SELECT 2").await;
    assert_eq!((g.sets.len(), g.affected.len()), (2, 1));

    // an error in the middle keeps what came before, and the right kind / SQLSTATE
    let g = run(&d, "SELECT 1; SELECT * FROM dbw_no_such_table; SELECT 3").await;
    assert_eq!(g.sets.len(), 1);
    match g.outcome {
        Outcome::Failed(e) => assert!(
            e.kind == ErrorKind::Query && e.message.starts_with("[42P01]"),
            "{e}"
        ),
        o => panic!("{o:?}"),
    }
    assert_eq!(scalar(&d, "SELECT 1").await, "1", "the pool still works");

    // 1000 rows over several batches
    let mut h = d.query(
        "SELECT g, repeat('x', 50) FROM generate_series(1, 1000) g".into(),
        db_core::QueryOptions {
            batch_rows: 100,
            timeout: None,
        },
    );
    let g = collect(&mut h).await;
    assert!(
        matches!(g.outcome, Outcome::Done { rows: 1000 }),
        "{:?}",
        g.outcome
    );
    assert_eq!(g.sets[0].len(), 1000);
    d.kill_session(0); // a handle for an unknown pid must not panic
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_reads_and_origin() {
    let d = live!();
    let c = d.catalog().unwrap();
    let schemas = c.schemas().await.unwrap();
    assert!(
        schemas.iter().any(|s| s == "public") && !schemas.iter().any(|s| s.starts_with("pg_toast"))
    );
    let cur = c.current_schema().await.unwrap();
    println!("schemas: {}, current: {cur:?}", schemas.len());
    let objs = c.objects("public").await.unwrap();
    println!("public: {} tables/views", objs.len());
    let kinds = c.supported_kinds();
    assert!(kinds.contains(&K::Index) && kinds.contains(&K::MaterializedView));
    for k in &kinds {
        let l = c
            .list_objects("public", *k, 1000)
            .await
            .unwrap_or_else(|e| panic!("{k:?}: {e}"));
        println!("  {:<20} {}", k.label(), l.len());
    }
    let fks = c.foreign_keys("public").await.unwrap();
    println!("foreign keys: {}", fks.len());
    println!("principals: {}", c.principals(100).await.unwrap().len());
    let info = c.server_info(1000).await.unwrap();
    assert!(
        info.iter()
            .any(|i| i.group == "server" && i.name == "server_version")
    );
    assert!(
        info.iter()
            .any(|i| i.group == "charset" && i.name == "server_encoding")
    );
    println!("server info items: {}", info.len());

    let Some(first) = objs.iter().find(|o| o.kind == db_core::ObjectKind::Table) else {
        return;
    };
    let cols = c.columns("public", &first.name).await.unwrap();
    println!(
        "first table: {} columns, {} primary key columns",
        cols.len(),
        cols.iter().filter(|c| c.primary_key).count()
    );
    assert!(!cols.is_empty());
    let ddl = c.ddl("public", K::Table, &first.name).await.unwrap();
    assert!(ddl.starts_with("CREATE TABLE") && ddl.contains(&cols[0].name));
    let idx = c
        .children("public", &first.name, K::Index, 100)
        .await
        .unwrap();
    println!("first table: {} indexes", idx.len());
    if let Some(i) = idx.first() {
        let ddl = c.ddl("public", K::Index, &i.name).await.unwrap();
        assert!(ddl.starts_with("CREATE"), "index ddl");
    }
    c.children("public", &first.name, K::Trigger, 100)
        .await
        .unwrap();
    // functions: parameters and DDL of the first one
    let funcs = c.list_objects("public", K::Function, 50).await.unwrap();
    if let Some(f) = funcs.first() {
        let ps = c
            .routine_params("public", K::Function, &f.name, 100)
            .await
            .unwrap();
        let ddl = c.ddl("public", K::Function, &f.name).await.unwrap();
        println!(
            "first function: {} params (incl. result), ddl {} bytes",
            ps.len(),
            ddl.len()
        );
    }
    if let Some(v) = c.list_objects("public", K::View, 50).await.unwrap().first() {
        assert!(
            c.ddl("public", K::View, &v.name)
                .await
                .unwrap()
                .contains("VIEW")
        );
    }

    // origin: SELECT of the real table, capped; column names / counts only
    let q = format!(
        "SELECT * FROM {}.{} LIMIT 5",
        d.quote_identifier("public"),
        d.quote_identifier(&first.name)
    );
    let mut h = d.query(q.clone(), opts());
    let g = collect(&mut h).await;
    assert!(matches!(g.outcome, Outcome::Done { .. }), "{:?}", g.outcome);
    let m = &g.columns[0];
    assert_eq!(m.len(), cols.len());
    assert!(
        m.iter().all(|c| c
            .origin
            .as_ref()
            .is_some_and(|o| o.schema == "public" && o.table == first.name)),
        "origin missing"
    );
    assert!(
        m.iter()
            .zip(&cols)
            .all(|(a, b)| a.origin.as_ref().unwrap().column == b.name)
    );
    assert!(
        m.iter()
            .zip(&cols)
            .all(|(a, b)| a.nullable == Some(b.nullable))
    );
    println!(
        "origin resolved for all {} columns; single_table agrees: {:?}",
        m.len(),
        db_core::sql::single_table(&q).map(|t| t.table == first.name)
    );
    // an expression has no origin
    let g = ok(&d, "SELECT 1 + 1 AS two").await;
    assert!(g.columns[0][0].origin.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_execute_and_transactions_use_temp_tables_only() {
    let d = live!();
    let s = d.session().await.expect("session");
    let pid = s.session_id();
    assert_eq!(scalar(&s, "SELECT pg_backend_pid()").await, pid.to_string());
    ok(
        &s,
        "CREATE TEMP TABLE dbw_pg_tmp (id int PRIMARY KEY, name text, b bytea)",
    )
    .await;

    // bound values: hostile text is data; NULL; bytea
    let hostile = "x'); DROP TABLE dbw_pg_tmp; -- \\ \" ? $1";
    let ins = "INSERT INTO dbw_pg_tmp (id, name, b) VALUES (?, ?, ?)";
    let g = collect(&mut s.execute(
        ins.into(),
        vec![
            Param::Text("1".into()),
            Param::Text(hostile.into()),
            Param::Bytes(vec![0xff, 0, 0xfe]),
        ],
        opts(),
    ))
    .await;
    assert!(
        matches!(g.outcome, Outcome::Done { .. }) && g.affected == [1],
        "{:?} {:?}",
        g.outcome,
        g.affected
    );
    let g = collect(&mut s.execute(
        ins.into(),
        vec![Param::Text("2".into()), Param::Null, Param::Null],
        opts(),
    ))
    .await;
    assert_eq!(g.affected, [1]);
    let g = collect(&mut s.execute(
        "SELECT name, b, id FROM dbw_pg_tmp WHERE id = ?".into(),
        vec![Param::Text("1".into())],
        opts(),
    ))
    .await;
    assert!(
        matches!(g.outcome, Outcome::Done { rows: 1 }),
        "{:?}",
        g.outcome
    );
    assert_eq!(
        g.sets[0][0],
        vec![
            Some(hostile.to_string()),
            Some("bytes:3".into()),
            Some("1".into())
        ]
    );
    assert_eq!(g.columns[0][2].type_name, "int4");
    let g = collect(&mut s.execute(
        "SELECT name FROM dbw_pg_tmp WHERE id = ?".into(),
        vec![Param::Text("2".into())],
        opts(),
    ))
    .await;
    assert_eq!(g.sets[0][0], vec![None]);
    // UPDATE ... RETURNING with a parameter streams text rows too
    let g = collect(&mut s.execute(
        "UPDATE dbw_pg_tmp SET name = ? WHERE id = ? RETURNING id, name".into(),
        vec![Param::Text("new".into()), Param::Text("2".into())],
        opts(),
    ))
    .await;
    assert_eq!(g.sets[0][0], vec![Some("2".into()), Some("new".into())]);
    let g = collect(&mut s.execute(
        "DELETE FROM dbw_pg_tmp WHERE id = ?".into(),
        vec![Param::Text("2".into())],
        opts(),
    ))
    .await;
    assert_eq!(g.affected, [1]);
    // server error is a normal failure
    let g = collect(&mut s.execute(
        ins.into(),
        vec![Param::Text("1".into()), Param::Null, Param::Null],
        opts(),
    ))
    .await;
    match g.outcome {
        Outcome::Failed(e) => assert!(e.message.starts_with("[23505]"), "{e}"),
        o => panic!("{o:?}"),
    }
    assert_eq!(scalar(&s, "SELECT count(*) FROM dbw_pg_tmp").await, "1");

    // transactions: rollback keeps nothing, commit keeps the row
    ok(&s, "BEGIN").await;
    collect(&mut s.execute(
        ins.into(),
        vec![Param::Text("10".into()), Param::Null, Param::Null],
        opts(),
    ))
    .await;
    assert_eq!(scalar(&s, "SELECT count(*) FROM dbw_pg_tmp").await, "2");
    ok(&s, "ROLLBACK").await;
    assert_eq!(scalar(&s, "SELECT count(*) FROM dbw_pg_tmp").await, "1");
    ok(&s, "BEGIN; INSERT INTO dbw_pg_tmp (id) VALUES (11); COMMIT").await;
    assert_eq!(scalar(&s, "SELECT count(*) FROM dbw_pg_tmp").await, "2");

    // cancel keeps the session (and the temp table); the transaction is aborted by the server
    ok(&s, "BEGIN").await;
    let mut h = s.query("SELECT pg_sleep(30)".into(), opts());
    started(&mut h).await;
    h.cancel();
    assert!(matches!(drain(&mut h).await, Outcome::Cancelled));
    ok(&s, "ROLLBACK").await;
    assert_eq!(
        scalar(&s, "SELECT count(*) FROM dbw_pg_tmp").await,
        "2",
        "same connection, temp table intact"
    );

    // the temp table is session-local: it is gone on another connection and after drop
    assert!(matches!(
        run(&d, "SELECT count(*) FROM dbw_pg_tmp").await.outcome,
        Outcome::Failed(_)
    ));
    drop(s);
    let t0 = Instant::now();
    loop {
        let n = scalar(
            &d,
            &format!("SELECT count(*) FROM pg_stat_activity WHERE pid = {pid}"),
        )
        .await;
        if n == "0" {
            println!("session connection gone {:?} after drop", t0.elapsed());
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "session connection still on the server"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // rollback on drop: an open transaction inside a session leaves nothing behind
    let s2 = d.session().await.unwrap();
    ok(&s2, "CREATE TEMP TABLE dbw_pg_tmp2 (id int)").await;
    ok(&s2, "BEGIN; INSERT INTO dbw_pg_tmp2 VALUES (1)").await;
    drop(s2);
}

async fn started(h: &mut db_core::QueryHandle) -> u64 {
    match tokio::time::timeout(Duration::from_secs(10), h.next())
        .await
        .expect("no event")
    {
        Some(QueryEvent::Started { session_id }) => session_id,
        o => panic!("expected Started, got {o:?}"),
    }
}

async fn drain(h: &mut db_core::QueryHandle) -> Outcome {
    loop {
        match tokio::time::timeout(Duration::from_secs(30), h.next())
            .await
            .expect("no event in 30 s")
        {
            Some(QueryEvent::Finished(o)) => return o,
            Some(_) => {}
            None => panic!("no Finished"),
        }
    }
}

async fn active(d: &impl Driver, pid: u64) -> bool {
    // `d` must be a pinned observer session: a pooled connection could be the very backend being checked.
    scalar(
        d,
        &format!("SELECT count(*) FROM pg_stat_activity WHERE pid = {pid} AND state = 'active'"),
    )
    .await
        != "0"
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_is_fast_and_the_backend_is_idle_afterwards() {
    let d = live!();
    let obs = d.session().await.unwrap();
    let mut samples = Vec::new();
    for _ in 0..5 {
        let mut h = d.query("SELECT pg_sleep(30)".into(), opts());
        let pid = started(&mut h).await;
        // wait until the server really runs it
        let t0 = Instant::now();
        while !active(&obs, pid).await {
            assert!(t0.elapsed() < Duration::from_secs(5));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let t = Instant::now();
        h.cancel();
        let o = drain(&mut h).await;
        let took = t.elapsed();
        assert!(matches!(o, Outcome::Cancelled), "{o:?}");
        assert!(
            !active(&obs, pid).await,
            "statement still active on the server"
        );
        samples.push(took.as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "[cancel] pg_sleep(30): p50 {:.1} ms / max {:.1} ms / min {:.1} ms (n=5), pg_stat_activity shows it gone each time: {samples:.1?}",
        samples[2], samples[4], samples[0]
    );
    assert!(samples[4] < 2000.0);
    // the connection is reusable straight away
    assert_eq!(scalar(&d, "SELECT 1").await, "1");

    // kill_session = pg_cancel_backend on the control connection
    let mut h = d.query("SELECT pg_sleep(30)".into(), opts());
    let pid = started(&mut h).await;
    while !active(&obs, pid).await {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let t = Instant::now();
    let k = collect(&mut d.kill_session(pid)).await;
    assert!(matches!(k.outcome, Outcome::Done { .. }), "{:?}", k.outcome);
    let o = drain(&mut h).await;
    println!(
        "[kill_session] pg_cancel_backend -> query ended {o:?} after {:?}",
        t.elapsed()
    );
    assert!(
        matches!(o, Outcome::Failed(ref e) if e.message.starts_with("[57014]")),
        "{o:?}"
    );

    // timeout
    let t = Instant::now();
    let mut h = d.query(
        "SELECT pg_sleep(30)".into(),
        db_core::QueryOptions {
            batch_rows: 1000,
            timeout: Some(Duration::from_millis(300)),
        },
    );
    let o = drain(&mut h).await;
    println!("[timeout] 300 ms limit -> {o:?} after {:?}", t.elapsed());
    assert!(matches!(o, Outcome::Failed(ref e) if e.kind == ErrorKind::Timeout));
    assert!(t.elapsed() < Duration::from_secs(3));

    // dropping the handle cancels the statement
    let h = d.query("SELECT pg_sleep(30)".into(), opts());
    let mut h = h;
    let pid = started(&mut h).await;
    while !active(&obs, pid).await {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    drop(h);
    let t = Instant::now();
    while active(&obs, pid).await {
        assert!(
            t.elapsed() < Duration::from_secs(3),
            "a dropped handle left the statement running"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    println!("[drop handle] statement gone after {:?}", t.elapsed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn back_pressure_stops_the_server() {
    let d = live!();
    let obs = d.session().await.unwrap();
    // ~100 MB generated by the server (no table is read); batches of 1 row, nobody reads.
    let mut h = d.query(
        "SELECT g, repeat('x', 100000) FROM generate_series(1, 1000) g".into(),
        db_core::QueryOptions {
            batch_rows: 1,
            timeout: None,
        },
    );
    let pid = started(&mut h).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let wait = scalar(
        &obs,
        &format!("SELECT coalesce(wait_event, '-') FROM pg_stat_activity WHERE pid = {pid}"),
    )
    .await;
    println!("[back-pressure] unread consumer: server wait_event = {wait}");
    assert_eq!(
        wait, "ClientWrite",
        "the server must be blocked writing to a client that does not read"
    );
    // now consume a few and cancel
    for _ in 0..8 {
        h.next().await;
    }
    h.cancel();
    assert!(matches!(drain(&mut h).await, Outcome::Cancelled));
    assert!(!active(&obs, pid).await);
    let _ = Param::Null;
}
