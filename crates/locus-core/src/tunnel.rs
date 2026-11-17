//! SSH tunnel (`russh`, pure Rust): a local listener on `127.0.0.1:<random>`
//! forwards every accepted connection through a `direct-tcpip` channel to the
//! database host as seen from the jump host. The drivers (including the Go
//! sidecar) just connect to that local port.
//!
//! * One tunnel per distinct settings (so one per saved connection); every
//!   driver / session / KILL connection of it shares the one SSH session.
//! * The SSH session is opened by `acquire` (so Test connection reports SSH
//!   errors first), dropped 30 s after the last forwarded connection closes,
//!   and opened again on demand: that is also the reconnect after a drop.
//!   The local listener lives until `close` (connection edited / deleted).
//! * Host keys are checked against `known_hosts` (`sshcfg`): unknown keys and
//!   changed keys are errors; `trust_pending` records an unknown one after the
//!   user confirmed its fingerprint.

use crate::sshcfg::{HostCheck, KnownHosts, SshAuth, SshConfig, fingerprint, load_known_hosts};
use db_core::{DbError, ErrorKind};
use parking_lot::Mutex;
use russh::client::{self, Handle};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::{PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate, load_secret_key};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

/// `DbError::code` of tunnel failures, so the connection form can offer "Trust".
pub const CODE_SSH: i64 = -9000;
pub const CODE_HOST_UNKNOWN: i64 = -9001;
pub const CODE_HOST_CHANGED: i64 = -9002;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const IDLE_CLOSE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub enum TunnelError {
    HostUnknown {
        host: String,
        port: u16,
        fingerprint: String,
    },
    HostChanged {
        host: String,
        port: u16,
        expected: String,
        got: String,
        file: String,
    },
    Connect(String),
    Auth(String),
}

impl TunnelError {
    pub fn into_db(self) -> DbError {
        match self {
            TunnelError::HostUnknown { host, port, fingerprint } => DbError::new(
                ErrorKind::Connect,
                format!(
                    "SSH tunnel: the host key of {host}:{port} is not trusted yet ({fingerprint}). Use Test connection in the connection form to check the fingerprint and trust it."
                ),
            )
            .with_code(CODE_HOST_UNKNOWN),
            TunnelError::HostChanged { host, port, expected, got, file } => DbError::new(
                ErrorKind::Connect,
                format!(
                    "SSH tunnel: HOST KEY CHANGED for {host}:{port}. Trusted {expected}, the server now presents {got}. Someone may be intercepting the connection, or the server was reinstalled. If you expected this, delete the line of this host from {file} and try again."
                ),
            )
            .with_code(CODE_HOST_CHANGED),
            TunnelError::Connect(m) => DbError::new(ErrorKind::Connect, format!("SSH tunnel: could not connect: {m}")).with_code(CODE_SSH),
            TunnelError::Auth(m) => DbError::new(ErrorKind::Auth, format!("SSH tunnel: login failed: {m}")).with_code(CODE_SSH),
        }
    }
}

impl std::fmt::Display for TunnelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.clone().into_db().message)
    }
}

/// Everything that identifies a tunnel. `ssh.secret` must be resolved.
#[derive(Clone, PartialEq)]
pub struct TunnelSpec {
    pub conn_id: String,
    pub ssh: SshConfig,
    pub target_host: String,
    pub target_port: u16,
    pub known_hosts: PathBuf,
}

impl TunnelSpec {
    fn key(&self) -> String {
        let s = &self.ssh;
        format!(
            "{}|{}:{}|{}|{:?}|{:?}|{:?}|{}:{}",
            self.conn_id,
            s.host,
            s.port,
            s.user,
            s.auth,
            s.key_path,
            s.secret,
            self.target_host,
            self.target_port
        )
    }
}

/// Unknown host keys seen by failed connects, waiting for the user's answer.
static PENDING: Mutex<Vec<(String, PublicKey)>> = Mutex::new(Vec::new());

/// Records the key last refused for `host:port` in `known_hosts` (the user
/// confirmed its fingerprint). Returns the fingerprint trusted.
pub fn trust_pending(
    known_hosts: &std::path::Path,
    host: &str,
    port: u16,
) -> Result<String, String> {
    let id = crate::sshcfg::host_id(host, port);
    let key = PENDING
        .lock()
        .iter()
        .rev()
        .find(|(h, _)| *h == id)
        .map(|(_, k)| k.clone());
    let key = key.ok_or_else(|| "no pending host key; test the connection again".to_string())?;
    crate::sshcfg::trust_host(known_hosts, host, port, &key).map_err(|e| e.to_string())?;
    PENDING.lock().retain(|(h, _)| *h != id);
    Ok(fingerprint(&key))
}

struct HostKeys {
    host: String,
    port: u16,
    file: PathBuf,
    issue: Arc<Mutex<Option<TunnelError>>>,
}

impl client::Handler for HostKeys {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = key.public_key();
        let known: KnownHosts = load_known_hosts(&self.file);
        let issue = match known.check(&self.host, self.port, &key) {
            HostCheck::Trusted => return Ok(true),
            HostCheck::Unknown => {
                let id = crate::sshcfg::host_id(&self.host, self.port);
                let mut p = PENDING.lock();
                p.retain(|(h, _)| *h != id);
                p.push((id, key.clone()));
                TunnelError::HostUnknown {
                    host: self.host.clone(),
                    port: self.port,
                    fingerprint: fingerprint(&key),
                }
            }
            HostCheck::Changed { expected } => TunnelError::HostChanged {
                host: self.host.clone(),
                port: self.port,
                expected,
                got: fingerprint(&key),
                file: self.file.display().to_string(),
            },
        };
        *self.issue.lock() = Some(issue);
        Ok(false)
    }
}

async fn open_session(spec: &TunnelSpec) -> Result<Handle<HostKeys>, TunnelError> {
    let ssh = &spec.ssh;
    let issue = Arc::new(Mutex::new(None));
    let handler = HostKeys {
        host: ssh.host.clone(),
        port: ssh.port,
        file: spec.known_hosts.clone(),
        issue: issue.clone(),
    };
    let config = Arc::new(client::Config {
        nodelay: true,
        keepalive_interval: Some(Duration::from_secs(20)),
        keepalive_max: 3,
        ..Default::default()
    });
    let connecting = client::connect(config, (ssh.host.as_str(), ssh.port), handler);
    let mut h = match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
        Err(_) => {
            return Err(TunnelError::Connect(format!(
                "{}:{} did not answer within {} s",
                ssh.host,
                ssh.port,
                CONNECT_TIMEOUT.as_secs()
            )));
        }
        Ok(Err(e)) => {
            let found = issue.lock().take();
            return Err(found.unwrap_or_else(|| {
                TunnelError::Connect(format!("{}:{}: {e}", ssh.host, ssh.port))
            }));
        }
        Ok(Ok(h)) => h,
    };
    authenticate(&mut h, ssh).await?;
    Ok(h)
}

fn auth_failed(what: &str) -> TunnelError {
    TunnelError::Auth(format!("{what} was rejected by the SSH server"))
}

async fn authenticate(h: &mut Handle<HostKeys>, ssh: &SshConfig) -> Result<(), TunnelError> {
    let user = ssh.user.clone();
    let proto = |e: russh::Error| TunnelError::Auth(e.to_string());
    match ssh.auth {
        SshAuth::Password => {
            let pw = ssh.secret.clone().unwrap_or_default();
            if h.authenticate_password(user, pw)
                .await
                .map_err(proto)?
                .success()
            {
                Ok(())
            } else {
                Err(auth_failed("the password"))
            }
        }
        SshAuth::Key => {
            let path = ssh.key_path.clone().unwrap_or_default();
            if path.is_empty() {
                return Err(TunnelError::Auth("no private key file is set".into()));
            }
            let pass = ssh.secret.as_deref().filter(|s| !s.is_empty());
            let key = load_secret_key(&path, pass).map_err(|e| {
                let hint = if pass.is_none() {
                    " (if the key has a passphrase, enter it)"
                } else {
                    ""
                };
                TunnelError::Auth(format!("could not read the private key {path}: {e}{hint}"))
            })?;
            let hash = h.best_supported_rsa_hash().await.map_err(proto)?.flatten();
            if h.authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(proto)?
                .success()
            {
                Ok(())
            } else {
                Err(auth_failed("the private key"))
            }
        }
        SshAuth::Agent => {
            if let Ok(mut a) = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await
            {
                return agent_auth(h, &user, &mut a).await;
            }
            match AgentClient::connect_pageant().await {
                Ok(mut a) => agent_auth(h, &user, &mut a).await,
                Err(_) => Err(TunnelError::Auth(
                    "no SSH agent found (Windows OpenSSH agent or Pageant is not running)".into(),
                )),
            }
        }
    }
}

async fn agent_auth<S: AgentStream + Unpin + Send + 'static>(
    h: &mut Handle<HostKeys>,
    user: &str,
    agent: &mut AgentClient<S>,
) -> Result<(), TunnelError> {
    let ids = agent
        .request_identities()
        .await
        .map_err(|e| TunnelError::Auth(format!("the SSH agent did not answer: {e}")))?;
    let keys: Vec<PublicKey> = ids
        .into_iter()
        .filter_map(|i| match i {
            AgentIdentity::PublicKey { key, .. } => Some(key),
            _ => None,
        })
        .collect();
    if keys.is_empty() {
        return Err(TunnelError::Auth("the SSH agent holds no keys".into()));
    }
    let hash = h
        .best_supported_rsa_hash()
        .await
        .map_err(|e| TunnelError::Auth(e.to_string()))?
        .flatten();
    for key in keys {
        if let Ok(r) = h.authenticate_publickey_with(user, key, hash, agent).await
            && r.success()
        {
            return Ok(());
        }
    }
    Err(auth_failed("every key of the SSH agent"))
}

struct Inner {
    spec: TunnelSpec,
    session: tokio::sync::Mutex<Option<Handle<HostKeys>>>,
    active: AtomicUsize,
    epoch: AtomicU64,
    /// How often the SSH session was (re)opened; tests and diagnostics.
    opened: AtomicUsize,
}

impl Inner {
    /// A live session, opening one when there is none or it was closed.
    async fn open_channel(
        &self,
        peer: std::net::SocketAddr,
    ) -> Result<russh::Channel<client::Msg>, TunnelError> {
        let mut g = self.session.lock().await;
        for attempt in 0..2 {
            if g.as_ref().is_none_or(|h| h.is_closed()) {
                *g = Some(open_session(&self.spec).await?);
                self.opened.fetch_add(1, Ordering::SeqCst);
            }
            let h = g.as_ref().unwrap();
            match h
                .channel_open_direct_tcpip(
                    self.spec.target_host.clone(),
                    self.spec.target_port as u32,
                    peer.ip().to_string(),
                    peer.port() as u32,
                )
                .await
            {
                Ok(c) => return Ok(c),
                Err(e) if attempt == 1 => {
                    return Err(TunnelError::Connect(format!(
                        "forwarding to {}:{} failed: {e}",
                        self.spec.target_host, self.spec.target_port
                    )));
                }
                // The session is dead or half dead: drop it and reconnect once.
                Err(_) => *g = None,
            }
        }
        unreachable!()
    }

    async fn ensure_session(&self) -> Result<(), TunnelError> {
        let mut g = self.session.lock().await;
        if g.as_ref().is_none_or(|h| h.is_closed()) {
            *g = Some(open_session(&self.spec).await?);
            self.opened.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

async fn serve(inner: Arc<Inner>, mut sock: TcpStream, peer: std::net::SocketAddr) {
    inner.active.fetch_add(1, Ordering::SeqCst);
    inner.epoch.fetch_add(1, Ordering::SeqCst);
    if let Ok(ch) = inner.open_channel(peer).await {
        let mut stream = ch.into_stream();
        let _ = tokio::io::copy_bidirectional(&mut sock, &mut stream).await;
    }
    // Dropping `sock` here resets the driver's connection when forwarding failed.
    if inner.active.fetch_sub(1, Ordering::SeqCst) == 1 {
        let epoch = inner.epoch.load(Ordering::SeqCst);
        let inner = inner.clone();
        tokio::spawn(async move {
            tokio::time::sleep(IDLE_CLOSE).await;
            if inner.active.load(Ordering::SeqCst) == 0
                && inner.epoch.load(Ordering::SeqCst) == epoch
            {
                *inner.session.lock().await = None;
            }
        });
    }
}

pub struct Tunnel {
    port: u16,
    inner: Arc<Inner>,
    task: JoinHandle<()>,
}

impl Tunnel {
    /// The port on `127.0.0.1` the driver connects to.
    pub fn local_port(&self) -> u16 {
        self.port
    }

    /// Times the SSH session was opened (1 + reconnects).
    pub fn sessions_opened(&self) -> usize {
        self.inner.opened.load(Ordering::SeqCst)
    }

    /// Drops the SSH session now (as a network drop would); the next
    /// forwarded connection opens a new one.
    pub async fn drop_session(&self) {
        *self.inner.session.lock().await = None;
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

static REGISTRY: tokio::sync::Mutex<Vec<(String, Arc<Tunnel>)>> =
    tokio::sync::Mutex::const_new(Vec::new());

/// The tunnel for `spec`: the running one, or a new one whose SSH session is
/// opened and logged in before this returns. Runs on `rt`, so it can be
/// awaited from any executor.
pub async fn acquire(
    rt: &tokio::runtime::Handle,
    spec: TunnelSpec,
) -> Result<Arc<Tunnel>, DbError> {
    match rt.spawn(acquire_inner(spec)).await {
        Ok(r) => r.map_err(TunnelError::into_db),
        Err(e) => Err(DbError::new(
            ErrorKind::Internal,
            format!("SSH tunnel task failed: {e}"),
        )),
    }
}

async fn acquire_inner(spec: TunnelSpec) -> Result<Arc<Tunnel>, TunnelError> {
    let key = spec.key();
    let mut reg = REGISTRY.lock().await;
    if let Some((_, t)) = reg.iter().find(|(k, _)| *k == key) {
        t.inner.ensure_session().await?;
        return Ok(t.clone());
    }
    // Logs in first: an error here is the "SSH step" of Test connection.
    let first = open_session(&spec).await?;
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| TunnelError::Connect(format!("local listener: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| TunnelError::Connect(e.to_string()))?
        .port();
    let inner = Arc::new(Inner {
        spec,
        session: tokio::sync::Mutex::new(Some(first)),
        active: AtomicUsize::new(0),
        epoch: AtomicU64::new(0),
        opened: AtomicUsize::new(1),
    });
    let task = tokio::spawn({
        let inner = inner.clone();
        async move {
            let mut conns = JoinSet::new();
            loop {
                tokio::select! {
                    r = listener.accept() => match r {
                        Ok((sock, peer)) => { conns.spawn(serve(inner.clone(), sock, peer)); }
                        Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                    },
                    // Reap finished forwards so the set does not grow.
                    Some(_) = conns.join_next(), if !conns.is_empty() => {}
                }
            }
        }
    });
    let t = Arc::new(Tunnel { port, inner, task });
    reg.push((key, t.clone()));
    Ok(t)
}

/// Closes every tunnel of a saved connection (edited or deleted): the local
/// ports stop listening and the SSH sessions end.
pub fn close(rt: &tokio::runtime::Handle, conn_id: &str) {
    let id = conn_id.to_string();
    rt.spawn(async move {
        REGISTRY
            .lock()
            .await
            .retain(|(_, t)| t.inner.spec.conn_id != id)
    });
}
