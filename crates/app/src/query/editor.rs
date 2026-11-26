//! SQL editor: gpui-component's `EditorState` with tree-sitter SQL, plus a
//! size limit above which the buffer is shown as plain text.
//!
//! Why a limit: the tree-sitter syntax tree costs roughly 40 bytes per source
//! byte (POC: a 4.6 MB / 100K-line file added ~180 MB). Zed keeps full
//! parsing and bounds *queries* to visible chunks (`MAX_BYTES_TO_QUERY`,
//! crates/language/src/syntax_map.rs); the component here parses the whole
//! buffer, so the bound has to be on the buffer size instead. The limit was
//! picked by measurement.

use gpui_kit::component::input::{EditorState, InputEditsEvent, InputEvent};
use gpui_kit::*;
use locus_core::snippet::{Session, Step};

/// Default limit for syntax highlighting, in bytes of buffer text.
pub const DEFAULT_SYNTAX_LIMIT: usize = 1024 * 1024;

pub struct SqlEditor {
    pub state: Entity<EditorState>,
    limit: usize,
    /// True while the buffer is over the limit and shown as plain text.
    pub plain: bool,
    /// The snippet being filled in (Tab / Shift+Tab visit its stops), if any.
    pub snippet: Option<Session>,
    _sub: Subscription,
    _edits: Subscription,
}

impl SqlEditor {
    pub fn new(text: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let limit = std::env::var("LOCUS_SYNTAX_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_SYNTAX_LIMIT);
        let plain = text.len() > limit;
        let lang = if plain { "text" } else { "sql" };
        let text = text.to_string();
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .searchable(false)
                .language(lang)
                .line_number(true)
                .soft_wrap(false)
                .placeholder(rust_i18n::t!("t2.editor_placeholder").to_string())
                .default_value(text)
        });
        let _sub = cx.subscribe(&state, |this: &mut Self, _, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.apply_policy(cx);
            }
        });
        let _edits = cx.subscribe(&state, |this: &mut Self, _, ev: &InputEditsEvent, cx| {
            this.track_snippet(ev, cx)
        });
        Self {
            state,
            limit,
            plain,
            snippet: None,
            _sub,
            _edits,
        }
    }

    /// Hysteresis of 10 % so typing around the limit does not flip modes.
    fn apply_policy(&mut self, cx: &mut Context<Self>) {
        let len = self.state.read(cx).text().len();
        let want_plain = if self.plain {
            len > self.limit * 9 / 10
        } else {
            len > self.limit
        };
        if want_plain != self.plain {
            self.plain = want_plain;
            let lang = if want_plain { "text" } else { "sql" };
            self.state.update(cx, |s, cx| s.set_highlighter(lang, cx));
            cx.notify();
        }
    }
}

impl crate::query::tab::QueryTab {
    /// Tab in the editor: accepts the highlighted completion while the popup is
    /// open (as Enter does), otherwise does what Tab did before (indent, snippet
    /// stops). Never accepts during an IME composition.
    pub fn accept_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::input::{Enter, IndentInline};
        let state = self.editor.read(cx).state.clone();
        let open = state.read(cx).completion_menu_state().open;
        let composing = state.update(cx, |s, cx| {
            gpui::EntityInputHandler::marked_text_range(s, window, cx).is_some()
        });
        if !open && !composing && self.editor.update(cx, |e, cx| e.snippet_active(cx)) {
            self.editor.update(cx, |e, cx| e.snippet_next(cx));
            return;
        }
        let action: Box<dyn Action> = if open && !composing {
            Box::new(Enter {
                secondary: false,
                shift: false,
            })
        } else {
            Box::new(IndentInline)
        };
        window.dispatch_action(action, cx);
    }

    /// Shift+Tab while a snippet is being filled in: back to the previous stop.
    pub fn snippet_prev(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.editor.read(cx).state.clone();
        let composing = state.update(cx, |s, cx| {
            gpui::EntityInputHandler::marked_text_range(s, window, cx).is_some()
        });
        if !composing {
            self.editor.update(cx, |e, cx| e.snippet_prev(cx));
        }
    }

    /// Esc while a snippet is being filled in: leave snippet mode.
    pub fn snippet_exit(&mut self, cx: &mut Context<Self>) {
        self.editor.update(cx, |e, cx| e.snippet_exit(cx));
    }
}

impl SqlEditor {
    /// Follows every edit: a text equal to a snippet's expansion means it was
    /// just accepted (start its stops, select the first); later edits move the
    /// stops, and one that cuts across them ends snippet mode.
    fn track_snippet(&mut self, ev: &InputEditsEvent, cx: &mut Context<Self>) {
        for (edit, rope) in ev.edits() {
            let new_len = edit.new_end_byte - edit.start_byte;
            if let Some(s) = &mut self.snippet {
                if !s.on_edit(edit.start_byte, edit.old_end_byte, new_len) {
                    self.snippet = None;
                }
                continue;
            }
            if !(2..=4096).contains(&new_len) {
                continue;
            }
            let inserted = rope.slice(edit.start_byte..edit.new_end_byte).to_string();
            if let Some(stops) = super::completion::snippet_stops_for(&inserted)
                && let Some(s) = Session::new(edit.start_byte, inserted.len(), &stops)
            {
                let first = s.first();
                self.snippet = Some(s);
                self.state
                    .update(cx, |st, cx| st.set_selected_range(first, cx));
            }
        }
        cx.notify();
    }

    /// Whether snippet mode is on: a snippet is live and the caret is in it.
    pub fn snippet_active(&self, cx: &App) -> bool {
        self.snippet
            .as_ref()
            .is_some_and(|s| s.contains(&self.state.read(cx).selected_range()))
    }

    pub fn snippet_next(&mut self, cx: &mut Context<Self>) {
        let Some(s) = &mut self.snippet else { return };
        let step = s.advance();
        let range = match step {
            Step::Select(r) => r,
            Step::Finish(at) => {
                self.snippet = None;
                at..at
            }
        };
        self.state
            .update(cx, |st, cx| st.set_selected_range(range, cx));
        cx.notify();
    }

    pub fn snippet_prev(&mut self, cx: &mut Context<Self>) {
        let Some(s) = &mut self.snippet else { return };
        let range = s.retreat();
        self.state
            .update(cx, |st, cx| st.set_selected_range(range, cx));
    }

    pub fn snippet_exit(&mut self, cx: &mut Context<Self>) {
        self.snippet = None;
        cx.notify();
    }
}
