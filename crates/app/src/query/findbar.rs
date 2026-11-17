//! The editor's find / replace bar (Ctrl+F / Ctrl+H). gpui-kit's built-in search panel has fixed
//! small controls and cannot be styled, so the editor is not `searchable` and this bar drives
//! its search API (`set_search_query`, `next_search_match`, `replace_*`).
use crate::query::tab::QueryTab;
use crate::shell::scale::*;
use crate::shell::*;
use gpui_kit::component::input::{Input, InputEvent, InputState};

pub struct FindBar {
    pub find: Entity<InputState>,
    pub replace: Entity<InputState>,
    pub replace_mode: bool,
    /// Match case (the "Aa" toggle).
    pub case_sensitive: bool,
    _subs: Vec<Subscription>,
}

impl QueryTab {
    /// Opens the bar (or just focuses it), seeded with the selected text.
    pub fn open_find(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.editor.read(cx).state.clone();
        let selected = state.read(cx).selected_text().to_string();
        let seed = !selected.is_empty() && !selected.contains('\n');
        if let Some(f) = self.find.as_mut() {
            f.replace_mode = f.replace_mode || replace;
            if seed {
                f.find.update(cx, |i, cx| i.set_value(selected, window, cx));
            }
            f.find.update(cx, |i, cx| i.focus(window, cx));
            cx.notify();
            return;
        }
        let find = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("t4.find_placeholder").to_string())
        });
        let rep = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("t4.replace_placeholder").to_string())
        });
        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(
            &find,
            window,
            |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => this.run_find(cx),
                InputEvent::PressEnter { .. } => this.find_step(true, window, cx),
                _ => {}
            },
        ));
        subs.push(cx.subscribe_in(
            &rep,
            window,
            |this: &mut Self, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.replace_one(window, cx);
                }
            },
        ));
        if seed {
            find.update(cx, |i, cx| i.set_value(selected, window, cx));
        }
        find.update(cx, |i, cx| i.focus(window, cx));
        self.find = Some(FindBar {
            find,
            replace: rep,
            replace_mode: replace,
            case_sensitive: false,
            _subs: subs,
        });
        self.run_find(cx);
        cx.notify();
    }

    pub fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.take().is_some() {
            let state = self.editor.read(cx).state.clone();
            state.update(cx, |s, cx| s.close_search(cx));
            state.read(cx).focus_handle(cx).focus(window, cx);
            cx.notify();
        }
    }

    fn run_find(&mut self, cx: &mut Context<Self>) {
        let Some(f) = &self.find else { return };
        let q = f.find.read(cx).value().to_string();
        let ci = !f.case_sensitive;
        let state = self.editor.read(cx).state.clone();
        state.update(cx, |s, cx| s.set_search_query(q, ci, cx));
        cx.notify();
    }

    fn find_step(&mut self, forward: bool, _: &mut Window, cx: &mut Context<Self>) {
        let state = self.editor.read(cx).state.clone();
        state.update(cx, |s, cx| {
            if forward {
                s.next_search_match(cx);
            } else {
                s.previous_search_match(cx);
            }
        });
        cx.notify();
    }

    fn replace_one(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = &self.find else { return };
        let r = f.replace.read(cx).value().to_string();
        let state = self.editor.read(cx).state.clone();
        state.update(cx, |s, cx| {
            s.replace_current_search_match(&r, window, cx);
        });
        self.run_find(cx);
    }

    fn replace_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = &self.find else { return };
        let r = f.replace.read(cx).value().to_string();
        let state = self.editor.read(cx).state.clone();
        state.update(cx, |s, cx| {
            s.replace_all_search_matches(&r, window, cx);
        });
        self.run_find(cx);
    }

    /// The bar above the editor, or `None` while closed.
    pub fn render_find(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let f = self.find.as_ref()?;
        let t = crate::shell::theme::tokens(cx);
        let label = self
            .editor
            .read(cx)
            .state
            .read(cx)
            .search_session()
            .matcher
            .label();
        let has_query = !f.find.read(cx).value().is_empty();
        let count_col = if has_query && label.starts_with("0/") {
            t.red_text
        } else {
            t.tx3
        };
        let tip_of = |k: &str| tip(t!(k).into_owned());
        let case = ib(
            "find-case",
            crate::shell::icons::icon("case-sensitive"),
            H_CONTROL,
            f.case_sensitive,
            cx,
        )
        .tooltip(tip_of("t4.find_case"))
        .on_click(cx.listener(|this, _, _, cx| {
            if let Some(f) = this.find.as_mut() {
                f.case_sensitive = !f.case_sensitive;
            }
            this.run_find(cx);
        }));
        let prev = ib(
            "find-prev",
            crate::shell::icons::icon("chevron-up"),
            H_CONTROL,
            false,
            cx,
        )
        .tooltip(tip_of("t4.find_prev"))
        .on_click(cx.listener(|this, _, w, cx| this.find_step(false, w, cx)));
        let next = ib(
            "find-next",
            crate::shell::icons::icon("chevron-down"),
            H_CONTROL,
            false,
            cx,
        )
        .tooltip(tip_of("t4.find_next"))
        .on_click(cx.listener(|this, _, w, cx| this.find_step(true, w, cx)));
        let toggle = ib(
            "find-replace-toggle",
            crate::shell::icons::icon("replace"),
            H_CONTROL,
            f.replace_mode,
            cx,
        )
        .tooltip(tip_of("t4.find_replace_toggle"))
        .on_click(cx.listener(|this, _, _, cx| {
            if let Some(f) = this.find.as_mut() {
                f.replace_mode = !f.replace_mode;
            }
            cx.notify();
        }));
        let close = ib(
            "find-close",
            crate::shell::icons::icon("x"),
            H_CONTROL,
            false,
            cx,
        )
        .tooltip(tip_of("t4.find_close"))
        .on_click(cx.listener(|this, _, w, cx| this.close_find(w, cx)));
        let row1 = h_flex()
            .gap(px(S4))
            .items_center()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&f.find).px(px(S5))),
            )
            .child(case)
            .child(prev)
            .child(next)
            .child(
                div()
                    .flex_none()
                    .min_w(px(48.))
                    .text_size(px(TX_SM))
                    .text_color(count_col)
                    .child(if has_query { label } else { String::new() }),
            )
            .child(toggle)
            .child(close);
        let row2 = f.replace_mode.then(|| {
            let one = ib(
                "replace-one",
                crate::shell::icons::icon("replace"),
                H_CONTROL,
                false,
                cx,
            )
            .tooltip(tip_of("t4.replace_one"))
            .on_click(cx.listener(|this, _, w, cx| this.replace_one(w, cx)));
            let all = ib(
                "replace-all",
                crate::shell::icons::icon("replace-all"),
                H_CONTROL,
                false,
                cx,
            )
            .tooltip(tip_of("t4.replace_all"))
            .on_click(cx.listener(|this, _, w, cx| this.replace_all(w, cx)));
            h_flex()
                .gap(px(S4))
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&f.replace).px(px(S5))),
                )
                .child(one)
                .child(all)
        });
        Some(
            v_flex()
                .id("find-bar")
                .key_context("FindBar")
                .flex_none()
                .gap(px(S4))
                .px(px(S5))
                .py(px(S4))
                .border_b_1()
                .border_color(t.ln)
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                    if ev.keystroke.key == "escape" {
                        this.close_find(w, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(row1)
                .children(row2)
                .into_any_element(),
        )
    }
}
