mod common;
use common::*;
use db_core::{Driver, ErrorKind, Outcome};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tokio::time::sleep;

const CHILD_ENV: &str = "SIDECAR_TEST_HOST_CHILD";

/// Not a test of its own: the entry point of the child process that
/// `host_exit_*` starts and kills. A normal run returns immediately.
#[test]
fn zz_host_child_entry() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let d = driver().await;
        let mut h = d.query("SELECT SLEEP(30)".into(), opts());
        let id = started(&mut h).await;
        sleep(Duration::from_millis(100)).await;
        println!("CHILD {id} {}", d.sidecar_pid().unwrap());
        sleep(Duration::from_secs(120)).await;
    });
}

/// The host process dies (TerminateProcess, no destructors): the sidecar sees
/// stdin EOF, sends KILL QUERY and exits by itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_exit_kills_the_statement() {
    let mut obs = Obs::new().await;
    let (mut exit_ms, mut gone_ms) = (vec![], vec![]);
    for _ in 0..RUNS {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "zz_host_child_entry",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, "1")
            .env("BENCH_DSN", dsn())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let (id, spid) = {
            let mut out = BufReader::new(child.stdout.take().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                assert!(
                    out.read_line(&mut line).unwrap() > 0,
                    "child exited before reporting its session"
                );
                if let Some(rest) = line.trim().split_once("CHILD ").map(|x| x.1) {
                    // the harness prints "test .. ... " on the same line
                    let mut it = rest.split_whitespace().map(|x| x.parse::<u64>().unwrap());
                    break (it.next().unwrap(), it.next().unwrap() as u32);
                }
            }
        };
        assert!(obs.running(id).await && alive(spid));
        let t0 = Instant::now();
        child.kill().unwrap(); // TerminateProcess
        let (mut exit, mut gone) = (None, None);
        while t0.elapsed() < Duration::from_secs(8) && (exit.is_none() || gone.is_none()) {
            if exit.is_none() && !alive(spid) {
                exit = Some(ms(t0.elapsed()));
            }
            if gone.is_none() && !obs.running(id).await {
                gone = Some(ms(t0.elapsed()));
            }
            sleep(Duration::from_millis(1)).await;
        }
        let _ = child.wait();
        if gone.is_none() {
            let _ =
                mysql_async::prelude::Queryable::query_drop(&mut obs.0, format!("KILL {id}")).await; // clean up
        }
        exit_ms.push(exit.expect("the sidecar did not exit after its host died"));
        gone_ms.push(gone.expect("the statement survived the host"));
    }
    let g = report("host_exit", "host killed -> statement gone", "ms", &gone_ms);
    let e = report(
        "host_exit",
        "host killed -> sidecar process exited",
        "ms",
        &exit_ms,
    );
    println!("[host_exit] POC reference: sidecar exit 230 ms, statement gone 221 ms");
    assert!(
        g.max <= 300.0,
        "statement lived {:.0} ms after the host died (limit about 300)",
        g.max
    );
    assert!(e.max <= 1000.0, "sidecar took {:.0} ms to exit", e.max);
}

/// The sidecar process is killed while a statement runs. The host finishes the
/// handle with Failed(Internal), starts a new sidecar and kills the saved session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sidecar_crash_is_recovered() {
    let mut obs = Obs::new().await;
    let d = driver().await;
    let (mut failed_ms, mut gone_ms) = (vec![], vec![]);
    for _ in 0..RUNS {
        let mut h = d.query("SELECT SLEEP(30)".into(), opts());
        let id = started(&mut h).await;
        sleep(Duration::from_millis(100)).await;
        let old_pid = d.sidecar_pid().unwrap();
        assert_eq!(d.running_sessions(), vec![id]);
        let t0 = Instant::now();
        terminate(old_pid);
        let (_, outcome) = drain(&mut h).await;
        failed_ms.push(ms(t0.elapsed()));
        let Outcome::Failed(e) = outcome else {
            panic!("expected Failed(Internal), got {outcome:?}")
        };
        assert_eq!(e.kind, ErrorKind::Internal, "{e}");
        // Without recovery MariaDB only notices the dead socket when SLEEP ends (~4.9 s in the POC).
        gone_ms.push(
            obs.gone(id, t0, Duration::from_secs(4))
                .await
                .expect("the orphaned statement was not killed by the new sidecar"),
        );
        assert!(
            select1_ok(&d).await,
            "queries must work again through the new sidecar"
        );
        let new_pid = d.sidecar_pid().unwrap();
        assert_ne!(new_pid, old_pid, "a fresh sidecar must be running");
        assert!(!alive(old_pid));
    }
    report(
        "sidecar_crash",
        "sidecar killed -> handle Failed(Internal)",
        "ms",
        &failed_ms,
    );
    let g = report(
        "sidecar_crash",
        "sidecar killed -> orphan gone (incl. new sidecar start)",
        "ms",
        &gone_ms,
    );
    println!(
        "[sidecar_crash] POC reference: 71-81 ms from a warm start of the new sidecar; ~4.9 s without recovery"
    );
    assert!(g.max < 1500.0, "recovery took {:.0} ms", g.max);
}

/// Orderly shutdown with a query running: stdin closes first, the sidecar
/// kills the statement and exits within the grace period, nothing is terminated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orderly_shutdown_with_a_running_query() {
    let mut obs = Obs::new().await;
    let (mut gone_ms, mut exit_ms) = (vec![], vec![]);
    for _ in 0..RUNS {
        let d = driver().await;
        let mut h = d.query("SELECT SLEEP(30)".into(), opts());
        let id = started(&mut h).await;
        sleep(Duration::from_millis(100)).await;
        let pid = d.sidecar_pid().unwrap();
        let t0 = Instant::now();
        let report = d.shutdown().await.expect("a running sidecar");
        exit_ms.push(ms(t0.elapsed()));
        assert!(
            !report.forced,
            "the sidecar had to be terminated: {report:?}"
        );
        assert!(!alive(pid), "sidecar still running after shutdown");
        gone_ms.push(
            obs.gone(id, t0, Duration::from_secs(5))
                .await
                .expect("statement still running after shutdown"),
        );
        let (_, outcome) = drain(&mut h).await;
        assert!(
            matches!(outcome, Outcome::Cancelled),
            "the running query should end Cancelled, got {outcome:?}"
        );
        // A shut-down driver does not start new sidecars.
        let (_, o) = drain(&mut d.query("SELECT 1".into(), opts())).await;
        assert!(matches!(o, Outcome::Failed(_)), "{o:?}");
    }
    let e = report("orderly_shutdown", "shutdown() returned", "ms", &exit_ms);
    let g = report(
        "orderly_shutdown",
        "shutdown start -> statement gone",
        "ms",
        &gone_ms,
    );
    println!(
        "[orderly_shutdown] POC reference: sidecar cleans up and exits in about 250 ms; no forced termination"
    );
    assert!(e.max < 1000.0, "shutdown took {:.0} ms", e.max);
    assert!(g.max < 1000.0);
}
