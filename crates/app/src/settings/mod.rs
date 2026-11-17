//! The Settings tab and applying saved settings (the model is
//! `locus_core::settings`).
pub mod apply;
pub mod page;

use gpui_kit::*;
use locus_core::settings::Settings;
use rust_i18n::t;
use std::sync::Mutex;

/// The settings, read once at start-up and kept here (`cx.global`). Change
/// them with [`update`], which also saves.
pub struct AppSettings(pub Settings);
impl Global for AppSettings {}

/// Installs the settings read once at start-up (`Settings::load`).
pub fn init(settings: Settings, cx: &mut App) {
    cx.set_global(AppSettings(settings));
}

/// The current settings (defaults when `init` was not called, e.g. in tests).
pub fn get(cx: &App) -> &Settings {
    static DEFAULT: std::sync::OnceLock<Settings> = std::sync::OnceLock::new();
    cx.try_global::<AppSettings>()
        .map(|g| &g.0)
        .unwrap_or_else(|| DEFAULT.get_or_init(Settings::default))
}

/// Latest snapshot waiting to be written; a writer takes it, so several quick
/// changes become one write and the newest always wins.
static PENDING: Mutex<Option<Settings>> = Mutex::new(None);

/// Changes the settings in memory, then saves them off the UI thread. A failed
/// save is logged and shown once as a toast.
pub fn update(cx: &mut App, change: impl FnOnce(&mut Settings)) {
    let mut s = get(cx).clone();
    change(&mut s);
    cx.set_global(AppSettings(s.clone()));
    save(s, cx);
}

/// Saves `s` in the background (also used by `update`).
pub fn save(s: Settings, cx: &mut App) {
    if let Ok(mut p) = PENDING.lock() {
        *p = Some(s);
    }
    let write = cx.background_executor().spawn(async {
        let latest = PENDING.lock().ok().and_then(|mut p| p.take());
        latest.map(|s| s.save())
    });
    cx.spawn(async move |cx| {
        if let Some(Err(e)) = write.await {
            log::warn!("settings.json could not be saved: {e}");
            cx.update(|cx| {
                crate::shell::toast::show_once(
                    "settings-save",
                    t!("dataerr.settings_save", msg = e.to_string()).to_string(),
                    cx,
                )
            });
        }
    })
    .detach();
}
