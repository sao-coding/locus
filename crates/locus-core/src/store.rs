//! Saved connections: metadata in `%APPDATA%\Locus\connections.json`,
//! passwords in Credential Manager (see `creds`). The JSON never holds a
//! password field at all.

use crate::{creds, settings};
use db_core::{ConnectOptions, TlsMode};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tls {
    #[default]
    Required,
    /// Encrypted, certificate not verified.
    Unverified,
    Disabled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DbKind {
    #[default]
    Mysql,
    Postgres,
    /// Through the Go sidecar; `database` is the service name.
    Oracle,
    /// A file: the path is in `database`; no host, user or password.
    Sqlite,
    /// Microsoft SQL Server, through the Go sidecar (go-mssqldb).
    #[serde(rename = "sqlserver")]
    SqlServer,
    /// Not SQL: own views (`crates/app/src/redis`). `database` holds the DB index (0-15), `user` the ACL user.
    Redis,
}

impl DbKind {
    pub fn default_port(self) -> u16 {
        match self {
            DbKind::Mysql => 3306,
            DbKind::Postgres => 5432,
            DbKind::Oracle => 1521,
            DbKind::SqlServer => 1433,
            DbKind::Sqlite => 0,
            DbKind::Redis => 6379,
        }
    }
}

/// Environment tag of a connection, shown as a badge (no locking here).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Env {
    Local,
    #[default]
    Dev,
    Staging,
    Team,
    Prod,
}

impl Env {
    /// Badge text (the mockup's short forms).
    pub fn badge(self) -> &'static str {
        match self {
            Env::Local => "LOCAL",
            Env::Dev => "DEV",
            Env::Staging => "STG",
            Env::Team => "TEAM",
            Env::Prod => "PROD",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedConnection {
    #[serde(default)]
    pub kind: DbKind,
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(default)]
    pub tls: Tls,
    /// Accent colour `#rrggbb` used for the badge, tab marker and picker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Icon name (component icon set); `None` = initials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Environment tag (badge). Missing in older files = Dev.
    #[serde(default)]
    pub env: Env,
    /// SQLite only: open the file read-only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    /// Jump host the database is reached through (secret in Credential Manager).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<crate::sshcfg::SshConfig>,
}

impl SavedConnection {
    pub fn options(&self, password: String) -> ConnectOptions {
        ConnectOptions {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            password,
            database: self.database.clone(),
            tls: match self.tls {
                Tls::Required => TlsMode::Require,
                Tls::Unverified => TlsMode::RequireUnverified,
                Tls::Disabled => TlsMode::Disabled,
            },
        }
    }

    pub fn subtitle(&self) -> String {
        if self.kind == DbKind::Sqlite {
            return self.database.clone().unwrap_or_default();
        }
        if self.kind == DbKind::Redis {
            return format!(
                "{}:{}/{}",
                self.host,
                self.port,
                self.database.as_deref().unwrap_or("0")
            );
        }
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
}

/// Where connections go. Bench mode uses `Memory` so it never touches the
/// user's files or Credential Manager.
pub enum Store {
    Disk,
    Memory {
        passwords: Vec<(String, String)>,
        persist: bool,
    },
}

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Credential(creds::CredError),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "{e}"),
            StoreError::Credential(e) => write!(f, "{e}"),
        }
    }
}

impl Store {
    /// Whether Spaces and connections are written to disk. The demo scene
    /// with `LOCUS_DATA_DIR` set (UI tests) persists, with passwords in memory.
    pub fn persists(&self) -> bool {
        matches!(self, Store::Disk | Store::Memory { persist: true, .. })
    }

    pub fn load(&self) -> Vec<SavedConnection> {
        match self {
            Store::Memory { persist: false, .. } => Vec::new(),
            Store::Disk | Store::Memory { persist: true, .. } => {
                crate::datafile::load_named("connections.json").unwrap_or_default()
            }
        }
    }

    pub fn save_list(&self, list: &[SavedConnection]) -> Result<(), StoreError> {
        if !self.persists() {
            return Ok(());
        }
        let dir = settings::app_dir()
            .ok_or_else(|| StoreError::Io(std::io::Error::other("APPDATA is not set")))?;
        std::fs::create_dir_all(&dir).map_err(StoreError::Io)?;
        let bytes = serde_json::to_vec_pretty(list).map_err(|e| StoreError::Io(e.into()))?;
        settings::write_atomic(&dir.join("connections.json"), &bytes).map_err(StoreError::Io)
    }

    /// `None` keeps whatever password is stored.
    pub fn set_password(
        &mut self,
        c: &SavedConnection,
        password: Option<&str>,
    ) -> Result<(), StoreError> {
        // A SQLite file has no password: never store one.
        if c.kind == DbKind::Sqlite {
            return Ok(());
        }
        self.set_ssh_secret(c)?;
        let Some(pw) = password else { return Ok(()) };
        match self {
            Store::Disk => creds::write(&c.id, &c.user, pw).map_err(StoreError::Credential),
            Store::Memory { passwords, .. } => {
                passwords.retain(|(id, _)| id != &c.id);
                passwords.push((c.id.clone(), pw.to_string()));
                Ok(())
            }
        }
    }

    /// Stores the SSH password / passphrase typed in the form (Credential Manager only).
    fn set_ssh_secret(&mut self, c: &SavedConnection) -> Result<(), StoreError> {
        if let (Store::Disk, Some(s)) = (&*self, c.ssh.as_ref().and_then(|s| s.secret.as_deref())) {
            creds::write(&crate::sshcfg::SshConfig::secret_id(&c.id), &c.user, s)
                .map_err(StoreError::Credential)?;
        }
        Ok(())
    }

    pub fn password(&self, id: &str) -> Result<Option<String>, StoreError> {
        match self {
            Store::Disk => creds::read(id).map_err(StoreError::Credential),
            Store::Memory { passwords, .. } => Ok(passwords
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, p)| p.clone())),
        }
    }

    pub fn has_password(&self, id: &str) -> bool {
        matches!(self.password(id), Ok(Some(_)))
    }

    pub fn delete(&mut self, id: &str) -> Result<(), StoreError> {
        match self {
            Store::Disk => {
                if let Err(e) = creds::delete(&crate::sshcfg::SshConfig::secret_id(id)) {
                    log::warn!("ssh secret of connection {id} could not be deleted: {e}");
                }
                creds::delete(id).map_err(StoreError::Credential)
            }
            Store::Memory { passwords, .. } => {
                passwords.retain(|(i, _)| i != id);
                Ok(())
            }
        }
    }
}

/// A new opaque id (UUID v7, 32 hex digits, time-ordered). Ids written by older
/// builds are other opaque strings and stay valid.
pub fn new_id() -> String {
    uuid::Uuid::now_v7().simple().to_string()
}

/// Parses `mysql://user:pass@host:port/db` (bench mode). Returns the
/// connection and its password separately.
pub fn parse_dsn(dsn: &str) -> Option<(SavedConnection, String)> {
    parse_url(dsn).filter(|(c, _)| c.kind == DbKind::Mysql)
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parses a pasted connection URL: `mysql://` / `mariadb://`,
/// `postgres://` / `postgresql://`, `oracle://user:pass@host:port/SERVICE`.
/// User and password may be percent-encoded; the port defaults per engine.
pub fn parse_url(url: &str) -> Option<(SavedConnection, String)> {
    let url = url.trim();
    if let Some(path) = db_sqlite::path_from_url(url) {
        return Some((sqlite_connection(&path), String::new()));
    }
    let (scheme, rest) = url.split_once("://")?;
    let kind = match scheme.to_ascii_lowercase().as_str() {
        "mysql" | "mariadb" => DbKind::Mysql,
        "postgres" | "postgresql" => DbKind::Postgres,
        "oracle" => DbKind::Oracle,
        "sqlserver" | "mssql" => DbKind::SqlServer,
        "redis" | "rediss" => DbKind::Redis,
        _ => return None,
    };
    let (auth, hostpart) = match rest.rsplit_once('@') {
        Some((a, h)) => (a, h),
        None => ("", rest),
    };
    let (user, pass) = auth.split_once(':').unwrap_or((auth, ""));
    let (hostport, db) = hostpart.split_once('/').unwrap_or((hostpart, ""));
    let db = db.split('?').next().unwrap_or("");
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (hostport, kind.default_port()),
    };
    if host.is_empty() {
        return None;
    }
    let c = SavedConnection {
        env: Default::default(),
        kind,
        id: "bench".into(),
        name: "bench".into(),
        host: host.into(),
        port,
        user: pct_decode(user),
        database: (!db.is_empty()).then(|| pct_decode(db)),
        tls: Tls::Disabled,
        color: None,
        icon: None,
        read_only: false,
        ssh: None,
    };
    let mut c = c;
    if scheme.eq_ignore_ascii_case("rediss") {
        c.tls = Tls::Required;
    }
    if kind == DbKind::Redis {
        // `redis://:pw/3`: the path is the DB index (0-15).
        c.database = Some(
            db.parse::<u8>()
                .ok()
                .filter(|d| *d <= 15)
                .unwrap_or(0)
                .to_string(),
        );
        c.name = host.into();
    }
    Some((c, pct_decode(pass)))
}

/// A SQLite connection for a file path: no host, user, port or password.
pub fn sqlite_connection(path: &str) -> SavedConnection {
    let name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    SavedConnection {
        env: Default::default(),
        kind: DbKind::Sqlite,
        id: "bench".into(),
        name,
        host: String::new(),
        port: 0,
        user: String::new(),
        database: Some(path.to_string()),
        tls: Tls::Disabled,
        color: None,
        icon: None,
        read_only: false,
        ssh: None,
    }
}

#[cfg(test)]
mod url_tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn urls() {
        let (c, p) = parse_url("postgresql://bob:p%40ss@db.local/app?sslmode=require").unwrap();
        assert_eq!(
            (
                c.kind,
                c.port,
                c.user.as_str(),
                p.as_str(),
                c.database.as_deref()
            ),
            (DbKind::Postgres, 5432, "bob", "p@ss", Some("app"))
        );
        let (c, _) = parse_url("oracle://scott:x@10.0.0.1:1526/ORCLPDB1").unwrap();
        assert_eq!(
            (c.kind, c.port, c.database.as_deref()),
            (DbKind::Oracle, 1526, Some("ORCLPDB1"))
        );
        let (c, _) = parse_url("mariadb://root@127.0.0.1").unwrap();
        assert_eq!(
            (c.kind, c.port, c.user.as_str()),
            (DbKind::Mysql, 3306, "root")
        );
        assert!(parse_url("http://x").is_none());
    }

    #[::core::prelude::v1::test]
    fn sqlite_urls_and_no_password() {
        let (c, p) = parse_url("sqlite:///C:/data/my%20file.db").unwrap();
        assert_eq!(
            (c.kind, c.database.as_deref(), c.name.as_str(), p.as_str()),
            (DbKind::Sqlite, Some("C:/data/my file.db"), "my file.db", "")
        );
        assert_eq!((c.host.as_str(), c.port, c.user.as_str()), ("", 0, ""));
        assert_eq!(c.subtitle(), "C:/data/my file.db");
        assert!(parse_url("sqlite://").is_none());
        // The saved JSON has no host / password for it, and read_only survives.
        let mut c = c;
        c.read_only = true;
        let s = serde_json::to_string(&c).unwrap();
        assert!(
            s.contains(r#""kind":"sqlite""#)
                && s.contains(r#""read_only":true"#)
                && !s.contains("password"),
            "{s}"
        );
        assert!(
            serde_json::from_str::<SavedConnection>(&s)
                .unwrap()
                .read_only
        );
        c.read_only = false;
        assert!(!serde_json::to_string(&c).unwrap().contains("read_only"));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dsn() {
        let (c, p) = super::parse_dsn("mysql://u:p:w@127.0.0.1:3307/bench").unwrap();
        assert_eq!(
            (c.user.as_str(), p.as_str(), c.port, c.database.as_deref()),
            ("u", "p:w", 3307, Some("bench"))
        );
    }

    #[test]
    fn json_has_no_password() {
        let (c, _) = super::parse_dsn("mysql://u:secret@h:1/d").unwrap();
        let s = serde_json::to_string(&[c]).unwrap();
        assert!(!s.contains("secret") && !s.contains("password"), "{s}");
    }
}

#[cfg(test)]
mod kind_tests {
    use super::*;

    #[test]
    fn old_files_are_mysql_and_oracle_round_trips() {
        let old: Vec<SavedConnection> =
            serde_json::from_str(r#"[{"id":"a","name":"n","host":"h","port":3306,"user":"u"}]"#)
                .unwrap();
        assert_eq!(old[0].kind, DbKind::Mysql);
        let mut c = old[0].clone();
        c.kind = DbKind::Oracle;
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains(r#""kind":"oracle""#));
        assert_eq!(
            serde_json::from_str::<SavedConnection>(&s).unwrap().kind,
            DbKind::Oracle
        );
    }
}
