//! SSH tunnel settings of a saved connection and the `known_hosts` file the
//! tunnel verifies host keys against. Pure logic: no network, no GPUI.
//! The password / key passphrase never lives here on disk (see `creds`).

use russh::keys::{HashAlg, PublicKey};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SshAuth {
    #[default]
    Password,
    /// Private key file (OpenSSH / PEM), optional passphrase.
    Key,
    /// Windows OpenSSH agent or Pageant.
    Agent,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct SshConfig {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub auth: SshAuth,
    /// Private key file path (`SshAuth::Key`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    /// Password or key passphrase typed in the form. Never serialised: it goes
    /// to Credential Manager (`SshConfig::secret_id`).
    #[serde(skip)]
    pub secret: Option<String>,
}

impl std::fmt::Debug for SshConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("auth", &self.auth)
            .field("key_path", &self.key_path)
            .field("secret", &self.secret.as_ref().map(|_| "<hidden>"))
            .finish()
    }
}

fn default_port() -> u16 {
    22
}

impl SshConfig {
    pub fn new() -> Self {
        Self {
            host: String::new(),
            port: 22,
            user: String::new(),
            auth: SshAuth::Password,
            key_path: None,
            secret: None,
        }
    }

    /// Credential Manager id of the SSH password / passphrase of a connection.
    pub fn secret_id(conn_id: &str) -> String {
        format!("{conn_id}#ssh")
    }
}

impl Default for SshConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of checking a server key against the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCheck {
    Trusted,
    Unknown,
    /// The host is known with a different key (fingerprint of the stored one).
    Changed {
        expected: String,
    },
}

/// `SHA256:...` fingerprint, the format `ssh` prints.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// Host name as OpenSSH writes it: bare for port 22, `[host]:port` otherwise.
pub fn host_id(host: &str, port: u16) -> String {
    let h = host.to_ascii_lowercase();
    if port == 22 {
        h
    } else {
        format!("[{h}]:{port}")
    }
}

#[derive(Default, Debug, Clone)]
pub struct KnownHosts {
    entries: Vec<(String, PublicKey)>,
}

impl KnownHosts {
    /// Comments, hashed hosts (`|1|...`), markers and broken lines are skipped.
    pub fn parse(text: &str) -> Self {
        let mut entries = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty()
                || line.starts_with('#')
                || line.starts_with('|')
                || line.starts_with('@')
            {
                continue;
            }
            let mut it = line.split_whitespace();
            let (Some(hosts), Some(algo), Some(b64)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            let Ok(key) = PublicKey::from_openssh(&format!("{algo} {b64}")) else {
                continue;
            };
            for h in hosts.split(',') {
                entries.push((h.to_ascii_lowercase(), key.clone()));
            }
        }
        Self { entries }
    }

    pub fn check(&self, host: &str, port: u16, key: &PublicKey) -> HostCheck {
        let id = host_id(host, port);
        let mut seen = None;
        for (h, k) in &self.entries {
            if *h != id {
                continue;
            }
            if k.key_data() == key.key_data() {
                return HostCheck::Trusted;
            }
            seen.get_or_insert_with(|| fingerprint(k));
        }
        match seen {
            Some(expected) => HostCheck::Changed { expected },
            None => HostCheck::Unknown,
        }
    }

    pub fn add(&mut self, host: &str, port: u16, key: &PublicKey) {
        self.entries.push((host_id(host, port), key.clone()));
    }
}

/// The line appended to the file when a host is trusted.
pub fn known_hosts_line(host: &str, port: u16, key: &PublicKey) -> String {
    let openssh = key.to_openssh().unwrap_or_default();
    let mut parts = openssh.split_whitespace();
    let (algo, b64) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    format!("{} {algo} {b64}\n", host_id(host, port))
}

/// `known_hosts` in the app data folder (`LOCUS_DATA_DIR` relocates it).
pub fn default_known_hosts_path() -> Option<std::path::PathBuf> {
    crate::settings::app_dir().map(|d| d.join("known_hosts"))
}

pub fn load_known_hosts(path: &std::path::Path) -> KnownHosts {
    std::fs::read_to_string(path)
        .map(|t| KnownHosts::parse(&t))
        .unwrap_or_default()
}

/// Appends the key of `host:port` to the `known_hosts` file at `path`.
pub fn trust_host(
    path: &std::path::Path,
    host: &str,
    port: u16,
    key: &PublicKey,
) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(known_hosts_line(host, port, key).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const K1: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIC0cSF3nqAx7H47GGEVDUGzZFUInoIzFQ8+s8Kxq10sO";
    pub const K2: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEvrP7KaVYPcHtW5AhYsXTp2V/mDW2RiUXCdHCOCfrRG";

    fn key(s: &str) -> PublicKey {
        PublicKey::from_openssh(s).unwrap()
    }

    #[test]
    fn config_json_has_no_secret_and_defaults() {
        let mut c = SshConfig::new();
        c.host = "jump".into();
        c.user = "u".into();
        c.secret = Some("hunter2".into());
        let s = serde_json::to_string(&c).unwrap();
        assert!(!s.contains("hunter2") && !s.contains("secret"), "{s}");
        assert!(!format!("{c:?}").contains("hunter2"));
        let back: SshConfig = serde_json::from_str(r#"{"host":"h","user":"u"}"#).unwrap();
        assert_eq!(
            (back.port, back.auth, back.secret),
            (22, SshAuth::Password, None)
        );
        let k: SshConfig =
            serde_json::from_str(r#"{"host":"h","user":"u","auth":"key","key_path":"C:/k"}"#)
                .unwrap();
        assert_eq!(
            (k.auth, k.key_path.as_deref()),
            (SshAuth::Key, Some("C:/k"))
        );
        assert_eq!(SshConfig::secret_id("abc"), "abc#ssh");
    }

    #[test]
    fn host_ids() {
        assert_eq!(host_id("Jump.Example", 22), "jump.example");
        assert_eq!(host_id("jump", 2222), "[jump]:2222");
    }

    #[test]
    fn known_hosts_check() {
        let text =
            format!("# c\n|1|abc|def {K2}\njump {K1}\n[h2]:2200,other {K1} comment\n\nbroken\n");
        let kh = KnownHosts::parse(&text);
        assert_eq!(kh.check("JUMP", 22, &key(K1)), HostCheck::Trusted);
        assert_eq!(kh.check("h2", 2200, &key(K1)), HostCheck::Trusted);
        assert_eq!(
            kh.check("other", 22, &key(K1)),
            HostCheck::Trusted,
            "comma list: bare name is port 22"
        );
        assert_eq!(
            kh.check("h2", 22, &key(K1)),
            HostCheck::Unknown,
            "port is part of the id"
        );
        assert_eq!(kh.check("new", 22, &key(K1)), HostCheck::Unknown);
        match kh.check("jump", 22, &key(K2)) {
            HostCheck::Changed { expected } => assert_eq!(expected, fingerprint(&key(K1))),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn trusted_line_round_trips() {
        let line = known_hosts_line("db.example", 2222, &key(K1));
        assert!(line.starts_with("[db.example]:2222 ssh-ed25519 "), "{line}");
        let mut kh = KnownHosts::parse(&line);
        assert_eq!(kh.check("db.example", 2222, &key(K1)), HostCheck::Trusted);
        kh.add("x", 22, &key(K2));
        assert_eq!(kh.check("x", 22, &key(K2)), HostCheck::Trusted);
        assert!(fingerprint(&key(K1)).starts_with("SHA256:"));
    }
}
