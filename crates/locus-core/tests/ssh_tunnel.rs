//! SSH tunnel against an in-process `russh` server that forwards `direct-tcpip`
//! channels to the local MariaDB (127.0.0.1:3307).
//! Credentials: `BENCH_DSN`; the test is skipped without it.

use db_core::{Driver, Outcome, QueryEvent};
use locus_core::driver::AnyDriver;
use locus_core::sshcfg::{SshAuth, SshConfig, fingerprint};
use locus_core::store::{DbKind, SavedConnection, Tls, parse_dsn};
use locus_core::tunnel::{self, CODE_HOST_CHANGED, CODE_HOST_UNKNOWN};
use russh::keys::PrivateKey;
use russh::server::{self, Msg, Server as _, Session};
use russh::{Channel, ChannelOpenFailure};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;

/// Test-only host keys (generated with ssh-keygen, no passphrase).
const KEY_A: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACAtHEhd56gMex+OxhhFQ1Bs2RVCJ6CMxUPPrPCsatdLDgAAAIgskvFwLJLx
cAAAAAtzc2gtZWQyNTUxOQAAACAtHEhd56gMex+OxhhFQ1Bs2RVCJ6CMxUPPrPCsatdLDg
AAAEAr9NmZAYxLr/EkFETl6PcO2r8Fmi7KteCkFn/7lTks3i0cSF3nqAx7H47GGEVDUGzZ
FUInoIzFQ8+s8Kxq10sOAAAAAAECAwQF
-----END OPENSSH PRIVATE KEY-----
";
const KEY_B: &str = include_str!("keyb.txt");

#[derive(Clone)]
struct Srv;

impl server::Server for Srv {
    type Handler = Srv;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Srv {
        Srv
    }
}

impl server::Handler for Srv {
    type Error = russh::Error;

    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        Ok(if user == "tun" && password == "pw" {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _: &str,
        _: u32,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        match TcpStream::connect((host, port as u16)).await {
            Ok(mut up) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let mut s = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut up, &mut s).await;
                });
            }
            Err(_) => reply.reject(ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }
}

struct Running {
    port: u16,
    handle: server::RunningServerHandle,
}

impl Running {
    fn stop(&self) {
        self.handle.shutdown("test".into());
    }
}

/// Starts the SSH server on `port` (0 = any free port).
async fn start_server(key: &str, port: u16) -> Running {
    let config = Arc::new(server::Config {
        keys: vec![PrivateKey::from_openssh(key).unwrap()],
        auth_rejection_time: Duration::from_millis(1),
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let socket = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let port = socket.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut srv = Srv;
        let running = srv.run_on_socket(config, &socket);
        let _ = tx.send(running.handle());
        let _ = running.await;
    });
    Running {
        port,
        handle: rx.await.unwrap(),
    }
}

fn db() -> Option<(SavedConnection, String)> {
    let dsn = std::env::var("BENCH_DSN").ok().or_else(|| {
        let env = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../devenv/tools/bench.env"),
        )
        .ok()?;
        env.lines()
            .find_map(|l| l.strip_prefix("BENCH_DSN=").map(str::to_string))
    })?;
    parse_dsn(&dsn)
}

fn known_hosts_file(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("wb-ssh-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn ssh_conn(base: &SavedConnection, id: &str, ssh_port: u16, password: &str) -> SavedConnection {
    let mut c = base.clone();
    c.id = id.into();
    c.tls = Tls::Disabled;
    c.kind = DbKind::Mysql;
    c.ssh = Some(SshConfig {
        host: "127.0.0.1".into(),
        port: ssh_port,
        user: "tun".into(),
        auth: SshAuth::Password,
        key_path: None,
        secret: Some(password.into()),
    });
    c
}

fn spec(c: &SavedConnection, kh: &Path) -> tunnel::TunnelSpec {
    tunnel::TunnelSpec {
        conn_id: c.id.clone(),
        ssh: c.ssh.clone().unwrap(),
        target_host: c.host.clone(),
        target_port: c.port,
        known_hosts: kh.to_path_buf(),
    }
}

async fn trust(kh: &Path, port: u16) -> String {
    let rt = tokio::runtime::Handle::current();
    let c = db().unwrap().0;
    let err = tunnel::acquire(&rt, spec(&ssh_conn(&c, "trust", port, "pw"), kh))
        .await
        .err()
        .expect("unknown host must be refused");
    assert_eq!(err.code, Some(CODE_HOST_UNKNOWN), "{err}");
    tunnel::trust_pending(kh, "127.0.0.1", port).unwrap()
}

async fn rows(d: &AnyDriver, sql: &str) -> Result<usize, db_core::DbError> {
    let mut h = d.query(sql.into(), Default::default());
    let mut n = 0;
    while let Some(ev) = h.next().await {
        match ev {
            QueryEvent::Batch(b) => n += b.rows(),
            QueryEvent::Finished(Outcome::Failed(e)) => return Err(e),
            _ => {}
        }
    }
    Ok(n)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn query_cancel_and_reconnect_through_the_tunnel() {
    let Some((base, pw)) = db() else {
        return eprintln!("skipped: no BENCH_DSN");
    };
    let rt = tokio::runtime::Handle::current();
    let kh = known_hosts_file("main");
    let srv = start_server(KEY_A, 0).await;
    let ssh_port = srv.port;
    let fp = trust(&kh, ssh_port).await;
    assert!(fp.starts_with("SHA256:"));

    let c = ssh_conn(&base, "t-main", ssh_port, "pw");
    // The driver sees only the local end: 127.0.0.1:<random>, not 3307.
    let t = tunnel::acquire(&rt, spec(&c, &kh))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_ne!(t.local_port(), base.port);
    let opts = c.options(pw.clone());
    let driver = db_mysql::MysqlDriver::connect(
        &rt,
        db_core::ConnectOptions {
            host: "127.0.0.1".into(),
            port: t.local_port(),
            ..opts
        },
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    let d = AnyDriver::Mysql(driver);
    assert_eq!(rows(&d, "SELECT 1 UNION SELECT 2").await.unwrap(), 2);

    // Cancel (KILL QUERY goes over a second connection, through the same tunnel).
    let mut h = d.query("SELECT SLEEP(30)".into(), Default::default());
    tokio::time::sleep(Duration::from_millis(500)).await;
    let t0 = Instant::now();
    h.cancel();
    while h.next().await.is_some() {}
    assert!(
        t0.elapsed() < slack(Duration::from_secs(3)),
        "cancel took {:?}",
        t0.elapsed()
    );
    assert_eq!(
        rows(&d, "SELECT 1").await.unwrap(),
        1,
        "the connection survives the cancel"
    );

    // The same settings reuse the tunnel.
    let t2 = tunnel::acquire(&rt, spec(&c, &kh)).await.unwrap();
    assert_eq!(t2.local_port(), t.local_port());

    // The SSH server goes away and comes back on the same port: the tunnel
    // reconnects on its own and the driver recovers on the next queries.
    srv.stop();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = t.sessions_opened();
    let _srv2 = start_server(KEY_A, ssh_port).await;
    let mut ok = false;
    for _ in 0..10 {
        if rows(&d, "SELECT 1").await.is_ok() {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(ok, "queries did not recover after the SSH server came back");
    assert!(
        t.sessions_opened() > before,
        "the SSH session was not reopened"
    );

    // A dropped session alone (no server restart) is reopened on demand too.
    t.drop_session().await;
    let n = t.sessions_opened();
    let mut ok = false;
    for _ in 0..10 {
        if rows(&d, "SELECT 1").await.is_ok() {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(ok);
    let _ = n;
    tunnel::close(&rt, "t-main");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anydriver_connects_through_the_tunnel() {
    // Same path the app takes: SavedConnection with `ssh` -> AnyDriver::connect.
    let Some((base, pw)) = db() else {
        return eprintln!("skipped: no BENCH_DSN");
    };
    let rt = tokio::runtime::Handle::current();
    let srv = start_server(KEY_A, 0).await;
    let dir = std::env::temp_dir().join(format!("wb-ssh-data-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let kh = dir.join("known_hosts");
    trust(&kh, srv.port).await;
    let c = ssh_conn(&base, "t-any", srv.port, "pw");
    let d = AnyDriver::connect_with_known_hosts(&rt, &c, pw, Default::default(), Some(kh.clone()))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(rows(&d, "SELECT 1").await.unwrap(), 1);

    // Wrong SSH password: the SSH step fails, the message says so.
    let bad = ssh_conn(&base, "t-any-bad", srv.port, "nope");
    let e = AnyDriver::connect_with_known_hosts(
        &rt,
        &bad,
        String::new(),
        Default::default(),
        Some(kh.clone()),
    )
    .await
    .err()
    .unwrap();
    assert!(e.message.starts_with("SSH tunnel: login failed"), "{e}");
    // Wrong database password: the database step fails, the tunnel is up.
    let e = AnyDriver::connect_with_known_hosts(
        &rt,
        &c,
        "wrong".into(),
        Default::default(),
        Some(kh.clone()),
    )
    .await
    .err()
    .unwrap();
    assert!(e.message.contains("database step"), "{e}");
    tunnel::close(&rt, "t-any");
    tunnel::close(&rt, "t-any-bad");
}

#[tokio::test]
async fn host_key_mismatch_is_a_hard_error() {
    let Some((base, _)) = db() else {
        return eprintln!("skipped: no BENCH_DSN");
    };
    let rt = tokio::runtime::Handle::current();
    let kh = known_hosts_file("mismatch");
    let a = start_server(KEY_A, 0).await;
    let fp_a = trust(&kh, a.port).await;
    // A different server (other key) now answers on the trusted host:port.
    a.stop();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let _b = start_server(KEY_B, a.port).await;
    let c = ssh_conn(&base, "t-mm", a.port, "pw");
    let e = tunnel::acquire(&rt, spec(&c, &kh))
        .await
        .err()
        .expect("a changed key must fail");
    assert_eq!(e.code, Some(CODE_HOST_CHANGED), "{e}");
    assert!(
        e.message.contains("HOST KEY CHANGED") && e.message.contains(&fp_a),
        "{e}"
    );
    // It is not recorded as pending, so "trust" cannot be used to accept it by accident.
    assert!(tunnel::trust_pending(&kh, "127.0.0.1", a.port).is_err());
    let _ = fingerprint;
}

#[tokio::test]
async fn unreachable_ssh_host_names_the_ssh_step() {
    let Some((base, _)) = db() else {
        return eprintln!("skipped: no BENCH_DSN");
    };
    let rt = tokio::runtime::Handle::current();
    let kh = known_hosts_file("down");
    // A port nothing listens on.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let c = ssh_conn(&base, "t-down", port, "pw");
    let e = tunnel::acquire(&rt, spec(&c, &kh)).await.err().unwrap();
    assert!(
        e.message.starts_with("SSH tunnel: could not connect"),
        "{e}"
    );
}

/// A timing limit scaled by `LOCUS_TEST_SLACK` (default 1; CI raises it
/// because shared runners are slower and noisier).
fn slack(d: std::time::Duration) -> std::time::Duration {
    let k = std::env::var("LOCUS_TEST_SLACK")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .map_or(1.0, |v| v.max(1.0));
    d.mul_f64(k)
}
