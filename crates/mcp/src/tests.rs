//! End-to-end tests without a database server: SQLite files, a fake host,
//! and the real HTTP endpoint on an ephemeral 127.0.0.1 port.

use crate::host::{ConfirmRequest, Event, Host};
use crate::tools::{Answer, Call, Core};
use locus_core::store::{Env, McpAccess, SavedConnection};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Audit lines go to a temp folder, never the real app data.
fn data_dir() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let d = std::env::temp_dir().join(format!("locus-mcp-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        // SAFETY: set once, before any test thread reads it (the Once blocks others).
        unsafe { std::env::set_var("LOCUS_DATA_DIR", &d) };
    });
}

#[derive(Default)]
struct FakeHost {
    conns: Mutex<Vec<SavedConnection>>,
    events: Mutex<Vec<Event>>,
    /// `Some(answer)`: answer at once. `None`: keep the request unanswered.
    answer: Mutex<Option<bool>>,
    asked: Mutex<Vec<(String, String, String)>>,
    pending: Mutex<Vec<tokio::sync::oneshot::Sender<bool>>>,
}

impl Host for FakeHost {
    fn connections(&self) -> Vec<SavedConnection> {
        self.conns.lock().clone()
    }
    fn password(&self, _: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn confirm(&self, req: ConfirmRequest) {
        self.asked
            .lock()
            .push((req.conn.name.clone(), req.client.clone(), req.sql.clone()));
        match *self.answer.lock() {
            Some(a) => {
                let _ = req.reply.send(a);
            }
            None => self.pending.lock().push(req.reply),
        }
    }
    fn event(&self, ev: Event) {
        self.events.lock().push(ev);
    }
}

async fn sqlite_file(tag: &str, rows: usize) -> String {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "locus-mcp-{tag}{n}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&p);
    locus_core::driver::create_sqlite_file(&p).unwrap();
    let path = p.to_string_lossy().into_owned();
    // Fill it through a driver, outside MCP.
    {
        let c = locus_core::store::sqlite_connection(&path);
        let d = locus_core::driver::AnyDriver::connect(
            &tokio::runtime::Handle::current(),
            &c,
            String::new(),
            Default::default(),
        )
        .await
        .unwrap();
        let s = d.session().await.unwrap();
        let mut sql = String::from(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT 'x', note TEXT)",
        );
        crate::exec::run_one(s.as_ref(), &sql, 1).await;
        for i in 0..rows {
            sql = format!(
                "INSERT INTO t (id, name, note) VALUES ({i}, 'n{i}', {})",
                if i == 0 {
                    format!("'{}'", "y".repeat(300))
                } else {
                    "NULL".into()
                }
            );
            crate::exec::run_one(s.as_ref(), &sql, 1).await;
        }
    }
    path
}

fn conn(id: &str, name: &str, path: &str, env: Env, mcp: Option<McpAccess>) -> SavedConnection {
    let mut c = locus_core::store::sqlite_connection(path);
    c.id = id.into();
    c.name = name.into();
    c.env = env;
    c.mcp = mcp;
    c
}

fn args(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => panic!(),
    }
}

struct Fixture {
    host: Arc<FakeHost>,
    core: Arc<Core>,
    path: String,
}

async fn fixture(rows: usize) -> Fixture {
    data_dir();
    let path = sqlite_file("fx", rows).await;
    let host = Arc::new(FakeHost::default());
    *host.conns.lock() = vec![
        conn("dev1", "devdb", &path, Env::Dev, None),
        conn(
            "ro1",
            "readonly-db",
            &path,
            Env::Dev,
            Some(McpAccess::ReadOnly),
        ),
        conn("off1", "hidden-db", &path, Env::Dev, Some(McpAccess::Off)),
        conn("prod1", "prod-db", &path, Env::Prod, None),
        conn(
            "prodrw",
            "prod-rw",
            &path,
            Env::Prod,
            Some(McpAccess::ReadWrite),
        ),
    ];
    let core = Arc::new(Core::new(host.clone(), tokio::runtime::Handle::current()));
    Fixture { host, core, path }
}

async fn tool(core: &Core, name: &str, a: Value) -> Result<String, String> {
    let ct = tokio_util::sync::CancellationToken::new();
    let call = Call {
        client: "test-client",
        ct: &ct,
    };
    match name {
        "list_connections" => core.list_connections(),
        "list_objects" => core.list_objects(&args(a), &call).await,
        "describe_table" => core.describe_table(&args(a), &call).await,
        "run_sql" => core.run_sql(&args(a), &call).await,
        "explain" => core.explain(&args(a), &call).await,
        "get_ddl" => core.get_ddl(&args(a), &call).await,
        _ => unreachable!(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn connections_list_hides_off_and_shows_access() {
    let f = fixture(0).await;
    let out = tool(&f.core, "list_connections", json!({})).await.unwrap();
    assert!(
        out.starts_with("id\tname\tkind\tenv\taccess\tdatabase\n"),
        "{out}"
    );
    assert!(
        out.contains("dev1\tdevdb\tsqlite\tdev\tread-write\t"),
        "{out}"
    );
    assert!(
        out.contains("ro1\treadonly-db\tsqlite\tdev\tread-only"),
        "{out}"
    );
    assert!(
        out.contains("prod1\tprod-db\tsqlite\tprod\tread-only"),
        "{out}"
    );
    assert!(!out.contains("hidden-db") && !out.contains("off1"), "{out}");
    assert!(out.ends_with("(4 connections)"), "{out}");
    // Off is invisible everywhere, by id and by name.
    for c in ["off1", "hidden-db"] {
        let e = tool(
            &f.core,
            "run_sql",
            json!({"connection": c, "sql": "SELECT 1"}),
        )
        .await
        .unwrap_err();
        assert!(e.contains("unknown connection"), "{e}");
    }
    let _ = std::fs::remove_file(&f.path);
}

#[tokio::test(flavor = "multi_thread")]
async fn read_write_and_read_only_paths() {
    let f = fixture(300).await;
    // Read with the default cap: 200 of 300, long cell cut.
    let out = tool(
        &f.core,
        "run_sql",
        json!({"connection": "devdb", "sql": "SELECT id, name, note FROM t ORDER BY id"}),
    )
    .await
    .unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "id\tname\tnote");
    assert_eq!(lines.len(), 1 + 200 + 1, "{out}");
    assert!(lines[1].ends_with("…[+100]"), "{}", lines[1]);
    assert_eq!(lines[2], "1\tn1\tNULL");
    assert_eq!(*lines.last().unwrap(), "(200 of 300 rows shown, truncated)");
    // max_rows.
    let out = tool(
        &f.core,
        "run_sql",
        json!({"connection": "dev1", "sql": "SELECT id FROM t ORDER BY id", "max_rows": 2}),
    )
    .await
    .unwrap();
    assert_eq!(out, "id\n0\n1\n(2 of 300 rows shown, truncated)");
    // A write on Dev runs at once; several statements are numbered.
    let out = tool(
        &f.core,
        "run_sql",
        json!({"connection": "devdb", "sql": "UPDATE t SET name = 'z' WHERE id < 3; SELECT COUNT(*) AS n FROM t WHERE name = 'z'"}),
    )
    .await
    .unwrap();
    assert_eq!(
        out,
        "#1 UPDATE t SET name = 'z' WHERE id < 3\n3 rows affected\n\n#2 SELECT COUNT(*) AS n FROM t WHERE name = 'z'\nn\n3\n(1 row)"
    );
    assert!(f.host.asked.lock().is_empty());
    // Read-only: refused before anything is sent, reads still work.
    let e = tool(
        &f.core,
        "run_sql",
        json!({"connection": "readonly-db", "sql": "DELETE FROM t"}),
    )
    .await
    .unwrap_err();
    assert_eq!(
        e,
        "connection 'readonly-db' is read-only for MCP; DELETE refused (nothing was sent)"
    );
    let out = tool(
        &f.core,
        "run_sql",
        json!({"connection": "ro1", "sql": "SELECT COUNT(*) AS n FROM t"}),
    )
    .await
    .unwrap();
    assert_eq!(out, "n\n300\n(1 row)");
    // Production default (read-only): refused without asking.
    let e = tool(
        &f.core,
        "run_sql",
        json!({"connection": "prod-db", "sql": "UPDATE t SET name = 'p' WHERE id = 1"}),
    )
    .await
    .unwrap_err();
    assert!(e.contains("'prod-db' is read-only for MCP"), "{e}");
    assert!(f.host.asked.lock().is_empty());
    // Errors are short.
    let e = tool(
        &f.core,
        "run_sql",
        json!({"connection": "dev1", "sql": "SELECT * FROM nope"}),
    )
    .await
    .unwrap();
    assert!(e.starts_with("error: ") && e.contains("nope"), "{e}");
    // History entries carry the client; Busy / Idle pairs.
    let ev = f.host.events.lock();
    assert!(ev.iter().any(|e| matches!(e,
        Event::History(h) if h.mcp_client.as_deref() == Some("test-client") && h.conn_id == "dev1")));
    let busy = ev
        .iter()
        .filter(|e| matches!(e, Event::Busy { .. }))
        .count();
    let idle = ev
        .iter()
        .filter(|e| matches!(e, Event::Idle { .. }))
        .count();
    assert_eq!(busy, idle);
    assert!(busy > 0);
    drop(ev);
    let _ = std::fs::remove_file(&f.path);
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_also_holds_on_the_server_side() {
    // Even if the classifier were fooled, a read-only connection's session refuses writes
    // (SQLite: PRAGMA query_only).
    let f = fixture(1).await;
    let c = f.core.resolve("ro1").unwrap();
    let ct = tokio_util::sync::CancellationToken::new();
    let _call = Call {
        client: "t",
        ct: &ct,
    };
    let d = locus_core::driver::AnyDriver::connect(
        &tokio::runtime::Handle::current(),
        &c,
        String::new(),
        Arc::new(std::sync::atomic::AtomicBool::new(true)),
    )
    .await
    .unwrap();
    let s = d.session().await.unwrap();
    let out = crate::exec::run_one(s.as_ref(), "DELETE FROM t", 1).await;
    assert!(
        matches!(out, crate::exec::StmtOut::Failed(ref m) if m.contains("readonly") || m.contains("read-only") || m.contains("query_only")),
        "{out:?}"
    );
    let _ = std::fs::remove_file(&f.path);
}

#[tokio::test(flavor = "multi_thread")]
async fn prod_read_write_asks_and_obeys_the_answer() {
    let f = fixture(2).await;
    *f.host.answer.lock() = Some(false);
    let e = tool(
        &f.core,
        "run_sql",
        json!({"connection": "prod-rw", "sql": "DELETE FROM t WHERE id = 0"}),
    )
    .await
    .unwrap_err();
    assert!(e.contains("denied"), "{e}");
    let asked = f.host.asked.lock().clone();
    assert_eq!(
        asked,
        vec![(
            "prod-rw".to_string(),
            "test-client".to_string(),
            "DELETE FROM t WHERE id = 0".to_string()
        )]
    );
    let n = tool(
        &f.core,
        "run_sql",
        json!({"connection": "prod-rw", "sql": "SELECT COUNT(*) AS n FROM t"}),
    )
    .await
    .unwrap();
    assert_eq!(n, "n\n2\n(1 row)", "deny sent nothing");
    // Reads on Production never ask.
    assert_eq!(f.host.asked.lock().len(), 1);
    *f.host.answer.lock() = Some(true);
    let out = tool(
        &f.core,
        "run_sql",
        json!({"connection": "prod-rw", "sql": "DELETE FROM t WHERE id = 0"}),
    )
    .await
    .unwrap();
    assert_eq!(out, "1 row affected");
    let _ = std::fs::remove_file(&f.path);
}

#[tokio::test(flavor = "multi_thread")]
async fn prod_write_without_an_answer_times_out() {
    let f = fixture(1).await;
    let mut core = Core::new(f.host.clone(), tokio::runtime::Handle::current());
    core.confirm_timeout = Duration::from_millis(150);
    let e = tool(
        &core,
        "run_sql",
        json!({"connection": "prod-rw", "sql": "DELETE FROM t"}),
    )
    .await
    .unwrap_err();
    assert!(e.contains("no answer in Locus"), "{e}");
    assert!(
        f.host
            .events
            .lock()
            .iter()
            .any(|e| matches!(e, Event::ConfirmExpired { .. }))
    );
    let n = tool(
        &core,
        "run_sql",
        json!({"connection": "prod-rw", "sql": "SELECT COUNT(*) AS n FROM t"}),
    )
    .await
    .unwrap();
    assert_eq!(n, "n\n1\n(1 row)");
    let _ = std::fs::remove_file(&f.path);
}

/// The real 60 s limit, on a paused clock.
#[tokio::test(start_paused = true)]
async fn confirmation_waits_sixty_seconds() {
    data_dir();
    let host = Arc::new(FakeHost::default());
    let core = Core::new(host.clone(), tokio::runtime::Handle::current());
    assert_eq!(core.confirm_timeout, Duration::from_secs(60));
    let c = conn("p", "prod", "x.db", Env::Prod, Some(McpAccess::ReadWrite));
    let info = locus_core::guard::WriteInfo {
        keywords: vec!["DELETE".into()],
        count: 1,
        missing_where: true,
    };
    let start = tokio::time::Instant::now();
    let a = core.ask(&c, "cli", "DELETE FROM t", info.clone()).await;
    assert_eq!(a, Answer::TimedOut);
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_secs(60) && waited < Duration::from_secs(61),
        "{waited:?}"
    );
    // A dropped reply (dialog closed) is a Deny, at once.
    host.pending.lock().clear();
    *host.answer.lock() = None;
    let h2 = host.clone();
    let t = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        h2.pending.lock().clear();
    });
    let start = tokio::time::Instant::now();
    assert_eq!(
        core.ask(&c, "cli", "DELETE FROM t", info).await,
        Answer::Denied
    );
    assert!(start.elapsed() < Duration::from_secs(6));
    t.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn catalog_tools_on_sqlite() {
    let f = fixture(1).await;
    let out = tool(&f.core, "list_objects", json!({"connection": "devdb"}))
        .await
        .unwrap();
    assert!(out.starts_with("schema\nmain\t(current)\n"), "{out}");
    let out = tool(
        &f.core,
        "list_objects",
        json!({"connection": "devdb", "schema": "main"}),
    )
    .await
    .unwrap();
    assert_eq!(out, "name\tkind\nt\ttable\n(1 objects)");
    let out = tool(
        &f.core,
        "describe_table",
        json!({"connection": "devdb", "table": "t"}),
    )
    .await
    .unwrap();
    assert!(
        out.starts_with("main.t\ncolumn\ttype\tnull\tdefault\textra\n"),
        "{out}"
    );
    assert!(out.contains("\nid\tINTEGER\t"), "{out}");
    assert!(out.contains("name\tTEXT\tno\t'x'"), "{out}");
    assert!(out.contains("pk: (id)"), "{out}");
    let e = tool(
        &f.core,
        "describe_table",
        json!({"connection": "devdb", "table": "missing"}),
    )
    .await
    .unwrap_err();
    assert!(e.contains("missing"), "{e}");
    let ddl = tool(
        &f.core,
        "get_ddl",
        json!({"connection": "devdb", "object": "t"}),
    )
    .await
    .unwrap();
    assert!(ddl.contains("CREATE TABLE t"), "{ddl}");
    let plan = tool(
        &f.core,
        "explain",
        json!({"connection": "ro1", "sql": "SELECT * FROM t WHERE id = 1"}),
    )
    .await
    .unwrap();
    assert!(plan.to_uppercase().contains("SEARCH"), "{plan}");
    let _ = std::fs::remove_file(&f.path);
}

// ---- HTTP -------------------------------------------------------------------

async fn post(port: u16, headers: &[(&str, &str)], body: &str) -> (u16, String) {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let mut req = format!(
        "POST /mcp HTTP/1.1\r\ncontent-type: application/json\r\naccept: application/json, text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe-client","version":"1"}}}"#;

#[tokio::test(flavor = "multi_thread")]
async fn http_endpoint_checks_and_serves() {
    let f = fixture(1).await;
    let run = crate::server::start_with(
        &tokio::runtime::Handle::current(),
        0,
        "tok-123".into(),
        f.core.clone(),
    )
    .unwrap();
    let port = run.port;
    assert_ne!(port, 0);
    let host = format!("127.0.0.1:{port}");
    let good = [("host", host.as_str()), ("authorization", "Bearer tok-123")];

    let (st, _) = post(port, &[("host", &host)], INIT).await;
    assert_eq!(st, 401, "no token");
    let (st, _) = post(
        port,
        &[("host", &host), ("authorization", "Bearer nope")],
        INIT,
    )
    .await;
    assert_eq!(st, 401, "bad token");
    let (st, _) = post(
        port,
        &[good[0], good[1], ("origin", "http://evil.example")],
        INIT,
    )
    .await;
    assert_eq!(st, 403, "bad origin");
    let (st, _) = post(port, &[("host", "evil.example"), good[1]], INIT).await;
    assert_eq!(st, 403, "bad host");

    let (st, body) = post(port, &good, INIT).await;
    assert_eq!(st, 200, "{body}");
    assert!(body.contains("\"serverInfo\""), "{body}");
    let (st, body) = post(
        port,
        &[good[0], good[1], ("mcp-protocol-version", "2025-06-18")],
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    )
    .await;
    assert_eq!(st, 200, "{body}");
    assert!(
        body.contains("\"run_sql\"") && body.contains("\"get_ddl\""),
        "{body}"
    );
    let (st, body) = post(
        port,
        &[good[0], good[1], ("mcp-protocol-version", "2025-06-18")],
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"run_sql","arguments":{"connection":"devdb","sql":"SELECT 40+2 AS x"}}}"#,
    )
    .await;
    assert_eq!(st, 200, "{body}");
    assert!(body.contains("x\\n42\\n(1 row)"), "{body}");

    // A new token takes effect at once.
    run.set_token("tok-456".into());
    let (st, _) = post(port, &good, INIT).await;
    assert_eq!(st, 401);

    // The port is taken while running: a second server says so.
    let e = crate::server::start_with(
        &tokio::runtime::Handle::current(),
        port,
        "t".into(),
        f.core.clone(),
    )
    .err()
    .unwrap();
    assert!(e.contains(&format!("port {port}")), "{e}");
    run.stop();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
    );
    let _ = std::fs::remove_file(&f.path);
}
