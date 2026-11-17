//! Shared helpers for the live tests. The password comes from `PG_TEST_PASSWORD`
//! (CI) or else Windows Credential Manager (generic credential `Locus/test/postgres`) and is
//! never printed or written anywhere. Tests are skipped when it is missing.
#![allow(dead_code)]

use db_core::{ConnectOptions, Driver, Outcome, QueryEvent, QueryHandle, QueryOptions, TlsMode};
use db_postgres::PgDriver;
use std::ptr::null_mut;
use std::time::Duration;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Security::Credentials::{
    CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
};

fn password() -> Option<String> {
    if let Ok(p) = std::env::var("PG_TEST_PASSWORD") {
        return Some(p);
    }
    let target: Vec<u16> = "Locus/test/postgres"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut p: *mut CREDENTIALW = null_mut();
    if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) } == 0 {
        let _ = unsafe { GetLastError() };
        return None;
    }
    unsafe {
        let c = &*p;
        let bytes = if c.CredentialBlob.is_null() {
            &[][..]
        } else {
            std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize)
        };
        // `cmdkey` stores UTF-16, the app stores UTF-8: accept both.
        let utf16 = bytes.len() >= 2
            && bytes.len() % 2 == 0
            && bytes.iter().skip(1).step_by(2).all(|b| *b == 0);
        let s = if utf16 {
            String::from_utf16_lossy(
                &bytes
                    .chunks(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect::<Vec<_>>(),
            )
        } else {
            String::from_utf8_lossy(bytes).into_owned()
        };
        CredFree(p as *const _);
        Some(
            s.trim_end_matches(|c: char| c.is_ascii_control())
                .to_string(),
        )
    }
}

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

/// Connects to the test server, or `None` (with a message) when the
/// credential is missing. Tries the database names and TLS modes in turn
/// unless `PG_TEST_DB` / `PG_TEST_TLS` fix them.
pub async fn connect() -> Option<PgDriver> {
    let Some(password) = password() else {
        eprintln!(
            "SKIPPED: no `PG_TEST_PASSWORD` and no Windows credential `Locus/test/postgres`; live PostgreSQL tests did not run"
        );
        return None;
    };
    let dbs: Vec<String> = match std::env::var("PG_TEST_DB") {
        Ok(d) => vec![d],
        Err(_) => vec!["blog".into(), "postgres".into()],
    };
    let modes: Vec<TlsMode> = match std::env::var("PG_TEST_TLS").as_deref() {
        Ok("require") => vec![TlsMode::Require],
        Ok("require-unverified") => vec![TlsMode::RequireUnverified],
        Ok("disabled") => vec![TlsMode::Disabled],
        _ => vec![TlsMode::RequireUnverified, TlsMode::Disabled],
    };
    let mut last = String::new();
    for db in &dbs {
        for tls in &modes {
            let opts = ConnectOptions {
                host: env("PG_TEST_HOST", "127.0.0.1"),
                port: env("PG_TEST_PORT", "5434").parse().unwrap(),
                user: env("PG_TEST_USER", "blog"),
                password: password.clone(),
                database: Some(db.clone()),
                tls: *tls,
            };
            match PgDriver::connect(&tokio::runtime::Handle::current(), opts).await {
                Ok(d) => {
                    println!("connected: database {db:?}, tls {tls:?}");
                    return Some(d);
                }
                Err(e) => last = format!("db {db:?} tls {tls:?}: {:?} {}", e.kind, e.message),
            }
        }
    }
    panic!("cannot connect to the test server: {last}");
}

pub fn opts() -> QueryOptions {
    QueryOptions::default()
}

pub struct Got {
    pub columns: Vec<Vec<db_core::ColumnMeta>>,
    /// Rows per result set as text cells (None = NULL).
    pub sets: Vec<Vec<Vec<Option<String>>>>,
    pub affected: Vec<u64>,
    pub outcome: Outcome,
    pub session: u64,
}

pub async fn collect(h: &mut QueryHandle) -> Got {
    let mut g = Got {
        columns: vec![],
        sets: vec![],
        affected: vec![],
        outcome: Outcome::Cancelled,
        session: 0,
    };
    loop {
        match tokio::time::timeout(Duration::from_secs(60), h.next())
            .await
            .expect("no event in 60 s")
        {
            Some(QueryEvent::Started { session_id }) => g.session = session_id,
            Some(QueryEvent::Columns(c)) => {
                g.columns.push(c);
                g.sets.push(vec![]);
            }
            Some(QueryEvent::Batch(b)) => {
                let set = g.sets.last_mut().expect("batch before columns");
                for r in 0..b.rows() {
                    set.push(
                        (0..b.columns())
                            .map(|c| match b.cell(r, c) {
                                db_core::Cell::Null => None,
                                db_core::Cell::Text(s) => Some(s.to_string()),
                                db_core::Cell::Bytes(x) => Some(format!("bytes:{}", x.len())),
                            })
                            .collect(),
                    );
                }
            }
            Some(QueryEvent::Affected { rows }) => g.affected.push(rows),
            Some(QueryEvent::Finished(o)) => {
                g.outcome = o;
                return g;
            }
            None => panic!("stream ended without Finished"),
        }
    }
}

pub async fn run(d: &impl Driver, sql: &str) -> Got {
    collect(&mut d.query(sql.into(), opts())).await
}

pub async fn ok(d: &impl Driver, sql: &str) -> Got {
    let g = run(d, sql).await;
    assert!(
        matches!(g.outcome, Outcome::Done { .. }),
        "{sql}: {:?}",
        g.outcome
    );
    g
}

pub async fn scalar(d: &impl Driver, sql: &str) -> String {
    ok(d, sql).await.sets[0][0][0]
        .clone()
        .unwrap_or_else(|| "<NULL>".into())
}
