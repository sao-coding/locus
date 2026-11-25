//! Updates in the app (logic: `locus_core::update`, `locus_core::update_install`). One `Updates`
//! entity, a global: the status bar shows a hint when a newer release is known, Settings › About
//! shows the state and the next action.
//!
//! - Check: a little after start, so it never competes with the first frames, and only for the
//!   real data folder (not the demo / UI-test scenes); or "Check now".
//! - Download: with "Download updates automatically" on (default), a release found is
//!   downloaded and its signature checked in the background; or "Download and update".
//! - Install: only when the user picks "Restart and update". Locus closes the normal way
//!   (`should_close`: open transactions, running queries), and while it quits
//!   (`on_app_quit`) the update is put in place and the new version started.
//! - A Scoop copy belongs to Scoop: it only gets the command.
use gpui_kit::*;
use locus_core::update::{self, Install, Release, State};
use locus_core::update_install::{self as inst, DlError, Progress};
use rust_i18n::t;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Wait after start before the automatic check.
const START_DELAY: Duration = Duration::from_secs(10);
/// Repaint interval while a download runs (progress).
const TICK: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// Nothing asked this run (the state may still hold what an earlier run found).
    Idle,
    Checking,
    /// The last check this run found nothing newer.
    UpToDate,
    Failed(String),
}

/// The download of the release in `version`.
#[derive(Clone, Debug)]
pub(crate) enum Dl {
    Idle,
    Downloading {
        version: String,
        progress: Arc<Progress>,
    },
    /// Downloaded and signature checked: ready to install.
    Ready {
        version: String,
        path: PathBuf,
    },
    Failed {
        version: String,
        err: DlError,
    },
}

impl Dl {
    /// Failed for good: a release without a signature can only be installed by hand.
    pub fn unsigned(&self) -> bool {
        matches!(
            self,
            Dl::Failed {
                err: DlError::Unsigned,
                ..
            }
        )
    }
}

pub(crate) struct Updates {
    pub status: Status,
    pub state: State,
    pub install: Install,
    pub dl: Dl,
    /// Set by "Restart and update": installed while Locus quits.
    pending: Option<PathBuf>,
    /// Online actions allowed (the real data folder).
    online: bool,
    /// Test versions are offered (`Settings::update_beta`, kept in step with the settings).
    beta: bool,
}

struct UpdatesGlobal(Entity<Updates>);
impl Global for UpdatesGlobal {}

/// Creates the entity. `load`: read `update.json` (the data folder is written to); `auto`: the
/// real data folder, so the automatic check and download may run.
pub(crate) fn init(load: bool, auto: bool, cx: &mut App) -> Entity<Updates> {
    let state = if load {
        State::load()
    } else {
        State::default()
    };
    let beta = crate::settings::get(cx).update_beta();
    let e = cx.new(|_| Updates {
        status: Status::Idle,
        state,
        install: Install::detect(),
        dl: Dl::Idle,
        pending: None,
        online: auto,
        beta,
    });
    // Turning "Receive test versions" on or off asks again: the answer depends on it.
    cx.observe_global::<crate::settings::AppSettings>(|cx| {
        let beta = crate::settings::get(cx).update_beta();
        if let Some(u) = entity(cx) {
            u.update(cx, |u, cx| {
                if u.beta != beta {
                    u.beta = beta;
                    if u.online {
                        u.check(true, cx);
                    }
                    cx.notify();
                }
            });
        }
    })
    .detach();
    cx.set_global(UpdatesGlobal(e.clone()));
    cx.on_app_quit(|cx| {
        if let Some(u) = entity(cx) {
            u.update(cx, |u, _| u.install_pending());
        }
        async {}
    })
    .detach();
    if auto {
        // Leftovers of an earlier update; a newer download still waiting is kept.
        let keep = e.read(cx).available().map(|r| r.version.clone());
        cx.background_executor()
            .spawn(async move { inst::cleanup(keep.as_deref()) })
            .detach();
        let weak = e.downgrade();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(START_DELAY).await;
            let _ = weak.update(cx, |u, cx| {
                let s = crate::settings::get(cx);
                if s.update_check() && u.state.due(update::now()) {
                    u.check(false, cx);
                } else if s.update_download() {
                    // Known from an earlier run: reuses the file if it is already there.
                    u.download(false, cx);
                }
            });
        })
        .detach();
    }
    e
}

pub(crate) fn entity(cx: &App) -> Option<Entity<Updates>> {
    cx.try_global::<UpdatesGlobal>().map(|g| g.0.clone())
}

impl Updates {
    /// The newer release known, if any.
    pub fn available(&self) -> Option<&Release> {
        self.state.available(update::CURRENT, self.beta)
    }

    /// The release the status bar points at (newer and not dismissed).
    pub fn hint(&self) -> Option<&Release> {
        self.available().filter(|r| self.state.should_hint(r))
    }

    /// Whether Locus can update this copy itself (not Scoop's).
    pub fn can_install(&self) -> bool {
        self.install.command().is_none()
    }

    /// The download state of the release on offer (`Dl::Idle` when it is for another one).
    pub fn dl_for_available(&self) -> &Dl {
        let v = self.available().map(|r| r.version.as_str());
        let of = match &self.dl {
            Dl::Idle => None,
            Dl::Downloading { version, .. }
            | Dl::Ready { version, .. }
            | Dl::Failed { version, .. } => Some(version.as_str()),
        };
        if v.is_some() && v == of {
            &self.dl
        } else {
            &Dl::Idle
        }
    }

    /// Asks GitHub in the background. `manual`: from Settings (no toast, the page shows it).
    pub fn check(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.status == Status::Checking {
            return;
        }
        self.status = Status::Checking;
        cx.notify();
        let beta = self.beta;
        let task = cx
            .background_executor()
            .spawn(async move { update::check(beta) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |u, cx| {
                u.finish(result, manual, cx);
                // The switch changed while asking: ask again with the new choice.
                if u.beta != beta && u.online {
                    u.check(true, cx);
                }
            });
        })
        .detach();
    }

    fn finish(
        &mut self,
        result: Result<Option<Release>, String>,
        manual: bool,
        cx: &mut Context<Self>,
    ) {
        self.state.last_check = update::now();
        match result {
            Ok(found) => {
                let new = found.is_some() && found != self.state.found;
                self.state.found = found;
                self.status = Status::UpToDate;
                let auto_dl = crate::settings::get(cx).update_download() && self.can_install();
                if let Some(r) = self.hint().filter(|_| new && !manual && !auto_dl) {
                    let msg = t!("upd.toast", v = r.version.clone()).into_owned();
                    crate::shell::toast::show(msg, cx);
                }
                if auto_dl {
                    self.download(false, cx);
                }
            }
            Err(e) => {
                log::info!("update check failed: {e}");
                self.status = Status::Failed(e);
            }
        }
        self.save(cx);
        cx.notify();
    }

    /// Downloads and checks the release on offer in the background. `manual`: "Download and
    /// update" (retries a failed download); automatic downloads only run online.
    pub fn download(&mut self, manual: bool, cx: &mut Context<Self>) {
        let Some(version) = self.available().map(|r| r.version.clone()) else {
            return;
        };
        if !self.can_install() || (!manual && !self.online) {
            return;
        }
        match self.dl_for_available() {
            Dl::Downloading { .. } | Dl::Ready { .. } => return,
            Dl::Failed { .. } if !manual => return,
            _ => {}
        }
        let progress = Arc::new(Progress::default());
        self.dl = Dl::Downloading {
            version: version.clone(),
            progress: progress.clone(),
        };
        cx.notify();
        let install = self.install;
        let (v, p) = (version.clone(), progress.clone());
        let task = cx
            .background_executor()
            .spawn(async move { inst::download(&v, install, &p) });
        // Repaint while it runs, for the progress.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let running = this
                    .update(cx, |u, cx| {
                        cx.notify();
                        matches!(u.dl, Dl::Downloading { .. })
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |u, cx| {
                u.dl = match result {
                    Ok(path) => {
                        if !manual && u.hint().is_some() {
                            let msg = t!("upd.toast_ready", v = version.clone()).into_owned();
                            crate::shell::toast::show(msg, cx);
                        }
                        Dl::Ready { version, path }
                    }
                    Err(err) => {
                        log::info!("update download failed: {err}");
                        Dl::Failed { version, err }
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// "Restart and update": marks the ready download to be installed while Locus quits.
    /// The caller then closes the window.
    pub fn request_install(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let Dl::Ready { path, .. } = self.dl_for_available().clone() else {
            return Err(t!("upd.not_ready").into_owned());
        };
        if self.install == Install::Portable {
            inst::can_swap().map_err(|e| t!("upd.cannot_write", msg = e).into_owned())?;
        }
        self.pending = Some(path);
        cx.notify();
        Ok(())
    }

    /// Runs at quit: puts the pending update in place and starts the new version.
    fn install_pending(&mut self) {
        let Some(path) = self.pending.take() else {
            return;
        };
        match inst::apply(&path, self.install, true) {
            Ok(()) => log::info!("update: {} handed over, quitting", path.display()),
            Err(e) => log::error!("update could not be installed: {e}"),
        }
    }

    /// No status bar hint for this version any more (Settings › About still shows it).
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if let Some(v) = self.available().map(|r| r.version.clone()) {
            self.state.dismissed = Some(v);
            self.save(cx);
            cx.notify();
        }
    }

    fn save(&self, cx: &mut Context<Self>) {
        let s = self.state.clone();
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = s.save() {
                    log::warn!("update.json could not be saved: {e}");
                }
            })
            .detach();
    }

    /// One line for Settings › About: what is known and when it was last checked.
    pub fn summary(&self) -> String {
        let when = (self.state.last_check > 0)
            .then(|| locus_core::history::format_time(self.state.last_check));
        match (&self.status, self.available(), self.dl_for_available()) {
            (_, Some(_), Dl::Downloading { version, progress }) => match progress.percent() {
                Some(p) => t!("upd.downloading_pct", v = version.clone(), p = p).into_owned(),
                None => t!("upd.downloading", v = version.clone()).into_owned(),
            },
            (_, Some(_), Dl::Ready { version, .. }) => {
                t!("upd.ready", v = version.clone()).into_owned()
            }
            (_, Some(_), Dl::Failed { version, err }) => match err {
                DlError::Unsigned => t!("upd.unsigned", v = version.clone()).into_owned(),
                DlError::BadSignature(e) => t!("upd.bad_signature", msg = e.clone()).into_owned(),
                DlError::Failed(e) => t!("upd.dl_failed", msg = e.clone()).into_owned(),
            },
            (Status::Checking, _, _) => t!("upd.checking").into_owned(),
            (_, Some(r), _) => t!("upd.available", v = r.version.clone()).into_owned(),
            (Status::Failed(e), None, _) => t!("upd.failed", msg = e.clone()).into_owned(),
            (_, None, _) => match when {
                Some(w) => t!("upd.up_to_date", when = w).into_owned(),
                None => t!("upd.never").into_owned(),
            },
        }
    }
}

/// `--after-update <pid>`: started by the updater of the version that just quit. Waits (at
/// most 15 s) for process `pid` to end, so the two never run side by side; 0 = nothing to wait
/// for (the installer started us).
pub(crate) fn wait_for_previous(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};
    const SYNCHRONIZE: u32 = 0x0010_0000;
    if pid == 0 || pid == std::process::id() {
        return;
    }
    // SAFETY: plain Win32 calls on a handle we own; a null handle (already gone) is skipped.
    unsafe {
        let h = OpenProcess(SYNCHRONIZE, 0, pid);
        if !h.is_null() {
            WaitForSingleObject(h, 15_000);
            CloseHandle(h);
        }
    }
}
