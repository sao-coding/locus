//! Downloading and installing a release found by [`crate::update`].
//!
//! - What: the file this copy is made of ([`asset_name`]): the setup for an installed copy,
//!   the exe for a portable one. A Scoop copy is Scoop's to update, so nothing here.
//! - Trust: every release file has a minisign signature (`<file>.minisig`, made by the release
//!   workflow, `scripts/ci/sign.ps1`). The file is kept only when the signature is from
//!   [`PUBLIC_KEY`] and its trusted comment (which the key signs too) is exactly
//!   `file:<name> version:<version>`, so another signed file or an older release can't be
//!   passed off as this one. Releases made before signing have no `.minisig`: they can only be
//!   downloaded by hand.
//! - Where: `%LOCALAPPDATA%\Locus\updates` (`LOCUS_DATA_DIR\updates` in UI tests). A download
//!   goes to `<file>.part` and is renamed only after the check.
//! - Install ([`apply`], called while Locus quits): the setup runs silently and starts Locus
//!   again (`/RELAUNCH=1`, see `installer/locus.iss`); a portable exe is swapped in place
//!   (`locus.exe` -> `locus.exe.old`, put back if the swap fails) and started with
//!   `--after-update <pid>`, which waits for this process to end. `.old` goes at the next start
//!   ([`cleanup`]).

use crate::update::{Install, REPO};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The release signing key (`minisign.pub` in the repository).
pub const PUBLIC_KEY: &str = "RWRDNREpYa8EZ59rUGsAnTVNIMjNfUWhciyq6SgS2MpNZvMGj0yVAZ0s";
/// Larger downloads are refused (a release exe is tens of MB).
const MAX_SIZE: u64 = 512 << 20;

/// Release file that updates a copy installed as `install`.
pub fn asset_name(version: &str, install: Install) -> Option<String> {
    match install {
        Install::Portable => Some(format!("locus-{version}-windows-x64.exe")),
        Install::Installer => Some(format!("locus-{version}-windows-x64-setup.exe")),
        Install::Scoop => None,
    }
}

/// Folder for downloaded updates.
pub fn updates_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("LOCUS_DATA_DIR") {
        return Some(PathBuf::from(d).join("updates"));
    }
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Locus").join("updates"))
}

/// Checks `data` against the minisign signature `sig` (the `.minisig` text): made by
/// [`PUBLIC_KEY`], trusted comment `file:<name> version:<version>`.
pub fn verify(data: &[u8], sig: &str, name: &str, version: &str) -> Result<(), String> {
    verify_with(PUBLIC_KEY, data, sig, name, version)
}

fn verify_with(key: &str, data: &[u8], sig: &str, name: &str, version: &str) -> Result<(), String> {
    let key = minisign_verify::PublicKey::from_base64(key).map_err(|e| format!("bad key: {e}"))?;
    let sig = minisign_verify::Signature::decode(sig)
        .map_err(|e| format!("unreadable signature: {e}"))?;
    key.verify(data, &sig, false)
        .map_err(|e| format!("signature check failed: {e}"))?;
    let want = format!("file:{name} version:{version}");
    if sig.trusted_comment() != want {
        return Err(format!(
            "the signature is for \"{}\", not \"{want}\"",
            sig.trusted_comment()
        ));
    }
    Ok(())
}

/// Download progress, read by the UI while [`download`] runs on another thread.
#[derive(Default, Debug)]
pub struct Progress {
    pub done: AtomicU64,
    /// 0 while unknown.
    pub total: AtomicU64,
}

impl Progress {
    /// Percent done, when the size is known.
    pub fn percent(&self) -> Option<u64> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| self.done.load(Ordering::Relaxed).min(total) * 100 / total)
    }
}

fn release_url(version: &str, file: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/v{version}/{file}")
}

/// Why a download did not give a file to install. The UI words each kind itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DlError {
    /// The release has no `.minisig` (made before releases were signed): by hand only.
    Unsigned,
    /// The file does not match its signature, or the signature is not from the release key.
    BadSignature(String),
    /// Network or disk; worth trying again.
    Failed(String),
}

impl std::fmt::Display for DlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DlError::Unsigned => f.write_str("the release has no signature"),
            DlError::BadSignature(e) | DlError::Failed(e) => f.write_str(e),
        }
    }
}

/// Downloads and checks the file that updates `install` to `version`; returns its path. A file
/// already downloaded and still valid is reused. Blocking (network): call it off the UI thread.
pub fn download(version: &str, install: Install, progress: &Progress) -> Result<PathBuf, DlError> {
    let failed = |e: String| DlError::Failed(e);
    let name = asset_name(version, install)
        .ok_or_else(|| failed("this copy is updated by its package manager".into()))?;
    let dir = updates_dir().ok_or_else(|| failed("LOCALAPPDATA is not set".into()))?;
    std::fs::create_dir_all(&dir).map_err(|e| failed(format!("{}: {e}", dir.display())))?;
    let target = dir.join(&name);
    let sig_path = dir.join(format!("{name}.minisig"));
    if let (Ok(data), Ok(sig)) = (std::fs::read(&target), std::fs::read_to_string(&sig_path))
        && verify(&data, &sig, &name, version).is_ok()
    {
        let n = data.len() as u64;
        progress.total.store(n, Ordering::Relaxed);
        progress.done.store(n, Ordering::Relaxed);
        return Ok(target);
    }

    let agent = crate::update::agent_with(None);
    // The signature first: a release without one is not downloaded at all.
    let sig = match crate::update::get(
        &agent,
        &release_url(version, &format!("{name}.minisig")),
        "*/*",
    )
    .map_err(failed)?
    {
        (200, body) => body,
        (404, _) => return Err(DlError::Unsigned),
        (s, _) => return Err(failed(format!("GitHub answered {s} for the signature"))),
    };

    let part = dir.join(format!("{name}.part"));
    let result = fetch(&agent, &release_url(version, &name), &part, progress)
        .map_err(failed)
        .and_then(|()| {
            let data = std::fs::read(&part).map_err(|e| failed(e.to_string()))?;
            verify(&data, &sig, &name, version).map_err(DlError::BadSignature)
        });
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::write(&sig_path, &sig).map_err(|e| failed(e.to_string()))?;
    let _ = std::fs::remove_file(&target);
    std::fs::rename(&part, &target).map_err(|e| failed(e.to_string()))?;
    Ok(target)
}

fn fetch(agent: &ureq::Agent, url: &str, to: &Path, progress: &Progress) -> Result<(), String> {
    let mut r = agent.get(url).call().map_err(|e| e.to_string())?;
    let status = r.status().as_u16();
    if status != 200 {
        return Err(format!("GitHub answered {status} for the download"));
    }
    let total = r
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if total > MAX_SIZE {
        return Err(format!("the download is too large ({total} bytes)"));
    }
    progress.total.store(total, Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    let mut body = r.body_mut().with_config().limit(MAX_SIZE).reader();
    let mut file = std::fs::File::create(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = body.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        progress.done.fetch_add(n as u64, Ordering::Relaxed);
    }
    file.sync_all().map_err(|e| e.to_string())
}

/// At start: removes `locus.exe.old` left by a swap, and downloads that are not for `keep`
/// (the newer version still waiting, if any).
pub fn cleanup(keep: Option<&str>) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
    }
    let Some(dir) = updates_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let prefix = keep.map(|v| format!("locus-{v}-"));
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let kept = prefix
            .as_deref()
            .is_some_and(|p| name.starts_with(p) && !name.ends_with(".part"));
        if !kept {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("exe.old")
}

/// Installs the checked download `file` for a copy installed as `install`. Called while Locus
/// quits; `relaunch` starts the new version afterwards.
pub fn apply(file: &Path, install: Install, relaunch: bool) -> Result<(), String> {
    match install {
        Install::Installer => {
            let mut cmd = std::process::Command::new(file);
            cmd.args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]);
            if relaunch {
                cmd.arg("/RELAUNCH=1");
            }
            cmd.spawn()
                .map(drop)
                .map_err(|e| format!("cannot start the installer: {e}"))
        }
        Install::Portable => {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            swap(&exe, file)?;
            if relaunch {
                std::process::Command::new(&exe)
                    .args(["--after-update", &std::process::id().to_string()])
                    .spawn()
                    .map_err(|e| format!("updated, but cannot start it: {e}"))?;
            }
            Ok(())
        }
        Install::Scoop => Err("this copy is updated by Scoop".into()),
    }
}

/// Whether a portable copy can be swapped: its folder takes a new file. Checked before Locus
/// quits for the update, so a read-only folder is reported instead of failing at quit.
pub fn can_swap() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let probe = exe.with_extension("exe.write-test");
    std::fs::write(&probe, b"").map_err(|e| {
        format!(
            "{}: {e}",
            exe.parent()
                .map(|d| d.display().to_string())
                .unwrap_or_default()
        )
    })?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Puts `new` in place of `exe` (which may be running: Windows lets a running exe be renamed).
/// The old file stays as `<exe>.old`; on a failure the old exe is put back.
fn swap(exe: &Path, new: &Path) -> Result<(), String> {
    let staged = exe.with_extension("exe.new");
    std::fs::copy(new, &staged)
        .map_err(|e| format!("cannot write next to {}: {e}", exe.display()))?;
    let old = old_path(exe);
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(exe, &old) {
        let _ = std::fs::remove_file(&staged);
        return Err(format!("cannot move {} aside: {e}", exe.display()));
    }
    if let Err(e) = std::fs::rename(&staged, exe) {
        let _ = std::fs::rename(&old, exe);
        let _ = std::fs::remove_file(&staged);
        return Err(format!("cannot put the new version in place: {e}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Throwaway key and a signature of DATA made with `minisign -S -t
    /// "file:locus-9.9.9-windows-x64.exe version:9.9.9"`.
    const TEST_KEY: &str = "RWRgF3i/UBrPSTPpMxIcOBCB+2vxc8p4TkOntNkvww24F8vGzRbBUyw9";
    const DATA: &[u8] = b"locus update test payload";
    const SIG: &str = "untrusted comment: signature from minisign secret key
RURgF3i/UBrPSbehqsX3XKwfOkdSlSNFmd89wUqYQlNxuDbfmn3Xwv+nT3YFRMWVYrUauCcMz3xe7oMx5HkN5sBhSwSgpM6cIgM=
trusted comment: file:locus-9.9.9-windows-x64.exe version:9.9.9
V10Pnj5QHOIWziHSusDZjSnoODyKZ+w0/Mtwbul60OyJ+hOGAn3WO75QF/3t6x7LLFf9xH4/AsGqcdimVVedAg==
";
    const NAME: &str = "locus-9.9.9-windows-x64.exe";

    #[test]
    fn signature_must_match_key_file_and_version() {
        assert_eq!(verify_with(TEST_KEY, DATA, SIG, NAME, "9.9.9"), Ok(()));
        // Changed bytes.
        assert!(verify_with(TEST_KEY, b"locus update test payloaD", SIG, NAME, "9.9.9").is_err());
        // Another file or an older version under the same signature.
        let e = verify_with(
            TEST_KEY,
            DATA,
            SIG,
            "locus-9.9.9-windows-x64-setup.exe",
            "9.9.9",
        );
        assert!(
            e.unwrap_err()
                .contains("not \"file:locus-9.9.9-windows-x64-setup.exe")
        );
        assert!(verify_with(TEST_KEY, DATA, SIG, NAME, "9.9.8").is_err());
        // The release key did not make this signature.
        assert!(
            verify(DATA, SIG, NAME, "9.9.9")
                .unwrap_err()
                .contains("signature check failed")
        );
        // A trusted comment edited by hand breaks the signature.
        let forged = SIG.replace("version:9.9.9", "version:9.9.10");
        assert!(verify_with(TEST_KEY, DATA, &forged, NAME, "9.9.10").is_err());
        assert!(verify_with(TEST_KEY, DATA, "garbage", NAME, "9.9.9").is_err());
    }

    #[test]
    fn release_key_parses() {
        assert!(minisign_verify::PublicKey::from_base64(PUBLIC_KEY).is_ok());
    }

    #[test]
    fn asset_per_install_kind() {
        assert_eq!(
            asset_name("0.2.0", Install::Portable).as_deref(),
            Some("locus-0.2.0-windows-x64.exe")
        );
        assert_eq!(
            asset_name("0.2.0-rc.1", Install::Installer).as_deref(),
            Some("locus-0.2.0-rc.1-windows-x64-setup.exe")
        );
        assert_eq!(asset_name("0.2.0", Install::Scoop), None);
        assert_eq!(
            release_url("0.2.0", "locus-0.2.0-windows-x64.exe"),
            "https://github.com/sao-coding/locus/releases/download/v0.2.0/locus-0.2.0-windows-x64.exe"
        );
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("locus-upd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn swap_keeps_the_old_exe_aside() {
        let d = temp("swap");
        let exe = d.join("locus.exe");
        let new = d.join("download.exe");
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        swap(&exe, &new).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(old_path(&exe)).unwrap(), b"old");
        assert!(!exe.with_extension("exe.new").exists());
        // A second swap replaces the earlier .old.
        std::fs::write(&new, b"newer").unwrap();
        swap(&exe, &new).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"newer");
        assert_eq!(std::fs::read(old_path(&exe)).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn swap_fails_cleanly_when_the_download_is_missing() {
        let d = temp("swap-fail");
        let exe = d.join("locus.exe");
        std::fs::write(&exe, b"old").unwrap();
        assert!(swap(&exe, &d.join("missing.exe")).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"old");
        assert!(!old_path(&exe).exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn progress_percent() {
        let p = Progress::default();
        assert_eq!(p.percent(), None);
        p.total.store(200, Ordering::Relaxed);
        p.done.store(50, Ordering::Relaxed);
        assert_eq!(p.percent(), Some(25));
        p.done.store(300, Ordering::Relaxed);
        assert_eq!(p.percent(), Some(100));
    }

    /// A release made before signing: `-- --ignored update_download_unsigned_live`.
    #[test]
    #[ignore = "network"]
    fn update_download_unsigned_live() {
        let e = download("0.1.2", Install::Portable, &Progress::default()).unwrap_err();
        assert_eq!(e, DlError::Unsigned);
    }
}
