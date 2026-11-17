//! The shared query-history store (an entity, so the History tab and the
//! palette see new entries). The entries, filtering and formatting are in
//! `locus_core::history`; the History tab is `page.rs`.

pub mod page;

use gpui_kit::*;
use locus_core::history::{Entry, load, push_capped, save};
use std::time::Duration;

/// Emitted after each added entry.
pub struct HistoryChanged;

pub struct History {
    pub entries: Vec<Entry>,
    persist: bool,
    save_task: Option<Task<()>>,
}

impl EventEmitter<HistoryChanged> for History {}

impl History {
    pub fn new(persist: bool) -> Self {
        let entries = if persist { load() } else { Vec::new() };
        Self {
            entries,
            persist,
            save_task: None,
        }
    }

    pub fn add(&mut self, e: Entry, cx: &mut Context<Self>) {
        push_capped(&mut self.entries, e);
        cx.emit(HistoryChanged);
        cx.notify();
        if self.persist {
            let snapshot = serde_json::to_vec(&self.entries).unwrap_or_default();
            // Written off the UI thread, at most once per second.
            self.save_task = Some(cx.spawn(async move |_, cx| {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let r = cx
                    .background_executor()
                    .spawn(async move { save(&snapshot) })
                    .await;
                if let Err(e) = r {
                    log::warn!("history.json could not be saved: {e}");
                    cx.update(|cx| {
                        crate::shell::toast::show_once(
                            "history-save",
                            rust_i18n::t!("dataerr.history_save", msg = e.to_string()).to_string(),
                            cx,
                        )
                    });
                }
            }));
        }
    }
}
