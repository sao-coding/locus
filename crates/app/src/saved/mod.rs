//! Saved queries and snippets: the shared store (an entity, so the Saved
//! tab and the palette see changes at once). Files and logic are in
//! `locus_core::saved`; the tab is `page.rs`.

pub mod ops;
pub mod page;

use gpui_kit::*;
use locus_core::saved::{self, SavedQuery, Snippet};

/// Emitted after every change of queries or snippets.
pub struct SavedChanged;

pub struct SavedLib {
    pub queries: Vec<SavedQuery>,
    /// The user's snippets (built-ins are added by `saved::all_snippets`).
    pub snippets: Vec<Snippet>,
    persist: bool,
}

impl EventEmitter<SavedChanged> for SavedLib {}

impl SavedLib {
    pub fn new(persist: bool) -> Self {
        let (queries, snippets) = if persist {
            (saved::load_queries(), saved::load_snippets())
        } else {
            (Vec::new(), Vec::new())
        };
        crate::query::completion::set_user_snippets(&snippets);
        Self {
            queries,
            snippets,
            persist,
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        if self.persist {
            for r in [
                saved::save_queries(&self.queries),
                saved::save_snippets(&self.snippets),
            ] {
                if let Err(e) = r {
                    log::warn!("saved queries / snippets could not be saved: {e}");
                    crate::shell::toast::show_once(
                        "saved-save",
                        rust_i18n::t!("dataerr.saved_save", msg = e.to_string()).to_string(),
                        cx,
                    );
                }
            }
        }
        crate::query::completion::set_user_snippets(&self.snippets);
        cx.emit(SavedChanged);
        cx.notify();
    }

    /// Adds a query; returns its id.
    pub fn add(&mut self, mut q: SavedQuery, cx: &mut Context<Self>) -> String {
        q.id = locus_core::store::new_id();
        q.updated = locus_core::history::now_secs();
        let id = q.id.clone();
        self.queries.push(q);
        self.changed(cx);
        id
    }

    /// Replaces the query with the same id.
    pub fn update(&mut self, mut q: SavedQuery, cx: &mut Context<Self>) {
        if let Some(x) = self.queries.iter_mut().find(|x| x.id == q.id) {
            q.updated = locus_core::history::now_secs();
            *x = q;
            self.changed(cx);
        }
    }

    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        self.queries.retain(|q| q.id != id);
        self.changed(cx);
    }

    pub fn import(&mut self, json: &str, cx: &mut Context<Self>) -> Result<usize, String> {
        let n = saved::import_json(&mut self.queries, json)?;
        self.changed(cx);
        Ok(n)
    }

    pub fn export(&self) -> String {
        saved::export_json(&self.queries)
    }

    pub fn set_snippets(&mut self, s: Vec<Snippet>, cx: &mut Context<Self>) {
        self.snippets = s;
        self.changed(cx);
    }
}
