//! SQL editor: gpui-component's `EditorState` with tree-sitter SQL, plus a
//! size limit above which the buffer is shown as plain text.
//!
//! Why a limit: the tree-sitter syntax tree costs roughly 40 bytes per source
//! byte (POC: a 4.6 MB / 100K-line file added ~180 MB). Zed keeps full
//! parsing and bounds *queries* to visible chunks (`MAX_BYTES_TO_QUERY`,
//! crates/language/src/syntax_map.rs); the component here parses the whole
//! buffer, so the bound has to be on the buffer size instead. The limit was
//! picked by measurement.

use gpui_kit::component::input::{EditorState, InputEvent};
use gpui_kit::*;

/// Default limit for syntax highlighting, in bytes of buffer text.
pub const DEFAULT_SYNTAX_LIMIT: usize = 1024 * 1024;

pub struct SqlEditor {
    pub state: Entity<EditorState>,
    limit: usize,
    /// True while the buffer is over the limit and shown as plain text.
    pub plain: bool,
    _sub: Subscription,
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
        Self {
            state,
            limit,
            plain,
            _sub,
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
}
