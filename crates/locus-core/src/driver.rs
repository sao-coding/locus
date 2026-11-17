//! The driver of one open connection: MySQL / MariaDB in process, or Oracle
//! through the Go sidecar. Everything above this file works on `AnyDriver`
//! through the `Driver` trait.

use crate::store::{DbKind, SavedConnection};
use db_core::{Catalog, DbError, Driver, ErrorKind, Param, QueryHandle, QueryOptions};
use db_mysql::MysqlDriver;
use db_sidecar_client::SidecarDriver;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub enum AnyDriver {
    Mysql(MysqlDriver),
    Postgres(db_postgres::PgDriver),
    Oracle(SidecarDriver),
    Sqlite(db_sqlite::SqliteDriver),
}

/// Tunnel settings of a connection, with the SSH secret taken from the form
/// (`ssh.secret`) or Credential Manager. `None` for SQLite and when SSH is off.
fn tunnel_spec(
    conn: &SavedConnection,
    known_hosts: Option<PathBuf>,
) -> Option<crate::tunnel::TunnelSpec> {
    let mut ssh = conn.ssh.clone()?;
    if conn.kind == DbKind::Sqlite {
        return None;
    }
    if ssh.secret.is_none() && ssh.auth != crate::sshcfg::SshAuth::Agent {
        ssh.secret = crate::creds::read(&crate::sshcfg::SshConfig::secret_id(&conn.id))
            .ok()
            .flatten();
    }
    Some(crate::tunnel::TunnelSpec {
        conn_id: conn.id.clone(),
        ssh,
        target_host: conn.host.clone(),
        target_port: conn.port,
        known_hosts: known_hosts.unwrap_or_default(),
    })
}

/// Where `sidecar.exe` is: `LOCUS_SIDECAR`, next to `locus.exe`, or
/// the build output of this checkout (development).
pub fn sidecar_exe() -> Result<PathBuf, DbError> {
    sidecar_exe_from(std::env::var_os("LOCUS_SIDECAR").as_deref())
}

/// [`sidecar_exe`] with the value of `LOCUS_SIDECAR` passed in.
fn sidecar_exe_from(env_value: Option<&std::ffi::OsStr>) -> Result<PathBuf, DbError> {
    let mut tried = Vec::new();
    let mut candidates = Vec::new();
    if let Some(p) = env_value {
        candidates.push(PathBuf::from(p));
    }
    if let Ok(me) = std::env::current_exe()
        && let Some(dir) = me.parent()
    {
        candidates.push(dir.join("sidecar.exe"));
    }
    candidates
        .push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sidecar/bin/sidecar.exe"));
    for c in candidates {
        if c.is_file() {
            return Ok(c);
        }
        tried.push(c.display().to_string());
    }
    Err(DbError::new(
        ErrorKind::Internal,
        format!("sidecar.exe not found (looked in: {})", tried.join(", ")),
    ))
}

impl AnyDriver {
    pub fn connect(
        rt: &tokio::runtime::Handle,
        conn: &SavedConnection,
        password: String,
        ro: Arc<AtomicBool>,
    ) -> impl std::future::Future<Output = Result<AnyDriver, DbError>> + Send + 'static {
        Self::connect_with_known_hosts(
            rt,
            conn,
            password,
            ro,
            crate::sshcfg::default_known_hosts_path(),
        )
    }

    /// [`connect`](Self::connect) with the SSH `known_hosts` file given
    /// instead of taken from the app data folder.
    pub fn connect_with_known_hosts(
        rt: &tokio::runtime::Handle,
        conn: &SavedConnection,
        password: String,
        ro: Arc<AtomicBool>,
        known_hosts: Option<PathBuf>,
    ) -> impl std::future::Future<Output = Result<AnyDriver, DbError>> + Send + 'static {
        let mut opts = conn.options(password);
        let tunnel_spec = tunnel_spec(conn, known_hosts);
        let rt = rt.clone();
        let kind = conn.kind;
        let path = conn.database.clone().unwrap_or_default();
        let read_only = conn.read_only;
        async move {
            // SSH step first: the driver then connects to the local end of the tunnel.
            let tunnel = match tunnel_spec {
                Some(spec) => {
                    let t = crate::tunnel::acquire(&rt, spec).await?;
                    opts.host = "127.0.0.1".into();
                    opts.port = t.local_port();
                    Some(t)
                }
                None => None,
            };
            let d = match kind {
                DbKind::Mysql => MysqlDriver::connect(&rt, opts).await.map(AnyDriver::Mysql),
                DbKind::Postgres => db_postgres::PgDriver::connect(&rt, opts)
                    .await
                    .map(AnyDriver::Postgres),
                DbKind::Sqlite => db_sqlite::SqliteDriver::connect(path, read_only)
                    .await
                    .map(AnyDriver::Sqlite),
                // Redis is not SQL: `crate::redis` and `db_redis` are used directly.
                DbKind::Redis => Err(DbError::new(
                    ErrorKind::Internal,
                    "Redis connections have no SQL driver",
                )),
                DbKind::Oracle => {
                    let exe = sidecar_exe()?;
                    SidecarDriver::spawn_driver(&rt, exe, "oracle", opts)
                        .await
                        .map(AnyDriver::Oracle)
                }
            }
            .map_err(|mut e| {
                if tunnel.is_some() {
                    e.message = format!("database step (the SSH tunnel is up): {}", e.message);
                }
                e
            })?;
            // Read at the end of the connect, so an unlock that happened while
            // connecting is not missed.
            if ro.load(Ordering::SeqCst) {
                d.set_read_only(true);
            }
            Ok(d)
        }
    }

    /// Server-side read-only mode for every session this driver hands out
    /// (SQLite: `PRAGMA query_only`)
    /// (MySQL / MariaDB, PostgreSQL). Oracle has no session-wide default: the
    /// classifier is its only guard.
    pub fn set_read_only(&self, on: bool) {
        match self {
            AnyDriver::Mysql(d) => d.set_read_only(on),
            AnyDriver::Postgres(d) => d.set_read_only(on),
            AnyDriver::Sqlite(d) => d.set_read_only(on),
            AnyDriver::Oracle(_) => {}
        }
    }

    pub fn is_sqlite(&self) -> bool {
        matches!(self, AnyDriver::Sqlite(_))
    }

    pub fn is_oracle(&self) -> bool {
        matches!(self, AnyDriver::Oracle(_))
    }

    /// One pinned server connection (for a transaction).
    pub async fn session(&self) -> Result<Box<dyn Driver>, DbError> {
        match self {
            AnyDriver::Mysql(d) => Ok(Box::new(d.session().await?)),
            AnyDriver::Postgres(d) => Ok(Box::new(d.session().await?)),
            AnyDriver::Oracle(d) => Ok(Box::new(d.session().await?)),
            AnyDriver::Sqlite(d) => Ok(Box::new(d.session().await?)),
        }
    }

    fn inner(&self) -> &dyn Driver {
        match self {
            AnyDriver::Mysql(d) => d,
            AnyDriver::Postgres(d) => d,
            AnyDriver::Oracle(d) => d,
            AnyDriver::Sqlite(d) => d,
        }
    }
}

impl Driver for AnyDriver {
    fn kind(&self) -> &'static str {
        self.inner().kind()
    }
    fn query(&self, sql: String, opts: QueryOptions) -> QueryHandle {
        self.inner().query(sql, opts)
    }
    fn kill_session(&self, session_id: u64) -> QueryHandle {
        self.inner().kill_session(session_id)
    }
    fn execute(&self, sql: String, params: Vec<Param>, opts: QueryOptions) -> QueryHandle {
        self.inner().execute(sql, params, opts)
    }
    fn quote_identifier(&self, name: &str) -> String {
        self.inner().quote_identifier(name)
    }
    fn catalog(&self) -> Option<&dyn Catalog> {
        self.inner().catalog()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Tls;

    /// Read only: `SELECT` of the version. `cargo test -p app oracle_connect -- --ignored`
    /// with ORACLE_TEST_HOST / PORT / SERVICE / USER / PASSWORD set.
    #[tokio::test]
    #[ignore = "needs a live Oracle server"]
    async fn oracle_connect() {
        let env = |n: &str| std::env::var(n).unwrap_or_else(|_| panic!("{n} is not set"));
        let conn = SavedConnection {
            env: Default::default(),
            kind: DbKind::Oracle,
            id: "t".into(),
            name: "t".into(),
            host: env("ORACLE_TEST_HOST"),
            port: env("ORACLE_TEST_PORT").parse().unwrap(),
            user: env("ORACLE_TEST_USER"),
            database: Some(env("ORACLE_TEST_SERVICE")),
            tls: Tls::Disabled,
            color: None,
            icon: None,
            read_only: false,
            ssh: None,
        };
        let d = AnyDriver::connect(
            &tokio::runtime::Handle::current(),
            &conn,
            env("ORACLE_TEST_PASSWORD"),
            Default::default(),
        )
        .await
        .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(d.is_oracle() && d.catalog().is_some());
        let mut h = d.query("SELECT 1 FROM DUAL".into(), Default::default());
        let mut rows = 0;
        while let Some(ev) = h.next().await {
            if let db_core::QueryEvent::Batch(b) = ev {
                rows += b.rows();
            }
        }
        assert_eq!(rows, 1);
    }
}

#[cfg(test)]
mod wiring_tests {
    use super::*;
    use crate::store::Tls;

    /// No network: the sidecar path comes from LOCUS_SIDECAR first, and an
    /// Oracle connection's service name travels as the database option.
    #[test]
    fn oracle_wiring_without_a_server() {
        let dir = std::env::temp_dir().join("wb-sidecar-test");
        let _ = std::fs::create_dir_all(&dir);
        let exe = dir.join("sidecar.exe");
        std::fs::write(&exe, b"not really").unwrap();
        assert_eq!(sidecar_exe_from(Some(exe.as_os_str())).unwrap(), exe);
        let c = SavedConnection {
            env: Default::default(),
            kind: DbKind::Oracle,
            id: "o".into(),
            name: "o".into(),
            host: "db.example".into(),
            port: DbKind::Oracle.default_port(),
            user: "u".into(),
            database: Some("ORCLPDB1".into()),
            tls: Tls::Disabled,
            color: None,
            icon: None,
            read_only: false,
            ssh: None,
        };
        let o = c.options("pw".into());
        assert_eq!((o.port, o.database.as_deref()), (1521, Some("ORCLPDB1")));
        let _ = std::fs::remove_file(exe);
    }
}

/// Creates an empty SQLite database file (the connection form's "New file").
/// Fails when the file exists, so nothing is ever overwritten.
pub fn create_sqlite_file(path: &std::path::Path) -> Result<(), DbError> {
    db_sqlite::create_database(path)
}
