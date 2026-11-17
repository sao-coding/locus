//! Table structure on Oracle through the sidecar. Ignored by default:
//! `cargo test -p db-sidecar-client --test structure_live -- --ignored`
//! READ ONLY: dictionary SELECT statements, each capped; picks one small table of the
//! connected user. Same ORACLE_TEST_* environment as `oracle_live.rs`.
mod common;
use common::*;
use db_core::{ConnectOptions, DbObjectKind, Driver, TlsMode};
use db_sidecar_client::SidecarDriver;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

#[tokio::test]
#[ignore]
async fn structure_of_a_user_table() {
    let opts = ConnectOptions {
        host: env("ORACLE_TEST_HOST"),
        port: env("ORACLE_TEST_PORT").parse().expect("ORACLE_TEST_PORT"),
        user: env("ORACLE_TEST_USER"),
        password: env("ORACLE_TEST_PASSWORD"),
        database: Some(env("ORACLE_TEST_SERVICE")),
        tls: TlsMode::Disabled,
    };
    let d = SidecarDriver::spawn_driver(
        &tokio::runtime::Handle::current(),
        sidecar_exe(),
        "oracle",
        opts,
    )
    .await
    .unwrap_or_else(|e| panic!("sidecar did not connect: {:?} {}", e.kind, e.message));
    let owner = std::env::var("ORACLE_TEST_OWNER")
        .unwrap_or_else(|_| env("ORACLE_TEST_USER").to_uppercase());
    let c = d.catalog().unwrap();
    let tables = c
        .list_objects(&owner, DbObjectKind::Table, 5)
        .await
        .unwrap();
    let t = tables.first().expect("no table in the schema").name.clone();
    let s = c.structure(&owner, &t, 200).await.unwrap();
    assert!(!s.columns.is_empty(), "{t}");
    assert!(s.columns.iter().all(|c| !c.type_name.is_empty()));
    println!(
        "{t}: {} columns, {} indexes, {} fks, {} referenced by, {} checks, {} triggers",
        s.columns.len(),
        s.indexes.len(),
        s.foreign_keys.len(),
        s.referenced_by.len(),
        s.checks.len(),
        s.triggers.len()
    );
    assert!(c.ddl(&owner, DbObjectKind::Table, &t).await.is_ok());
}
