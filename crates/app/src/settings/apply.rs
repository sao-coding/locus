//! Language switch and applying saved settings (the Settings tab is `page.rs`).
use crate::shell::*;

impl Locus {
    pub(crate) fn set_language(
        &mut self,
        lang: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        rust_i18n::set_locale(lang);
        gpui_kit::component::set_locale(lang);
        if self.store.persists() {
            crate::settings::update(cx, |s| s.language = Some(lang.to_string()));
        }
        let tables: Vec<_> = self
            .spaces
            .iter()
            .flat_map(|s| s.tabs.iter())
            .flat_map(|t| t.read(cx).out.results.iter().map(|r| r.table.clone()))
            .collect();
        for t in tables {
            t.update(cx, |t, cx| t.refresh(cx));
        }
        window.set_window_title(&t!("app.title"));
        cx.refresh_windows();
    }
}

impl Locus {
    /// Applies what the Settings tab saved: theme, material, motion.
    pub(crate) fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reduce_motion = crate::settings::get(cx).reduce_motion;
        apply_theme(window, cx);
        // Settings › Git may have changed the git path: look again.
        self.git_file_saved(cx);
        cx.refresh_windows();
    }
}
