//! Server activity on the local MariaDB (`BENCH_DSN`; start it first): a second connection
//! runs `SELECT SLEEP(30)`, the session list shows it, KILL QUERY stops it,
//! KILL ends the session. Only touches its own sessions.

use db_core::{Driver, QueryEvent, QueryOptions};
use locus_core::activity::{self as act, Engine, KillMode};
use locus_core::driver::AnyDriver;
use locus_core::store::Tls;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

async fn connect() -> Arc<AnyDriver> {
    let (mut c, pw) = locus_core::store::parse_dsn(&dsn()).expect("BENCH_DSN");
    c.tls = Tls::Disabled;
    let rt = tokio::runtime::Handle::current();
    Arc::new(
        AnyDriver::connect(&rt, &c, pw, Default::default())
            .await
            .expect("local MariaDB: set BENCH_DSN and start the server"),
    )
}

async fn list(d: &AnyDriver) -> Vec<act::Session> {
    let rows = act::fetch(d, act::sessions_sql(Engine::Mysql).unwrap())
        .await
        .expect("list query");
    act::parse_sessions(Engine::Mysql, &rows)
}

async fn gone(d: &AnyDriver, id: &str) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if !list(d).await.iter().any(|s| s.id == id) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_kill_query_kill_connection() {
    let (victim, observer) = (connect().await, connect().await);
    let mut h = victim.query(
        "SELECT SLEEP(30) /* wb_test_activity */".into(),
        QueryOptions::default(),
    );
    let id = loop {
        match h.next().await {
            Some(QueryEvent::Started { session_id }) => break session_id.to_string(),
            Some(_) => {}
            None => panic!("victim ended before it started"),
        }
    };
    // The sleeping statement is in the list, running, not ours.
    let start = Instant::now();
    let s = loop {
        let all = list(&observer).await;
        assert_eq!(
            all.iter().filter(|s| s.mine).count(),
            1,
            "exactly one own session"
        );
        if let Some(s) = all.into_iter().find(|s| s.id == id && s.active) {
            break s;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "session {id} not listed as running"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(!s.mine && s.sql.contains("wb_test_activity"), "{s:?}");

    // Kill query: the statement stops, the session stays.
    let kill = act::kill_cmd(Engine::Mysql, &s, KillMode::Query).unwrap();
    act::fetch(&observer, kill.text())
        .await
        .expect("KILL QUERY");
    let t = Instant::now();
    while h.next().await.is_some() {
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "victim statement still running"
        );
    }
    let after = list(&observer)
        .await
        .into_iter()
        .find(|x| x.id == id)
        .expect("session survives KILL QUERY");
    assert!(!after.active, "{after:?}");

    // Kill connection: the session is gone.
    let kill = act::kill_cmd(Engine::Mysql, &after, KillMode::Connection).unwrap();
    act::fetch(&observer, kill.text()).await.expect("KILL");
    assert!(
        gone(&observer, &id).await,
        "session {id} still listed after KILL"
    );

    // Our own session is refused.
    let me = list(&observer).await.into_iter().find(|s| s.mine).unwrap();
    assert_eq!(
        act::kill_cmd(Engine::Mysql, &me, KillMode::Connection),
        Err(act::KillError::Own)
    );

    // Variables and status read fine.
    let vars = act::fetch(&observer, act::variables_sql(Engine::Mysql).unwrap())
        .await
        .unwrap();
    assert!(
        act::parse_pairs(&vars)
            .iter()
            .any(|(k, _)| k == "max_connections")
    );
    let st = act::fetch(&observer, act::status_sql(Engine::Mysql).unwrap())
        .await
        .unwrap();
    assert!(act::parse_pairs(&st).iter().any(|(k, _)| k == "Uptime"));
}
