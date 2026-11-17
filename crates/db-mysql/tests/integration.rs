//! Integration tests against the local MariaDB (BENCH_DSN). Run with
//! `cargo test -p db-mysql -- --nocapture --test-threads=1`.

mod common;

use common::*;
use db_core::Driver;
use db_core::{Cell, ErrorKind, Outcome, QueryEvent, QueryOptions, TlsMode};
use db_mysql::MysqlDriver;
use std::time::{Duration, Instant};

const RUNS: usize = 5;

fn tsv() -> Vec<Vec<String>> {
    let text = student_tsv();
    text.lines()
        .map(|l| l.split('\t').map(str::to_string).collect())
        .collect()
}

fn is_cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

#[test]
fn full_stream() {
    let driver = connect();
    let file = tsv();
    let header = &file[0];
    let data = &file[1..];
    assert_eq!(data.len(), 40_450);

    let (mut secs, mut rates) = (vec![], vec![]);
    for run in 0..RUNS {
        let t = Instant::now();
        let mut h = driver.query("SELECT * FROM student_real".into(), QueryOptions::default());
        let got = collect(&mut h);
        let elapsed = t.elapsed();
        assert!(
            matches!(got.outcome, Outcome::Done { rows: 40_450 }),
            "run {run}: {:?}",
            got.outcome
        );
        assert_eq!(got.rows.rows(), 40_450);
        assert_eq!(got.columns.len(), 23);
        assert!(got.session_id > 0);
        secs.push(ms(elapsed));
        rates.push(40_450.0 / elapsed.as_secs_f64());
        if run > 0 {
            continue;
        }

        // Metadata.
        let names: Vec<&str> = got.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, header.iter().map(String::as_str).collect::<Vec<_>>());
        let desc = got.columns.iter().find(|c| c.name == "desc").unwrap();
        assert_eq!(desc.type_name, "TEXT");
        assert_eq!(desc.nullable, Some(false));
        assert_eq!(got.columns[0].type_name, "VARCHAR");
        // Batching: 1000 rows each, the remainder last.
        assert!(
            got.batch_sizes[..got.batch_sizes.len() - 1]
                .iter()
                .all(|n| *n == 1000),
            "{:?}",
            got.batch_sizes
        );
        assert_eq!(got.batch_sizes.iter().sum::<usize>(), 40_450);

        // Every cell equals the TSV (the table was loaded in file order).
        let mut cjk_cells = 0;
        let mut empty_cells = 0;
        for (r, line) in data.iter().enumerate() {
            for (c, want) in line.iter().enumerate() {
                match got.rows.cell(r, c) {
                    Cell::Text(have) => {
                        assert_eq!(have, want, "row {r} col {} ({})", c, header[c]);
                        cjk_cells += usize::from(is_cjk(have));
                        empty_cells += usize::from(have.is_empty());
                    }
                    other => panic!("row {r} col {c}: expected text, got {other:?}"),
                }
            }
        }
        assert!(
            cjk_cells > 40_000,
            "expected CJK text in the data, found {cjk_cells} cells"
        );
        println!(
            "[full_stream] verified 40450 x 23 = {} cells against the TSV ({cjk_cells} CJK cells, {empty_cells} empty strings)",
            40_450 * 23
        );
        let r = 12345;
        println!(
            "[full_stream] spot check row {r}: name={:?} address={:?}",
            got.rows.cell(r, 5),
            got.rows.cell(r, 13)
        );
    }
    report(
        "full_stream",
        "SELECT * FROM student_real wall time",
        &secs,
        "ms",
    );
    let (rate_p50, _) = report("full_stream", "rows/s", &rates, "rows/s");
    assert!(
        rate_p50 > 50_000.0 / slack(),
        "unexpectedly slow: {rate_p50:.0} rows/s"
    );
}

#[test]
fn null_empty_and_types() {
    let driver = connect();
    let mut h = driver.query(
        "SELECT NULL AS n, '' AS e, 'x' AS t, 42 AS i, CAST(-7 AS UNSIGNED) AS u, 1.50 AS d, '2024-01-02 03:04:05' AS s, \
         CAST('2024-01-02' AS DATE) AS dt, CAST('12:34:56' AS TIME) AS tm, CAST('列' AS BINARY) AS b"
            .into(),
        QueryOptions::default(),
    );
    let got = collect(&mut h);
    assert!(
        matches!(got.outcome, Outcome::Done { rows: 1 }),
        "{:?}",
        got.outcome
    );
    assert_eq!(got.rows.cell(0, 0), Cell::Null);
    assert_eq!(got.rows.cell(0, 1), Cell::Text(""));
    assert_eq!(got.rows.cell(0, 2), Cell::Text("x"));
    assert_eq!(got.rows.cell(0, 3), Cell::Text("42"));
    assert_eq!(got.rows.cell(0, 5), Cell::Text("1.50"));
    assert_eq!(got.rows.cell(0, 7), Cell::Text("2024-01-02"));
    assert_eq!(got.rows.cell(0, 8), Cell::Text("12:34:56"));
    assert_eq!(got.rows.cell(0, 9), Cell::Text("列"));
    let types: Vec<_> = got
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.type_name.as_str(), c.nullable))
        .collect();
    println!("[null_empty_and_types] columns: {types:?}");
    assert_eq!(got.columns[0].nullable, Some(true));
    assert_eq!(got.columns[3].type_name, "INT");
    assert_eq!(got.columns[4].type_name, "BIGINT UNSIGNED");
    assert_eq!(got.columns[5].type_name, "DECIMAL");
    assert_eq!(got.columns[7].type_name, "DATE");
    assert_eq!(got.columns[8].type_name, "TIME");

    // A statement without a result set reports affected rows.
    let mut h = driver.query(
        "CREATE TEMPORARY TABLE tmp_t (a INT)".into(),
        QueryOptions::default(),
    );
    assert!(matches!(collect(&mut h).outcome, Outcome::Done { rows: 0 }));
}

/// Waits for the first events and returns the session id.
fn start(driver: &MysqlDriver, sql: &str, opts: QueryOptions) -> (db_core::QueryHandle, u64) {
    let mut h = driver.query(sql.into(), opts);
    match h.blocking_next() {
        Some(QueryEvent::Started { session_id }) => (h, session_id),
        other => panic!("expected Started, got {other:?}"),
    }
}

fn read_batches(h: &mut db_core::QueryHandle, n: usize) {
    let mut seen = 0;
    while seen < n {
        match h.blocking_next() {
            Some(QueryEvent::Columns(_)) => {}
            Some(QueryEvent::Batch(_)) => seen += 1,
            other => panic!("expected data, got {other:?}"),
        }
    }
}

/// The next query on the same driver has to run at once.
fn next_query_ms(driver: &MysqlDriver) -> f64 {
    let t = Instant::now();
    let mut h = driver.query("SELECT 1".into(), QueryOptions::default());
    let got = collect(&mut h);
    let elapsed = ms(t.elapsed());
    assert!(
        matches!(got.outcome, Outcome::Done { rows: 1 }),
        "{:?}",
        got.outcome
    );
    assert_eq!(got.rows.cell(0, 0), Cell::Text("1"));
    elapsed
}

fn cancel_runs(test: &str, sql: &str, mid_stream: bool) {
    let driver = connect();
    let obs = Observer::new();
    obs.print_resolution(test);
    let (mut fin, mut gone, mut next) = (vec![], vec![], vec![]);
    for _ in 0..RUNS {
        let (mut h, id) = start(&driver, sql, QueryOptions::default());
        if mid_stream {
            read_batches(&mut h, 3);
        }
        obs.wait_running(id);
        let watcher = obs.watch_gone(id);
        let t0 = Instant::now();
        h.cancel();
        let mut rows_after = 0;
        let outcome = loop {
            match h.blocking_next().expect("stream ended without Finished") {
                QueryEvent::Finished(o) => break o,
                QueryEvent::Batch(b) => rows_after += b.rows(),
                _ => {}
            }
        };
        fin.push(ms(t0.elapsed()));
        assert!(
            matches!(outcome, Outcome::Cancelled),
            "expected Cancelled, got {outcome:?}"
        );
        assert!(
            rows_after < 10_000,
            "{rows_after} rows still arrived after the cancel"
        );
        gone.push(ms(join_gone(watcher).saturating_duration_since(t0)));
        next.push(next_query_ms(&driver));
    }
    let (_, fin_max) = report(test, "cancel() -> Finished(Cancelled)", &fin, "ms");
    let (_, gone_max) = report(
        test,
        "cancel() -> statement gone from PROCESSLIST",
        &gone,
        "ms",
    );
    let (_, next_max) = report(test, "next query on the same driver", &next, "ms");
    assert!(
        fin_max < lim_ms(1000.0),
        "Finished(Cancelled) took {fin_max} ms"
    );
    assert!(
        gone_max < lim_ms(1000.0),
        "statement was still on the server {gone_max} ms after the cancel"
    );
    assert!(next_max < lim_ms(100.0), "next query took {next_max} ms");
}

#[test]
fn cancel_mid_stream() {
    cancel_runs("cancel_mid_stream", BIG_QUERY, true);
}

#[test]
fn cancel_during_sleep() {
    cancel_runs("cancel_during_sleep", SLEEP_QUERY, false);
}

#[test]
fn drop_handle_without_reading_to_the_end() {
    let test = "drop_handle";
    let driver = connect();
    let obs = Observer::new();
    obs.print_resolution(test);
    for (label, sql, mid_stream) in [
        ("mid-stream", BIG_QUERY, true),
        ("during SLEEP(30)", SLEEP_QUERY, false),
    ] {
        let mut gone = vec![];
        let mut next = vec![];
        for _ in 0..RUNS {
            let (mut h, id) = start(&driver, sql, QueryOptions::default());
            if mid_stream {
                read_batches(&mut h, 1);
            }
            obs.wait_running(id);
            let watcher = obs.watch_gone(id);
            let t0 = Instant::now();
            drop(h);
            gone.push(ms(join_gone(watcher).saturating_duration_since(t0)));
            next.push(next_query_ms(&driver));
        }
        let (_, gone_max) = report(
            test,
            &format!("{label}: drop(handle) -> statement gone"),
            &gone,
            "ms",
        );
        let (_, next_max) = report(
            test,
            &format!("{label}: next query on the same driver"),
            &next,
            "ms",
        );
        assert!(
            gone_max < lim_ms(1000.0),
            "{label}: statement still running {gone_max} ms after the drop"
        );
        assert!(
            next_max < lim_ms(100.0),
            "{label}: next query took {next_max} ms"
        );
    }
}

#[test]
fn timeout_stops_the_query_on_the_server() {
    let test = "timeout";
    let driver = connect();
    let obs = Observer::new();
    obs.print_resolution(test);
    let (mut total, mut gone_after, mut next) = (vec![], vec![], vec![]);
    for _ in 0..RUNS {
        let t0 = Instant::now();
        let opts = QueryOptions {
            timeout: Some(Duration::from_millis(500)),
            ..Default::default()
        };
        let (mut h, id) = start(&driver, SLEEP_QUERY, opts);
        obs.wait_running(id);
        let watcher = obs.watch_gone(id);
        let outcome = finish(&mut h);
        let fin = t0.elapsed();
        total.push(ms(fin));
        match outcome {
            Outcome::Failed(e) => assert_eq!(e.kind, ErrorKind::Timeout, "{e}"),
            other => panic!("expected Failed(Timeout), got {other:?}"),
        }
        let gone = join_gone(watcher);
        gone_after.push(ms(gone.saturating_duration_since(t0)) - 500.0);
        next.push(next_query_ms(&driver));
    }
    let (min_total, max_total) = {
        let (_, max) = report(
            test,
            "query() -> Failed(Timeout) (limit 500 ms)",
            &total,
            "ms",
        );
        (total.iter().cloned().fold(f64::MAX, f64::min), max)
    };
    let (_, gone_max) = report(
        test,
        "statement gone, ms after the 500 ms limit",
        &gone_after,
        "ms",
    );
    let (_, next_max) = report(test, "next query on the same driver", &next, "ms");
    assert!(
        min_total >= 500.0 && max_total < lim_ms(1500.0),
        "timeout fired at {min_total}..{max_total} ms"
    );
    assert!(
        gone_max < lim_ms(500.0),
        "statement still running {gone_max} ms after the timeout"
    );
    assert!(next_max < lim_ms(100.0), "next query took {next_max} ms");
}

#[test]
fn kill_session_from_a_second_driver() {
    let test = "kill_session";
    let victim = connect();
    let killer = connect();
    let obs = Observer::new();
    obs.print_resolution(test);
    let (mut kill_ms, mut gone, mut ended) = (vec![], vec![], vec![]);
    for _ in 0..RUNS {
        let (mut h, id) = start(&victim, SLEEP_QUERY, QueryOptions::default());
        obs.wait_running(id);
        let t0 = Instant::now();
        let mut k = killer.kill_session(id);
        let k_out = finish(&mut k);
        kill_ms.push(ms(t0.elapsed()));
        assert!(matches!(k_out, Outcome::Done { rows: 0 }), "{k_out:?}");
        let out = finish(&mut h);
        ended.push(ms(t0.elapsed()));
        println!("[{test}] victim stream ended with {out:?}");
        let start = Instant::now();
        while obs.session_exists(id) {
            assert!(
                start.elapsed() < lim(Duration::from_secs(5)),
                "session {id} still exists after KILL"
            );
        }
        gone.push(ms(t0.elapsed()));
        // The victim driver reconnects on demand.
        let _ = next_query_ms(&victim);
    }
    report(test, "kill_session() -> Finished(Done)", &kill_ms, "ms");
    report(test, "kill_session() -> victim stream ended", &ended, "ms");
    let (_, gone_max) = report(
        test,
        "kill_session() -> session gone from PROCESSLIST",
        &gone,
        "ms",
    );
    assert!(gone_max < lim_ms(1000.0));

    // Unknown id: the server's error comes back as Failed.
    let mut k = killer.kill_session(4_000_000_000);
    match finish(&mut k) {
        Outcome::Failed(e) => {
            println!("[{test}] unknown id -> {e}");
            assert_eq!(e.kind, ErrorKind::Query);
            assert_eq!(e.code, Some(1094));
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn tls_unverified_connects_encrypted_to_a_self_signed_server() {
    let driver = block_on(MysqlDriver::connect(
        handle(),
        connect_options(TlsMode::RequireUnverified),
    ))
    .expect("RequireUnverified should accept the local self-signed certificate");
    let mut h = driver.query(
        "SHOW SESSION STATUS LIKE 'Ssl_cipher'".into(),
        QueryOptions::default(),
    );
    let got = collect(&mut h);
    let cipher = match got.rows.cell(0, 1) {
        Cell::Text(t) => t.to_string(),
        other => panic!("unexpected Ssl_cipher value {other:?}"),
    };
    println!("[tls] RequireUnverified session cipher = {cipher}");
    assert!(!cipher.is_empty(), "the session is not encrypted");
}

/// The local server offers TLS with a self-signed certificate (see
/// `server_tls_context`), so what `Require` rejects here is the certificate.
/// The no-TLS-at-all path is mapped to the same kind but has no local server
/// to test against.
#[test]
fn tls_required_rejects_a_self_signed_certificate() {
    let opts = connect_options(TlsMode::Require);
    let pw = opts.password.clone();
    let t = Instant::now();
    let err = match block_on(MysqlDriver::connect(handle(), opts)) {
        Ok(_) => panic!("TlsMode::Require accepted a self-signed certificate"),
        Err(e) => e,
    };
    println!(
        "[tls] Require against the local server -> {err} ({:.1} ms)",
        ms(t.elapsed())
    );
    assert_eq!(err.kind, ErrorKind::Tls);
    assert!(
        !format!("{err:?}{err}").contains(&pw),
        "the error leaks the password"
    );
}

#[test]
fn wrong_password_is_an_auth_error() {
    let mut opts = connect_options(TlsMode::Disabled);
    opts.password = "definitely-not-the-password".into();
    let err = match block_on(MysqlDriver::connect(handle(), opts.clone())) {
        Ok(_) => panic!("connected with a wrong password"),
        Err(e) => e,
    };
    println!("[auth] wrong password -> {err}");
    assert_eq!(err.kind, ErrorKind::Auth);
    assert_eq!(err.code, Some(1045));
    assert!(!format!("{err:?}{err}").contains(&opts.password));
    // The options type itself hides the password too.
    assert!(!format!("{opts:?}").contains(&opts.password));
}

#[test]
fn unreachable_server_is_a_connect_error() {
    let mut opts = connect_options(TlsMode::Disabled);
    opts.port = 1;
    let err = match block_on(MysqlDriver::connect(handle(), opts)) {
        Ok(_) => panic!("connected to a closed port"),
        Err(e) => e,
    };
    println!("[connect] closed port -> {err}");
    assert_eq!(err.kind, ErrorKind::Connect);
}

#[test]
fn syntax_error_is_a_query_error_and_the_driver_survives() {
    let driver = connect();
    let mut h = driver.query("SELEC 1".into(), QueryOptions::default());
    let got = collect(&mut h);
    match got.outcome {
        Outcome::Failed(e) => {
            println!("[query] syntax error -> {e}");
            assert_eq!(e.kind, ErrorKind::Query);
            assert_eq!(e.code, Some(1064));
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(next_query_ms(&driver) < lim_ms(100.0));
}

/// Context for the TLS test: says whether the local server offers TLS at all
/// (MariaDB 11.4+ generates a self-signed certificate by default).
#[test]
fn server_tls_context() {
    let driver = connect();
    let mut h = driver.query(
        "SHOW VARIABLES WHERE Variable_name IN ('have_ssl', 'version')".into(),
        QueryOptions::default(),
    );
    let got = collect(&mut h);
    for r in 0..got.rows.rows() {
        println!(
            "[tls] server {} = {:?}",
            match got.rows.cell(r, 0) {
                Cell::Text(t) => t,
                _ => "?",
            },
            got.rows.cell(r, 1)
        );
    }
}
