//! The hello handshake against the real sidecar (`sidecar/build.ps1`); no
//! database needed.

mod common;

use common::sidecar_exe;

#[test]
fn probe_accepts_the_sidecar_built_from_this_checkout() {
    let exe = sidecar_exe();
    assert!(
        exe.exists(),
        "{} is missing: run sidecar/build.ps1",
        exe.display()
    );
    let h = db_sidecar_client::probe(&exe).unwrap();
    assert_eq!(h.build, db_sidecar_client::BUILD);
}

fn nowhere() -> db_core::ConnectOptions {
    db_core::ConnectOptions {
        host: "127.0.0.1".into(),
        port: 1, // nothing listens on loopback port 1
        user: "u".into(),
        password: "p".into(),
        database: None,
        tls: db_core::TlsMode::Disabled,
    }
}

/// The hello frame passes, then the sidecar reports the failed connect:
/// the error is the database's (Connect), not a handshake one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_reads_hello_before_ready() {
    let r = db_sidecar_client::SidecarDriver::spawn(
        &tokio::runtime::Handle::current(),
        sidecar_exe(),
        nowhere(),
    )
    .await;
    let e = r.err().expect("nothing listens on port 1");
    assert_eq!(e.kind, db_core::ErrorKind::Connect, "{e}");
}

/// `SIDECAR_OLD_EXE`: a sidecar built before the hello frame. Skipped when
/// unset (CI builds only the current one).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_refuses_an_old_sidecar_before_it_connects() {
    let Some(old) = std::env::var_os("SIDECAR_OLD_EXE") else {
        return;
    };
    let r = db_sidecar_client::SidecarDriver::spawn(
        &tokio::runtime::Handle::current(),
        old.into(),
        nowhere(),
    )
    .await;
    let e = r.err().expect("an old sidecar must be refused");
    assert_eq!(e.kind, db_core::ErrorKind::Internal, "{e}");
    assert!(e.message.contains("older than"), "{e}");
}

#[test]
fn probe_refuses_something_that_is_not_a_sidecar() {
    // cmd.exe ignores --hello and prints no frame.
    let cmd = std::path::PathBuf::from(std::env::var_os("ComSpec").unwrap());
    let e = db_sidecar_client::probe(&cmd).unwrap_err();
    assert!(e.message.contains("hello"), "{}", e.message);
}
