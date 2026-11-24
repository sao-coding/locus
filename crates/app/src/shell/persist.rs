//! Persistence of spaces.json: snapshot, debounced save.
use super::*;

impl Locus {
    // ---- persistence ------------------------------------------------------

    pub(crate) fn snapshot(&self, cx: &App) -> SpacesFile {
        SpacesFile {
            spaces: self
                .spaces
                .iter()
                .map(|s| SpaceData {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    color: s.color,
                    icon: s.icon.clone(),
                    custom_hue: Some(s.theme.primary().h),
                    theme: Some(s.theme.clone()),
                    connections: s.connections.clone(),
                    tabs: s.tabs.iter().map(|t| t.read(cx).data(cx)).collect(),
                    active_tab: Some(s.focused_tab_id()),
                    layout: Some(s.layout.data(cx)),
                    focused_pane: s.focused,
                    view: s.view,
                    folder: s.folder.clone(),
                    groups: s.groups_data(cx),
                })
                .collect(),
            active_space: self.active_space,
            sidebar: self.sidebar,
            sidebar_width: self.sidebar_width,
            right_panel: self.right_panel,
            right_width: self.right_width,
        }
    }

    pub fn schedule_save(&mut self, cx: &mut Context<Self>) {
        if !self.store.persists() {
            return;
        }
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(800))
                .await;
            let _ = this.update(cx, |this, cx| this.save_now(cx));
        }));
    }

    pub(crate) fn save_now(&mut self, cx: &mut Context<Self>) {
        self.save_task = None;
        if !self.store.persists() {
            return;
        }
        let saved = self.snapshot(cx).save();
        if let Err(e) = &saved {
            log::warn!("spaces.json could not be saved: {e}");
            crate::shell::toast::show_once(
                "spaces-save",
                t!("dataerr.spaces_save", msg = e.to_string()).to_string(),
                cx,
            );
        }
        if saved.is_ok() {
            for t in self
                .spaces
                .iter()
                .flat_map(|s| s.tabs.iter())
                .cloned()
                .collect::<Vec<_>>()
            {
                t.update(cx, |t, cx| {
                    if t.doc.dirty {
                        t.doc.dirty = false;
                        cx.notify();
                    }
                });
            }
            cx.notify();
        } else if !self.close_requested {
            // The file can be held open by another program for a moment
            // (rename fails on Windows then): try again shortly.
            self.schedule_save(cx);
        }
    }
}
