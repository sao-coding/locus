mod common;
use common::*;
use db_core::{ErrorKind, TlsMode};
use db_sidecar_client::SidecarDriver;
use std::time::Instant;

async fn spawn_err(opts: db_core::ConnectOptions) -> (db_core::DbError, f64) {
    let t0 = Instant::now();
    match SidecarDriver::spawn(&tokio::runtime::Handle::current(), sidecar_exe(), opts).await {
        Ok(_) => panic!("the sidecar connected, but the connection must fail"),
        Err(e) => (e, ms(t0.elapsed())),
    }
}

/// The local server offers TLS with a self-signed certificate: "require-unverified" connects.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_unverified_connects_to_a_self_signed_server() {
    use db_core::{Cell, Driver, Outcome, QueryEvent, QueryOptions};
    let d = SidecarDriver::spawn(
        &tokio::runtime::Handle::current(),
        sidecar_exe(),
        connect_options(TlsMode::RequireUnverified),
    )
    .await
    .expect("require-unverified should accept the local self-signed certificate");
    let mut h = d.query(
        "SHOW SESSION STATUS LIKE 'Ssl_cipher'".into(),
        QueryOptions::default(),
    );
    let mut cipher = String::new();
    while let Some(ev) = h.next().await {
        match ev {
            QueryEvent::Batch(b) => {
                if let Cell::Text(t) = b.cell(0, 1) {
                    cipher = t.to_string();
                }
            }
            QueryEvent::Finished(o) => assert!(matches!(o, Outcome::Done { .. }), "{o:?}"),
            _ => {}
        }
    }
    println!("[tls_unverified] session cipher = {cipher}");
    assert!(!cipher.is_empty(), "the session is not encrypted");
}

/// "require" verifies the certificate, so the local self-signed server is
/// refused; it must not fall back to plain text. (No server without any TLS
/// is available locally to test that path.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_required_rejects_a_self_signed_certificate() {
    let (e, t) = spawn_err(connect_options(TlsMode::Require)).await;
    println!("[tls_required] {e} ({t:.0} ms)");
    assert_eq!(e.kind, ErrorKind::Tls, "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_password_fails_with_auth() {
    let mut o = connect_options(TlsMode::Disabled);
    o.password = "definitely-not-the-password".into();
    let (e, t) = spawn_err(o).await;
    println!("[wrong_password] {e} ({t:.0} ms)");
    assert_eq!(e.kind, ErrorKind::Auth, "{e}");
    assert_eq!(e.code, Some(1045));
    assert!(
        !e.message.contains("definitely-not-the-password"),
        "the password must not appear in errors"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_server_fails_with_connect() {
    let mut o = connect_options(TlsMode::Disabled);
    o.port = 1; // nothing listens on loopback port 1
    let (e, t) = spawn_err(o).await;
    println!("[unreachable] {e} ({t:.0} ms)");
    assert_eq!(e.kind, ErrorKind::Connect, "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_executable_is_an_internal_error() {
    let r = SidecarDriver::spawn(
        &tokio::runtime::Handle::current(),
        "D:/no/such/sidecar.exe".into(),
        connect_options(TlsMode::Disabled),
    )
    .await;
    assert_eq!(r.err().map(|e| e.kind), Some(ErrorKind::Internal));
}
