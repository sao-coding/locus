mod common;
use common::*;
use db_core::{DEFAULT_WINDOW, Driver, ErrorKind, Outcome, QueryEvent, QueryOptions};
use std::time::{Duration, Instant};
use tokio::time::sleep;

/// Cancel after the query has been running for 100 ms; `stream` consumes
/// batches meanwhile (BIG_Q), otherwise the statement just sleeps.
async fn cancel_scenario(name: &str, sql: &str, stream: bool) {
    let mut obs = Obs::new().await;
    let d = driver().await;
    let (mut to_finished, mut to_gone) = (vec![], vec![]);
    for _ in 0..RUNS {
        let mut h = d.query(sql.into(), opts());
        let id = started(&mut h).await;
        assert_eq!(
            d.running_sessions(),
            vec![id],
            "the host keeps the session id of the running query"
        );
        let deadline = Instant::now() + Duration::from_millis(100);
        let mut rows = 0u64;
        while let Ok(ev) =
            tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), h.next()).await
        {
            match ev.expect("stream ended") {
                QueryEvent::Batch(b) => rows += b.rows() as u64,
                QueryEvent::Finished(o) => panic!("query ended before the cancel: {o:?}"),
                _ => {}
            }
        }
        if stream {
            assert!(rows > 0, "no rows before the cancel");
        }
        let t0 = Instant::now();
        h.cancel();
        let (_, outcome) = drain(&mut h).await;
        let finished = ms(t0.elapsed());
        assert!(matches!(outcome, Outcome::Cancelled), "{outcome:?}");
        let gone = obs
            .gone(id, t0, Duration::from_secs(15))
            .await
            .expect("the statement is still running on the server");
        assert!(select1_ok(&d).await, "the next query must work");
        assert!(d.running_sessions().is_empty());
        to_finished.push(finished);
        to_gone.push(gone);
    }
    let f = report(name, "cancel -> Finished(Cancelled)", "ms", &to_finished);
    let g = report(name, "cancel -> gone from PROCESSLIST", "ms", &to_gone);
    println!("[{name}] POC reference: gone 18.1 / 22.0 ms");
    assert!(f.max < 250.0, "Finished(Cancelled) took {:.1} ms", f.max);
    assert!(
        g.max < 250.0,
        "statement lived {:.1} ms after the cancel",
        g.max
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_mid_stream() {
    cancel_scenario("cancel_mid_stream", BIG_Q, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_during_sleep() {
    cancel_scenario("cancel_during_sleep", "SELECT SLEEP(30)", false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_handle_cancels() {
    let mut obs = Obs::new().await;
    let d = driver().await;
    let mut gone = vec![];
    for _ in 0..RUNS {
        let mut h = d.query("SELECT SLEEP(30)".into(), opts());
        let id = started(&mut h).await;
        sleep(Duration::from_millis(100)).await;
        let t0 = Instant::now();
        drop(h);
        gone.push(
            obs.gone(id, t0, Duration::from_secs(15))
                .await
                .expect("dropped handle left the statement running"),
        );
        assert!(select1_ok(&d).await);
    }
    let g = report("drop_handle", "drop -> gone from PROCESSLIST", "ms", &gone);
    assert!(g.max < 250.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn back_pressure_stops_the_sidecar() {
    let mut obs = Obs::new().await;
    let d = driver().await;
    let (mut beyond, mut growth, mut to_finished, mut to_gone) = (vec![], vec![], vec![], vec![]);
    for _ in 0..RUNS {
        let base = d.batch_frames_received(); // the counter is cumulative per sidecar
        let mut h = d.query(BIG_Q.into(), opts());
        let id = started(&mut h).await;
        let mut consumed = 0u64;
        while consumed < 2 {
            if let Some(QueryEvent::Batch(_)) = h.next().await {
                consumed += 1;
            }
        }
        // The consumer stops reading. Allowed on the wire: the initial window
        // plus one credit per consumed batch.
        let allowed = DEFAULT_WINDOW as u64 + consumed;
        sleep(Duration::from_millis(500)).await;
        let pid = d.sidecar_pid().unwrap();
        let ws0 = working_set_mb(pid);
        let frames0 = d.batch_frames_received() - base;
        sleep(Duration::from_millis(2500)).await; // 3 s without reading
        let ws1 = working_set_mb(pid);
        let frames1 = d.batch_frames_received() - base;
        assert_eq!(frames1, frames0, "batches kept arriving while nobody read");
        assert!(
            frames1 <= allowed,
            "{frames1} batch frames received, credit allows {allowed}"
        );
        beyond.push(frames1.saturating_sub(allowed) as f64);
        growth.push(ws1 - ws0);

        let t0 = Instant::now();
        h.cancel();
        let (_, outcome) = drain(&mut h).await;
        to_finished.push(ms(t0.elapsed()));
        assert!(matches!(outcome, Outcome::Cancelled), "{outcome:?}");
        to_gone.push(
            obs.gone(id, t0, Duration::from_secs(15))
                .await
                .expect("statement still running"),
        );
        assert!(select1_ok(&d).await);
    }
    report("back_pressure", "batches beyond credit", "batches", &beyond);
    let g = report(
        "back_pressure",
        "sidecar working-set change over 2.5 s stall",
        "MB",
        &growth,
    );
    report(
        "back_pressure",
        "cancel -> Finished(Cancelled)",
        "ms",
        &to_finished,
    );
    report(
        "back_pressure",
        "cancel -> gone from PROCESSLIST",
        "ms",
        &to_gone,
    );
    println!(
        "[back_pressure] POC reference: cancel 0.5 / 1.2 ms, 0 batches beyond credit, memory flat"
    );
    assert!(beyond.iter().all(|b| *b == 0.0));
    assert!(
        g.max < 3.0,
        "sidecar working set grew {:.1} MB while stalled",
        g.max
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_stops_the_statement() {
    let mut obs = Obs::new().await;
    let d = driver().await;
    let (mut to_terminal, mut to_gone) = (vec![], vec![]);
    for _ in 0..RUNS {
        let t0 = Instant::now();
        let mut h = d.query(
            "SELECT SLEEP(10)".into(),
            QueryOptions {
                timeout: Some(Duration::from_secs(2)),
                ..opts()
            },
        );
        let id = started(&mut h).await;
        let (_, outcome) = drain(&mut h).await;
        to_terminal.push(ms(t0.elapsed()));
        let Outcome::Failed(e) = outcome else {
            panic!("expected Failed(Timeout), got {outcome:?}")
        };
        assert_eq!(e.kind, ErrorKind::Timeout, "{e}");
        to_gone.push(
            obs.gone(id, t0, Duration::from_secs(15))
                .await
                .expect("statement still running"),
        );
        assert!(select1_ok(&d).await);
    }
    let t = report("timeout_2s", "send -> Failed(Timeout)", "ms", &to_terminal);
    let g = report(
        "timeout_2s",
        "send -> gone from PROCESSLIST",
        "ms",
        &to_gone,
    );
    println!("[timeout_2s] POC reference: 2.0 s / gone 2.02 s");
    assert!(
        t.min >= 1900.0 && t.max < 2400.0,
        "timeout fired at {:.0}..{:.0} ms",
        t.min,
        t.max
    );
    assert!(g.max < 2500.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_session_ends_a_running_query() {
    let mut obs = Obs::new().await;
    let d = driver().await;
    let mut h = d.query("SELECT SLEEP(30)".into(), opts());
    let id = started(&mut h).await;
    let t0 = Instant::now();
    let mut k = d.kill_session(id);
    let (_, o) = drain(&mut k).await;
    assert!(
        matches!(o, Outcome::Done { rows: 0 }),
        "kill_session: {o:?}"
    );
    let gone = obs
        .gone(id, t0, Duration::from_secs(5))
        .await
        .expect("session still running");
    println!("[kill_session] kill_session -> gone: {gone:.1} ms");
    let (_, o) = drain(&mut h).await;
    assert!(
        matches!(o, Outcome::Failed(_)),
        "the killed query's handle must finish with an error, got {o:?}"
    );
    // Killing a session that no longer exists is not an error.
    let (_, o) = drain(&mut d.kill_session(id)).await;
    assert!(matches!(o, Outcome::Done { .. }), "{o:?}");
    assert!(select1_ok(&d).await);
}
