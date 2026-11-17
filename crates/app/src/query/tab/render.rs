use super::*;

impl QueryTab {
    pub(super) fn render_results(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::results::ui::{Pane, Special};
        let phase = self.status.read(cx).phase;
        let showing_tool = matches!(self.ui.special, Some(Special::Plan) | Some(Special::Stats));
        if self.out.results.is_empty() && self.out.messages.is_empty() && !showing_tool {
            let text = match phase {
                Phase::Connecting => t!("state.connecting"),
                Phase::Running | Phase::Cancelling => t!("results.waiting"),
                _ => t!("grid.empty"),
            };
            let hint = matches!(phase, Phase::Idle).then(|| t!("t2.run_hint").into_owned());
            return empty_with_hint(text.into_owned(), hint, cx);
        }
        // Relations of the tables this query uses: a diagram limited to them.
        let query_tables = {
            let mut v: Vec<(Option<String>, String)> = Vec::new();
            for r in db_core::sql::split_statements(&self.last_sql) {
                let stmt = &self.last_sql[r];
                for (_, schema, table) in locus_core::sqlrefs::table_refs(stmt) {
                    if !v.iter().any(|(s, t)| {
                        t.eq_ignore_ascii_case(&table)
                            && (s.is_none() || schema.is_none() || *s == schema)
                    }) {
                        v.push((schema, table));
                    }
                }
            }
            if v.is_empty()
                && let Some(t) = self.current_table()
            {
                for c in t.read(cx).delegate().rs.columns() {
                    if let Some(o) = &c.origin
                        && !o.schema.is_empty()
                        && !v
                            .iter()
                            .any(|(s, t)| s.as_deref() == Some(o.schema.as_str()) && *t == o.table)
                    {
                        v.push((Some(o.schema.clone()), o.table.clone()));
                    }
                }
            }
            v
        };
        let strip = self.render_strip(query_tables, cx);
        let body = match self.pane() {
            Pane::Data(i) => self.render_result(&self.out.results[i], cx),
            Pane::NoData => self.render_no_data(cx),
            Pane::Messages => v_flex()
                .size_full()
                .child(div().flex_1().min_h_0().child(self.render_messages(cx)))
                .child(self.messages_footer(cx))
                .into_any_element(),
            Pane::Plan => self.render_plan(cx),
            Pane::Stats => self.render_stats(cx),
        };
        let bar = self.render_export_bar(cx);
        v_flex()
            .size_full()
            .child(strip)
            .children(bar)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    pub fn truncated_at(&self) -> Option<u64> {
        self.out.truncated
    }

    pub(super) fn render_result(&self, r: &ResultTab, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let g = r.table.read(cx).delegate();
        let note = r.note.map(|n| match n {
            Note::Cancelled => (
                t!("results.note_cancelled", n = fmt::count(r.rows)).into_owned(),
                theme.warning,
            ),
            Note::Failed => (
                t!("results.note_failed", n = fmt::count(r.rows)).into_owned(),
                theme.danger,
            ),
            Note::Truncated(n) => (
                t!("results.note_truncated", n = fmt::count(n)).into_owned(),
                theme.warning,
            ),
        });
        let staged = g.staged.count();
        let ro = g.read_only_reason.clone();
        let editable = g.editable();
        let no_new_rows = g.staged.new_rows.is_empty();
        let mut col = v_flex().size_full();
        if let Some((text, color)) = note {
            col = col.child(line_bar("note", IconName::Info, text, color, cx));
        }
        if ro.is_none() && editable && staged == 0 && self.ap.error.is_none() {
            col = col.child(
                h_flex()
                    .id("editable")
                    .flex_none()
                    .gap(px(sc::S4))
                    .px(px(sc::S5))
                    .py(px(sc::S2))
                    .items_center()
                    .text_size(px(sc::TX_SM))
                    .text_color(theme.muted_foreground)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(t!("edit.editable_hint").to_string()),
                    )
                    .child(
                        Button::new("add-row")
                            .small()
                            .ghost()
                            .icon(IconName::Plus)
                            .label(t!("grid.add_row").to_string())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(t) = this.current_table() {
                                    t.update(cx, |t, cx| {
                                        let r = t.delegate_mut().add_row();
                                        t.scroll_to_row(r, cx);
                                        cx.notify();
                                    });
                                }
                                cx.notify();
                            })),
                    ),
            );
        }
        if editable && (staged > 0 || self.ap.error.is_some()) {
            col = col.child(self.render_pending(staged, cx));
        }
        let body = if r.rows == 0 && no_new_rows && r.note == Some(Note::Cancelled) {
            empty(t!("results.cancelled_empty").into_owned(), cx)
        } else {
            DataTable::new(&r.table).bordered(false).into_any_element()
        };
        let pager = self.render_pager(r.rows, cx);
        col.child(Self::grid_actions(div().flex_1().min_h_0(), cx).child(body))
            .children(pager)
            .into_any_element()
    }

    pub(super) fn render_pending(&self, staged: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .id("pending")
            .flex_none()
            .px(px(sc::S5))
            .py(px(sc::S2))
            .gap(px(sc::S2))
            .bg(theme.warning.opacity(0.10))
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap(px(sc::S4))
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(sc::TX_BASE))
                            .font_medium()
                            .truncate()
                            .child(t!("edit.pending", n = staged.to_string()).to_string()),
                    )
                    .child(
                        Button::new("add-row")
                            .small()
                            .ghost()
                            .icon(IconName::Plus)
                            .tooltip(t!("grid.add_row").to_string())
                            .disabled(self.ap.applying)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(t) = this.current_table() {
                                    t.update(cx, |t, cx| {
                                        let r = t.delegate_mut().add_row();
                                        t.scroll_to_row(r, cx);
                                        cx.notify();
                                    });
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("preview")
                            .small()
                            .ghost()
                            .label(t!("edit.preview").to_string())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.preview_edits(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("discard")
                            .small()
                            .label(t!("edit.discard").to_string())
                            .disabled(self.ap.applying)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(t) = this.current_table() {
                                    t.update(cx, |t, cx| {
                                        t.delegate_mut().discard();
                                        cx.notify();
                                    });
                                }
                                this.ap.error = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("apply")
                            .small()
                            .primary()
                            .label(
                                if self.ap.applying {
                                    t!("edit.applying")
                                } else {
                                    t!("edit.apply")
                                }
                                .to_string(),
                            )
                            .disabled(self.ap.applying || staged == 0)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(TabEvent::ApplyRequest))),
                    ),
            )
            .when_some(self.ap.error.clone(), |v, e| {
                v.child(
                    div()
                        .id("apply-error")
                        .text_size(px(sc::TX_BASE))
                        .text_color(theme.danger)
                        .whitespace_normal()
                        .child(e),
                )
            })
    }

    pub(super) fn render_messages(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let tk = crate::shell::theme::tokens(cx);
        let mut list = v_flex()
            .id("messages")
            .size_full()
            .p(px(sc::S4))
            .gap(px(sc::S2))
            .overflow_y_scroll();
        for m in &self.out.messages {
            let (icon, color, text, detail) = match &m.kind {
                MsgKind::Rows(n) => (
                    IconName::Check,
                    theme.success,
                    t!("msg.rows", n = fmt::rows(*n)).into_owned(),
                    None,
                ),
                MsgKind::Affected(n) => (
                    IconName::Check,
                    theme.success,
                    t!("msg.affected", n = fmt::rows(*n)).into_owned(),
                    None,
                ),
                MsgKind::Truncated(n) => (
                    IconName::Info,
                    theme.warning,
                    t!("msg.truncated", n = fmt::count(*n)).into_owned(),
                    None,
                ),
                MsgKind::Cancelled(n) => (
                    IconName::Info,
                    theme.warning,
                    t!("msg.cancelled", n = fmt::rows(*n)).into_owned(),
                    None,
                ),
                MsgKind::Error(e, n) => {
                    let detail = match e.code {
                        Some(code) => format!("[{code}] {}", e.message),
                        None => e.message.clone(),
                    };
                    let title = if *n > 0 {
                        format!(
                            "{} · {}",
                            error_title(e.kind),
                            t!("msg.rows_kept", n = fmt::rows(*n))
                        )
                    } else {
                        error_title(e.kind)
                    };
                    (IconName::CircleX, theme.danger, title, Some(detail))
                }
            };
            let sql = m
                .sql
                .as_deref()
                .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            list = list.child(
                v_flex()
                    .id(("msg", m.index))
                    .gap(px(sc::S1))
                    .px(px(sc::S4))
                    .py(px(sc::S2))
                    .rounded_md()
                    .bg(theme.muted.opacity(0.4))
                    .child(
                        h_flex()
                            .gap(px(sc::S4))
                            .items_center()
                            .text_size(px(sc::TX_BASE))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{}.", m.index + 1)),
                            )
                            .child(Icon::new(icon).small().flex_none().text_color(color))
                            .child(div().flex_none().font_medium().child(text))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.muted_foreground)
                                    .font_family("monospace")
                                    .child(sql),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(sc::TX_SM))
                                    .text_color(theme.muted_foreground)
                                    .child(fmt::duration(m.elapsed)),
                            ),
                    )
                    .when_some(detail, |v, d| {
                        let (code, msg) = match d.strip_prefix('[').and_then(|r| r.split_once("] "))
                        {
                            Some((c, m)) => (Some(format!("[{c}]")), m.to_string()),
                            None => (None, d),
                        };
                        let mono = theme.mono_font_family.clone();
                        v.child(
                            h_flex()
                                .gap(px(crate::shell::scale::S3))
                                .items_start()
                                .text_size(px(crate::shell::scale::TX_SM))
                                .children(code.map(|c| {
                                    div()
                                        .flex_none()
                                        .font_family(mono)
                                        .text_color(tk.tx3)
                                        .child(c)
                                }))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .whitespace_normal()
                                        .text_color(tk.tx1)
                                        .child(msg),
                                ),
                        )
                    }),
            );
        }
        list.into_any_element()
    }

    /// The pane body: editor and results (toolbar and status bar are in the v3 card).
    pub fn render_pane(
        &self,
        _conns: &[SavedConnection],
        editor_size: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let editor = self.editor.read(cx).state.clone();
        let routine_bar = self.render_routine_bar(cx);
        let lower = match self.render_routine_errors(cx) {
            Some(e) => e,
            None => self.render_results(cx).into_any_element(),
        };
        v_flex()
            .key_context("QueryTab")
            .size_full()
            .children(routine_bar)
            .children(self.grid_menu.as_ref().map(|(m, pos, _)| {
                deferred(anchored().position(*pos).snap_to_window().child(m.clone()))
                    .with_priority(1)
            }))
            .children(self.render_find(cx))
            .child(
                div().flex_1().min_h_0().child(
                    v_resizable(SharedString::from(format!("work-split-{}", self.id)))
                        .child(
                            resizable_panel().size(editor_size).child(
                                div()
                                    .key_context("Editor")
                                    .size_full()
                                    .child(Editor::new(&editor).bordered(false).h_full()),
                            ),
                        )
                        .child(
                            resizable_panel().child(
                                div()
                                    .size_full()
                                    .border_t_1()
                                    .border_color(theme.border)
                                    .child(lower),
                            ),
                        ),
                ),
            )
    }
}

pub(crate) fn empty(text: String, cx: &App) -> AnyElement {
    empty_with_hint(text, None, cx)
}

/// Empty state: message in `tx3` 12 px, optional second line with a key hint.
pub(crate) fn empty_with_hint(text: String, hint: Option<String>, cx: &App) -> AnyElement {
    use crate::shell::scale::*;
    let t = crate::shell::theme::tokens(cx);
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap(px(S3))
        .text_size(px(TX_SM))
        .text_color(t.tx3)
        .child(text)
        .children(hint.map(|h| div().text_size(px(TX_MONO)).text_color(t.tx4).child(h)))
        .into_any_element()
}

pub(super) fn line_bar(
    id: &'static str,
    icon: IconName,
    text: String,
    color: Hsla,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    h_flex()
        .id(id)
        .flex_none()
        .gap(px(sc::S4))
        .px(px(sc::S5))
        .py(px(sc::S2))
        .items_center()
        .text_size(px(sc::TX_SM))
        .border_b_1()
        .border_color(theme.border)
        .child(Icon::new(icon).xsmall().flex_none().text_color(color))
        .child(div().flex_1().min_w_0().truncate().child(text))
}

impl Render for QueryTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(v) = &self.pageslot.view {
            return v.clone().into_any_element();
        }
        if self.diagram.is_some() {
            return self.render_diagram(window, cx).into_any_element();
        }
        if self.structure.is_some() {
            return self.render_structure(window, cx);
        }
        let conns = std::mem::take(&mut self.space_conns);
        // E7: the pane is the window minus the title bar, card header, toolbar and status bar (about 150 px).
        let pane_h = f32::from(window.viewport_size().height) - 150.0;
        let editor_h = locus_core::split::default_editor_height(pane_h);
        let el = self
            .render_pane(&conns, px(editor_h), cx)
            .into_any_element();
        self.space_conns = conns;
        el
    }
}

/// First non-blank, non-comment line of the buffer, read line by line from
/// the rope (never the whole text: the sidebar shows it every frame and a
/// buffer can be megabytes).
pub fn first_line_title(state: &gpui_kit::component::input::EditorState) -> String {
    use gpui_kit::component::input::RopeExt as _;
    let rope = state.text();
    for row in 0..rope.lines_len().min(50) {
        let line = rope.slice_line(row).to_string();
        let l = line.trim();
        if !l.is_empty() && !l.starts_with("--") {
            return l.chars().take(48).collect();
        }
    }
    String::new()
}
