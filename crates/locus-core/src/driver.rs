//! The driver of one open connection: MySQL / MariaDB in process, or Oracle
//! through the Go sidecar. Everything above this file works on `AnyDriver`
//! through the `Driver` trait.

use crate::store::{DbKind, SavedConnection};
use db_core::{Catalog, DbError, Driver, ErrorKind, Param, QueryHandle, QueryOptions};
use db_mysql::MysqlDriver;
use db_sidecar_client::SidecarDriver;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub enum AnyDriver {
    Mysql(MysqlDriver),
    Postgres(db_postgres::PgDriver),
    Oracle(SidecarDriver),
    SqlServer(SidecarDriver),
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

/// The `sidecar.exe` bytes compiled into `locus.exe`, set once at startup
/// by the app. Unset in builds made without `sidecar/bin/sidecar.exe`.
static EMBEDDED_SIDECAR: std::sync::OnceLock<&'static [u8]> = std::sync::OnceLock::new();

/// Hands the embedded `sidecar.exe` to the driver layer. Only stores the
/// slice; nothing is written until Oracle is first used.
pub fn set_embedded_sidecar(bytes: &'static [u8]) {
    if !bytes.is_empty() {
        let _ = EMBEDDED_SIDECAR.set(bytes);
    }
}

/// Where `sidecar.exe` is: `LOCUS_SIDECAR`, next to `locus.exe`, the copy
/// embedded in `locus.exe` (unpacked once into `%LOCALAPPDATA%\Locus\sidecar`),
/// or the build output of this checkout (development).
///
/// A `sidecar.exe` next to `locus.exe` is used only when its hello frame
/// matches this build (an older one left behind by an update would
/// otherwise win over the embedded copy); otherwise the embedded copy is
/// used and the reason logged. `LOCUS_SIDECAR` is taken as given: the
/// handshake still refuses a mismatch, with the reason.
pub fn sidecar_exe() -> Result<PathBuf, DbError> {
    let me = std::env::current_exe().ok();
    let local =
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Locus").join("sidecar"));
    resolve_sidecar(
        std::env::var_os("LOCUS_SIDECAR").as_deref(),
        me.as_deref().and_then(|m| m.parent()),
        EMBEDDED_SIDECAR.get().copied(),
        local.as_deref(),
        &cached_probe,
    )
}

/// [`db_sidecar_client::probe`], remembered per file (path, size, modified
/// time) so a connect does not start an extra process each time.
fn cached_probe(exe: &Path) -> Result<(), DbError> {
    type Key = (PathBuf, u64, Option<std::time::SystemTime>);
    static SEEN: std::sync::Mutex<Option<(Key, Result<(), String>)>> = std::sync::Mutex::new(None);
    let meta = std::fs::metadata(exe).ok();
    let key = (
        exe.to_path_buf(),
        meta.as_ref().map_or(0, |m| m.len()),
        meta.and_then(|m| m.modified().ok()),
    );
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let result = match seen.as_ref() {
        Some((k, r)) if *k == key => r.clone(),
        _ => {
            let r = db_sidecar_client::probe(exe)
                .map(drop)
                .map_err(|e| e.message);
            *seen = Some((key, r.clone()));
            r
        }
    };
    result.map_err(|m| DbError::new(ErrorKind::Internal, m))
}

/// [`sidecar_exe`] with every input passed in. `probe` checks a
/// `sidecar.exe` found next to `locus.exe` when there is an embedded copy
/// to fall back on.
fn resolve_sidecar(
    env_value: Option<&std::ffi::OsStr>,
    exe_dir: Option<&Path>,
    embedded: Option<&[u8]>,
    extract_dir: Option<&Path>,
    probe: &dyn Fn(&Path) -> Result<(), DbError>,
) -> Result<PathBuf, DbError> {
    let mut tried = Vec::new();
    if let Some(p) = env_value {
        let c = PathBuf::from(p);
        if c.is_file() {
            return Ok(c);
        }
        tried.push(c.display().to_string());
    }
    let embedded = embedded.filter(|b| !b.is_empty());
    if let Some(dir) = exe_dir {
        let c = dir.join("sidecar.exe");
        if c.is_file() {
            match probe(&c) {
                Ok(()) => return Ok(c),
                // Nothing to fall back on: the handshake reports it.
                Err(_) if embedded.is_none() => return Ok(c),
                Err(e) => log::warn!(
                    "not using {}: {}; using the sidecar built into locus.exe",
                    c.display(),
                    e.message
                ),
            }
        }
        tried.push(c.display().to_string());
    }
    if let Some(bytes) = embedded {
        let dir = extract_dir.ok_or_else(|| {
            DbError::new(
                ErrorKind::Internal,
                "cannot unpack the embedded sidecar: LOCALAPPDATA is not set",
            )
        })?;
        return extract_sidecar(dir, bytes);
    }
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sidecar/bin/sidecar.exe");
    if dev.is_file() {
        return Ok(dev);
    }
    tried.push(dev.display().to_string());
    Err(DbError::new(
        ErrorKind::Internal,
        format!("sidecar.exe not found (looked in: {})", tried.join(", ")),
    ))
}

/// Writes `bytes` to `dir\sidecar-<12 hex of SHA-256>.exe` unless a file of
/// that name and size is already there (it may be running in another Locus).
/// The write goes to a temp file first, then a rename. Older `sidecar-*.exe`
/// files are removed best-effort; one in use just fails to delete.
fn extract_sidecar(dir: &Path, bytes: &[u8]) -> Result<PathBuf, DbError> {
    use sha2::{Digest, Sha256};
    let fail = |what: &str, target: &Path, e: std::io::Error| {
        DbError::new(
            ErrorKind::Internal,
            format!(
                "cannot unpack the database helper ({what} {}): {e}",
                target.display()
            ),
        )
    };
    let name = format!("sidecar-{}.exe", &hex::encode(Sha256::digest(bytes))[..12]);
    let target = dir.join(&name);
    let right_size =
        |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() == bytes.len() as u64);
    if !right_size(&target) {
        std::fs::create_dir_all(dir).map_err(|e| fail("creating", dir, e))?;
        let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, bytes).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            fail("writing", &tmp, e)
        })?;
        if let Err(e) = std::fs::rename(&tmp, &target) {
            let _ = std::fs::remove_file(&tmp);
            // Another instance may have put the same file there meanwhile.
            if !right_size(&target) {
                return Err(fail("replacing", &target, e));
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name();
            let n = n.to_string_lossy();
            if n != name && n.starts_with("sidecar-") && n.ends_with(".exe") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(target)
}

#[cfg(test)]
mod sidecar_embed_tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("locus-sc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn extract_writes_file_named_by_hash() {
        let d = temp("write");
        let p = extract_sidecar(&d.join("sub"), b"hello sidecar").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"hello sidecar");
        let n = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(n.starts_with("sidecar-") && n.ends_with(".exe") && n.len() == 24);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn extract_is_idempotent() {
        let d = temp("idem");
        let p = extract_sidecar(&d, b"abc123").unwrap();
        let m1 = std::fs::metadata(&p).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(extract_sidecar(&d, b"abc123").unwrap(), p);
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), m1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn extract_replaces_wrong_size_and_removes_old_hashes() {
        let d = temp("wrong");
        let p = extract_sidecar(&d, b"abc123").unwrap();
        std::fs::write(&p, b"x").unwrap();
        let old = d.join("sidecar-000000000000.exe");
        std::fs::write(&old, b"old").unwrap();
        assert_eq!(extract_sidecar(&d, b"abc123").unwrap(), p);
        assert_eq!(std::fs::read(&p).unwrap(), b"abc123");
        assert!(!old.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn extract_unwritable_folder_names_the_path() {
        let d = temp("ro");
        let blocker = d.join("file");
        std::fs::write(&blocker, b"x").unwrap();
        let e = extract_sidecar(&blocker.join("sidecar"), b"abc").unwrap_err();
        assert!(e.message.contains("cannot unpack"), "{}", e.message);
        assert!(e.message.contains("file"), "{}", e.message);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn resolution_order_env_then_exe_dir_then_embedded() {
        let d = temp("order");
        let env_file = d.join("env.exe");
        let exe_dir = d.join("bin");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::write(&env_file, b"e").unwrap();
        std::fs::write(exe_dir.join("sidecar.exe"), b"n").unwrap();
        let ex = d.join("ex");
        let r = |env: Option<&std::ffi::OsStr>| {
            resolve_sidecar(env, Some(&exe_dir), Some(b"emb"), Some(&ex), &|_| Ok(())).unwrap()
        };
        assert_eq!(r(Some(env_file.as_os_str())), env_file);
        assert_eq!(r(None), exe_dir.join("sidecar.exe"));
        std::fs::remove_file(exe_dir.join("sidecar.exe")).unwrap();
        let p = r(None);
        assert!(p.starts_with(&ex) && p.is_file());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn stale_sidecar_next_to_the_exe_gives_way_to_the_embedded_one() {
        let d = temp("stale");
        let exe_dir = d.join("bin");
        std::fs::create_dir_all(&exe_dir).unwrap();
        let beside = exe_dir.join("sidecar.exe");
        std::fs::write(&beside, b"old").unwrap();
        let ex = d.join("ex");
        let stale = |_: &Path| Err(DbError::new(ErrorKind::Internal, "build 0.0.1"));

        let p = resolve_sidecar(None, Some(&exe_dir), Some(b"emb"), Some(&ex), &stale).unwrap();
        assert!(p.starts_with(&ex), "{}", p.display());
        assert_eq!(std::fs::read(&p).unwrap(), b"emb");

        // No embedded copy: still the one next to the exe (the handshake
        // then reports the mismatch).
        let p = resolve_sidecar(None, Some(&exe_dir), None, Some(&ex), &stale).unwrap();
        assert_eq!(p, beside);

        // LOCUS_SIDECAR is never probed.
        let env_file = d.join("env.exe");
        std::fs::write(&env_file, b"e").unwrap();
        let p = resolve_sidecar(
            Some(env_file.as_os_str()),
            Some(&exe_dir),
            Some(b"emb"),
            Some(&ex),
            &|_| panic!("probed"),
        )
        .unwrap();
        assert_eq!(p, env_file);
        let _ = std::fs::remove_dir_all(&d);
    }
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
                DbKind::SqlServer => {
                    let exe = sidecar_exe()?;
                    SidecarDriver::spawn_driver(&rt, exe, "mssql", opts)
                        .await
                        .map(AnyDriver::SqlServer)
                }
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
            // No session-wide read-only switch: the classifier is the guard.
            AnyDriver::SqlServer(_) => {}
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
            AnyDriver::SqlServer(d) => Ok(Box::new(d.session().await?)),
            AnyDriver::Sqlite(d) => Ok(Box::new(d.session().await?)),
        }
    }

    fn inner(&self) -> &dyn Driver {
        match self {
            AnyDriver::Mysql(d) => d,
            AnyDriver::Postgres(d) => d,
            AnyDriver::Oracle(d) => d,
            AnyDriver::SqlServer(d) => d,
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
            mcp: None,
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
        assert_eq!(
            resolve_sidecar(Some(exe.as_os_str()), None, None, None, &|_| Ok(())).unwrap(),
            exe
        );
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
            mcp: None,
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
