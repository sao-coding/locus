//! The update check in the app (logic: `locus_core::update`). One `Updates` entity, a global:
//! the status bar shows a hint when a newer release was found, Settings › About shows the state
//! and the next action. Only tells; the user downloads (or runs `scoop update`) by hand.
//!
//! The automatic check runs a little after start, so it never competes with the first frames,
//! and only for the real data folder (not the demo / UI-test scenes).
use gpui_kit::*;
use locus_core::update::{self, Install, Release, State};
use rust_i18n::t;
use std::time::Duration;

/// Wait after start before the automatic check.
const START_DELAY: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// Nothing asked this run (the state may still hold what an earlier run found).
    Idle,
    Checking,
    /// The last check this run found nothing newer.
    UpToDate,
    Failed(String),
}

pub(crate) struct Updates {
    pub status: Status,
    pub state: State,
    pub install: Install,
}

struct UpdatesGlobal(Entity<Updates>);
impl Global for UpdatesGlobal {}

/// Creates the entity. `load`: read `update.json` (the data folder is written to); `auto`:
/// start the automatic check when it is on and due (the real data folder only).
pub(crate) fn init(load: bool, auto: bool, cx: &mut App) -> Entity<Updates> {
    let e = cx.new(|_| Updates {
        status: Status::Idle,
        state: if load {
            State::load()
        } else {
            State::default()
        },
        install: Install::detect(),
    });
    cx.set_global(UpdatesGlobal(e.clone()));
    if auto {
        let weak = e.downgrade();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(START_DELAY).await;
            let _ = weak.update(cx, |u, cx| {
                if crate::settings::get(cx).update_check() && u.state.due(update::now()) {
                    u.check(false, cx);
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
        self.state.available(update::CURRENT)
    }

    /// The release the status bar points at (newer and not dismissed).
    pub fn hint(&self) -> Option<&Release> {
        self.available().filter(|r| self.state.should_hint(r))
    }

    /// Asks GitHub in the background. `manual`: from Settings (no toast, the page shows it).
    pub fn check(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.status == Status::Checking {
            return;
        }
        self.status = Status::Checking;
        cx.notify();
        let task = cx.background_executor().spawn(async { update::check() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |u, cx| u.finish(result, manual, cx));
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
                if let Some(r) = self.hint().filter(|_| new && !manual) {
                    let msg = t!("upd.toast", v = r.version.clone()).into_owned();
                    crate::shell::toast::show(msg, cx);
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
        match (&self.status, self.available()) {
            (Status::Checking, _) => t!("upd.checking").into_owned(),
            (_, Some(r)) => t!("upd.available", v = r.version.clone()).into_owned(),
            (Status::Failed(e), None) => t!("upd.failed", msg = e.clone()).into_owned(),
            (_, None) => match when {
                Some(w) => t!("upd.up_to_date", when = w).into_owned(),
                None => t!("upd.never").into_owned(),
            },
        }
    }
}
