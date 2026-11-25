//! Update check: is there a newer release on GitHub than this build?
//!
//! Only looks and tells; nothing is downloaded or installed here.
//!
//! - Where: `api.github.com/repos/{REPO}/releases`. Without signing in the API allows 60
//!   requests an hour per address (shared by everyone behind one NAT), so on 403 / 429 the
//!   releases feed (`releases.atom`, a page, not the API) gives the same answer.
//! - Channel: a stable build sees stable releases only; an RC build (`0.2.0-rc.1`) sees RCs too.
//!   Versions compare by SemVer, so `0.2.0-rc.1` < `0.2.0`.
//! - When: after start, if the automatic check is on (`Settings::update_check`) and the last one
//!   is more than [`CHECK_EVERY`] ago; or by hand (Settings › About). `update.json` keeps the time
//!   and what was found, so a release found yesterday is still shown after a restart.
//! - How the copy was installed decides what to tell ([`Install`]): Scoop owns its copy, so its
//!   users get `scoop update`; the installer and the portable exe get the release page.
//! - Requests carry only `User-Agent: Locus/<version>`; nothing about the user or the machine.

use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// GitHub repository (owner/name) the releases come from.
pub const REPO: &str = "sao-coding/locus";
/// The running build: the release version (`LOCUS_VERSION`, set by release.yml, so an RC knows
/// it is `0.2.0-rc.1` and not `0.2.0`), else the `Cargo.toml` version (local builds).
pub const CURRENT: &str = match option_env!("LOCUS_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
/// Minimum time between two automatic checks.
pub const CHECK_EVERY: Duration = Duration::from_secs(20 * 60 * 60);
const TIMEOUT: Duration = Duration::from_secs(15);
const STATE_FILE: &str = "update.json";

/// A release newer than this build.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// Version without the `v`, e.g. `0.2.0` or `0.2.0-rc.1`.
    pub version: String,
    /// The release page.
    pub url: String,
}

impl Release {
    fn semver(&self) -> Option<Version> {
        Version::parse(&self.version).ok()
    }
}

/// How this copy got onto the machine, judged by where `locus.exe` is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Install {
    /// A downloaded exe (anywhere else).
    Portable,
    /// Inno Setup (`unins000.exe` next to the exe); winget installs it the same way.
    Installer,
    /// Scoop (`...\scoop\apps\...`): Scoop replaces the exe, Locus must not.
    Scoop,
}

impl Install {
    /// The running copy (decided once).
    pub fn detect() -> Self {
        static FOUND: std::sync::OnceLock<Install> = std::sync::OnceLock::new();
        *FOUND.get_or_init(|| {
            let Ok(exe) = std::env::current_exe() else {
                return Install::Portable;
            };
            let roots: Vec<PathBuf> = ["SCOOP", "SCOOP_GLOBAL"]
                .iter()
                .filter_map(|v| std::env::var_os(v).map(PathBuf::from))
                .collect();
            Self::of(&exe, &roots)
        })
    }

    /// [`detect`](Self::detect) for `exe`; `scoop_roots` are custom Scoop folders
    /// (the `SCOOP` / `SCOOP_GLOBAL` variables).
    pub fn of(exe: &Path, scoop_roots: &[PathBuf]) -> Self {
        let path = exe.to_string_lossy().to_lowercase().replace('/', "\\");
        let in_root = scoop_roots.iter().any(|r| {
            let r = r.to_string_lossy().to_lowercase().replace('/', "\\");
            let r = r.trim_end_matches('\\');
            !r.is_empty() && path.starts_with(&format!("{r}\\apps\\"))
        });
        if in_root || path.contains("\\scoop\\apps\\") {
            Install::Scoop
        } else if exe
            .parent()
            .is_some_and(|d| d.join("unins000.exe").is_file())
        {
            Install::Installer
        } else {
            Install::Portable
        }
    }

    /// The command that updates this copy, when a package manager owns it.
    pub fn command(self) -> Option<&'static str> {
        match self {
            Install::Scoop => Some("scoop update locus"),
            Install::Installer | Install::Portable => None,
        }
    }
}

/// What `update.json` keeps between runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Unix seconds of the last finished check (successful or not).
    #[serde(default)]
    pub last_check: u64,
    /// Newest release the last successful check found (may be older than this build by now).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub found: Option<Release>,
    /// Version whose prompt the user dismissed: no status bar hint for it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed: Option<String>,
}

impl State {
    pub fn load() -> Self {
        crate::datafile::load_named::<State>(STATE_FILE).unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let dir = crate::settings::app_dir()
            .ok_or_else(|| std::io::Error::other("APPDATA is not set"))?;
        std::fs::create_dir_all(&dir)?;
        crate::settings::write_atomic(&dir.join(STATE_FILE), &serde_json::to_vec_pretty(self)?)
    }

    /// Whether an automatic check is due at `now` (Unix seconds). A clock that went back
    /// counts as due.
    pub fn due(&self, now: u64) -> bool {
        now < self.last_check || now - self.last_check >= CHECK_EVERY.as_secs()
    }

    /// The release to offer: the one found, if it is newer than `current`.
    pub fn available(&self, current: &str) -> Option<&Release> {
        let cur = Version::parse(current).ok()?;
        self.found
            .as_ref()
            .filter(|r| r.semver().is_some_and(|v| v > cur))
    }

    /// Whether the status bar should point at `r` (not dismissed).
    pub fn should_hint(&self, r: &Release) -> bool {
        self.dismissed.as_deref() != Some(r.version.as_str())
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// One release as listed by the API or the feed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Listed {
    tag: String,
    url: String,
    /// Marked pre-release on GitHub (the feed doesn't say; the version's `-rc.N` still does).
    prerelease: bool,
}

/// The newest of `listed` that this build's channel sees and that is newer than `current`.
fn pick(listed: &[Listed], current: &str) -> Option<Release> {
    let cur = Version::parse(current).ok()?;
    let rc_channel = !cur.pre.is_empty();
    listed
        .iter()
        .filter_map(|l| {
            let v = Version::parse(l.tag.strip_prefix('v').unwrap_or(&l.tag)).ok()?;
            let pre = l.prerelease || !v.pre.is_empty();
            (v > cur && (rc_channel || !pre)).then_some((v, l))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(v, l)| Release {
            version: v.to_string(),
            url: l.url.clone(),
        })
}

/// `GET /repos/{REPO}/releases`: drafts are left out.
fn parse_api(body: &str) -> Result<Vec<Listed>, String> {
    #[derive(Deserialize)]
    struct Item {
        tag_name: String,
        html_url: String,
        #[serde(default)]
        prerelease: bool,
        #[serde(default)]
        draft: bool,
    }
    let items: Vec<Item> =
        serde_json::from_str(body).map_err(|e| format!("unexpected answer from GitHub: {e}"))?;
    Ok(items
        .into_iter()
        .filter(|i| !i.draft)
        .map(|i| Listed {
            tag: i.tag_name,
            url: i.html_url,
            prerelease: i.prerelease,
        })
        .collect())
}

/// `releases.atom`: every entry links to `https://github.com/{REPO}/releases/tag/<tag>`.
fn parse_feed(body: &str) -> Vec<Listed> {
    let prefix = format!("https://github.com/{REPO}/releases/tag/");
    let mut out: Vec<Listed> = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find(&prefix) {
        let after = &rest[i + prefix.len()..];
        let end = after
            .find(|c: char| c == '"' || c == '<' || c.is_whitespace())
            .unwrap_or(after.len());
        let tag = &after[..end];
        if !tag.is_empty() && !out.iter().any(|l| l.tag == tag) {
            out.push(Listed {
                tag: tag.to_string(),
                url: format!("{prefix}{tag}"),
                prerelease: false,
            });
        }
        rest = &after[end..];
    }
    out
}

fn agent() -> ureq::Agent {
    agent_with(Some(TIMEOUT))
}

/// HTTP client for the update requests: native-tls with the Windows roots, the system proxy,
/// statuses as values. `timeout`: for the whole request (`None` for a download, which instead
/// gets connect and response timeouts).
pub(crate) fn agent_with(timeout: Option<Duration>) -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .timeout_global(timeout)
        .timeout_connect(Some(TIMEOUT))
        .timeout_recv_response(Some(TIMEOUT * 2))
        .user_agent(format!("Locus/{CURRENT}"))
        .tls_config(tls)
        .http_status_as_error(false)
        .build()
        .into()
}

/// GET `url`: status and body.
pub(crate) fn get(agent: &ureq::Agent, url: &str, accept: &str) -> Result<(u16, String), String> {
    let mut r = agent
        .get(url)
        .header("Accept", accept)
        .call()
        .map_err(|e| e.to_string())?;
    let status = r.status().as_u16();
    let body = r
        .body_mut()
        .with_config()
        .limit(4 << 20)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    Ok((status, body))
}

/// Asks GitHub. `Ok(None)`: this build is the newest its channel sees. Blocking (network):
/// call it off the UI thread.
pub fn check() -> Result<Option<Release>, String> {
    let agent = agent();
    let api = format!("https://api.github.com/repos/{REPO}/releases?per_page=30");
    let listed = match get(&agent, &api, "application/vnd.github+json") {
        Ok((200, body)) => parse_api(&body)?,
        // 403 / 429: the hourly limit without signing in is used up; the feed has no limit.
        Ok((403 | 429, _)) => {
            log::info!("update check: GitHub API limit reached, reading the releases feed");
            feed(&agent)?
        }
        Ok((s, _)) => return Err(format!("GitHub answered {s}")),
        Err(e) => {
            log::info!("update check: API request failed ({e}), trying the releases feed");
            feed(&agent).map_err(|_| e)?
        }
    };
    Ok(pick(&listed, CURRENT))
}

fn feed(agent: &ureq::Agent) -> Result<Vec<Listed>, String> {
    let url = format!("https://github.com/{REPO}/releases.atom");
    match get(agent, &url, "application/atom+xml")? {
        (200, body) => Ok(parse_feed(&body)),
        (s, _) => Err(format!("GitHub answered {s}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(tag: &str, pre: bool) -> Listed {
        Listed {
            tag: tag.into(),
            url: format!("https://github.com/{REPO}/releases/tag/{tag}"),
            prerelease: pre,
        }
    }

    #[test]
    fn stable_builds_see_only_stable_releases() {
        let listed = [
            l("v0.1.2", false),
            l("v0.1.3", false),
            l("v0.2.0-rc.1", true),
            l("v0.1.1", false),
        ];
        assert_eq!(pick(&listed, "0.1.2").unwrap().version, "0.1.3");
        assert_eq!(pick(&listed, "0.1.3"), None);
        // A tag GitHub marks pre-release is skipped even without -rc.
        assert_eq!(pick(&[l("v0.1.4", true)], "0.1.3"), None);
    }

    #[test]
    fn rc_builds_see_rcs_and_stable() {
        let listed = [l("v0.2.0-rc.1", true), l("v0.2.0-rc.2", true)];
        assert_eq!(pick(&listed, "0.2.0-rc.1").unwrap().version, "0.2.0-rc.2");
        let listed = [l("v0.2.0-rc.2", true), l("v0.2.0", false)];
        assert_eq!(pick(&listed, "0.2.0-rc.1").unwrap().version, "0.2.0");
    }

    #[test]
    fn odd_tags_are_ignored() {
        let listed = [l("nightly", false), l("v0.9", false), l("0.1.5", false)];
        let r = pick(&listed, "0.1.2").unwrap();
        assert_eq!(r.version, "0.1.5");
        assert!(r.url.ends_with("/releases/tag/0.1.5"));
    }

    #[test]
    fn api_answer_skips_drafts() {
        let body = r#"[
            {"tag_name":"v0.1.4","html_url":"https://github.com/sao-coding/locus/releases/tag/v0.1.4","prerelease":false,"draft":true},
            {"tag_name":"v0.1.3","html_url":"https://github.com/sao-coding/locus/releases/tag/v0.1.3","prerelease":false,"draft":false,"assets":[]}
        ]"#;
        let listed = parse_api(body).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(pick(&listed, "0.1.2").unwrap().version, "0.1.3");
        assert!(parse_api("{\"message\":\"API rate limit exceeded\"}").is_err());
    }

    #[test]
    fn feed_lists_each_tag_once() {
        let body = format!(
            r#"<feed><entry><id>tag:github.com,2008:Repository/1/v0.1.3</id>
            <link rel="alternate" type="text/html" href="https://github.com/{REPO}/releases/tag/v0.1.3"/>
            <content>see https://github.com/{REPO}/releases/tag/v0.1.3 for details</content></entry>
            <entry><link rel="alternate" href="https://github.com/{REPO}/releases/tag/v0.2.0-rc.1"/></entry></feed>"#
        );
        let listed = parse_feed(&body);
        assert_eq!(
            listed.iter().map(|l| l.tag.as_str()).collect::<Vec<_>>(),
            ["v0.1.3", "v0.2.0-rc.1"]
        );
        // The feed has no pre-release flag: the -rc version keeps stable builds away from it.
        assert_eq!(pick(&listed, "0.1.2").unwrap().version, "0.1.3");
        assert_eq!(pick(&listed, "0.2.0-rc.0").unwrap().version, "0.2.0-rc.1");
    }

    /// `LOCUS_VERSION` (release builds) may only add a pre-release suffix to Cargo.toml's.
    #[test]
    fn current_is_the_cargo_version_with_an_optional_suffix() {
        let v = Version::parse(CURRENT).unwrap();
        let cargo = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        assert_eq!(
            (v.major, v.minor, v.patch),
            (cargo.major, cargo.minor, cargo.patch)
        );
    }

    #[test]
    fn state_due_available_and_dismissed() {
        let mut s = State::default();
        assert!(s.due(1_000_000));
        s.last_check = 1_000_000;
        assert!(!s.due(1_000_000 + 3600));
        assert!(s.due(1_000_000 + CHECK_EVERY.as_secs()));
        assert!(s.due(999_000), "clock went back");

        s.found = Some(Release {
            version: "0.1.3".into(),
            url: "u".into(),
        });
        assert!(s.available("0.1.2").is_some());
        assert!(s.available("0.1.3").is_none(), "already updated");
        let r = s.available("0.1.2").cloned().unwrap();
        assert!(s.should_hint(&r));
        s.dismissed = Some("0.1.3".into());
        assert!(!s.should_hint(&r));

        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<State>(&json).unwrap(), s);
        assert_eq!(
            serde_json::from_str::<State>("{}").unwrap(),
            State::default()
        );
    }

    #[test]
    fn install_kind_from_the_exe_path() {
        let of = |p: &str, roots: &[PathBuf]| Install::of(Path::new(p), roots);
        assert_eq!(
            of(r"C:\Users\me\scoop\apps\locus\current\locus.exe", &[]),
            Install::Scoop
        );
        assert_eq!(
            of(
                r"D:\tools\apps\locus\0.1.2\locus.exe",
                &[PathBuf::from(r"D:\Tools\")]
            ),
            Install::Scoop
        );
        assert_eq!(of(r"D:\Downloads\locus.exe", &[]), Install::Portable);
        assert_eq!(Install::Scoop.command(), Some("scoop update locus"));
        assert_eq!(Install::Portable.command(), None);

        let d = std::env::temp_dir().join(format!("locus-install-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("unins000.exe"), b"").unwrap();
        assert_eq!(Install::of(&d.join("locus.exe"), &[]), Install::Installer);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Real request to GitHub: `cargo test -p locus-core -- --ignored update_check_live`.
    #[test]
    #[ignore = "network"]
    fn update_check_live() {
        let r = check();
        println!("{r:?}");
        assert!(r.is_ok(), "{r:?}");
    }

    /// The fallback when the API limit is used up: `-- --ignored update_feed_live`.
    #[test]
    #[ignore = "network"]
    fn update_feed_live() {
        let listed = feed(&agent()).unwrap();
        println!("{listed:?}");
        assert!(listed.iter().any(|l| l.tag == "v0.1.0"), "{listed:?}");
        assert!(pick(&listed, "0.0.1").is_some());
    }
}
