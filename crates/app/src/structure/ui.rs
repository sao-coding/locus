//! Drawing of a Structure tab: the section switch, the tables of each
//! section, the DDL editor, and the card header / footer.
use super::*;
use crate::shell::scale::*;
use crate::shell::seg::{SegItem, seg};
use db_core::structure::{StructForeignKey, TableStructure};
use gpui_kit::component::input::Editor;
use std::rc::Rc;

const SECTIONS: [(Section, &str); 6] = [
    (Section::Columns, "structure.columns"),
    (Section::Indexes, "structure.indexes"),
    (Section::ForeignKeys, "structure.foreign_keys"),
    (Section::Triggers, "structure.triggers"),
    (Section::Checks, "structure.checks"),
    (Section::Ddl, "structure.ddl"),
];

fn count(st: &TableStructure, s: Section) -> Option<usize> {
    Some(match s {
        Section::Columns => st.columns.len(),
        Section::Indexes => st.indexes.len(),
        Section::ForeignKeys => st.foreign_keys.len() + st.referenced_by.len(),
        Section::Triggers => st.triggers.len(),
        Section::Checks => st.checks.len(),
        Section::Ddl => return None,
    })
}

/// One table cell: fixed width, or the rest of the row when `w` is `None`.
fn cell(text: impl Into<SharedString>, w: Option<f32>, color: Hsla, mono: bool) -> Div {
    let d = div()
        .min_w_0()
        .truncate()
        .px(px(S3))
        .text_color(color)
        .child(text.into());
    let d = if mono { d.font_family("monospace") } else { d };
    match w {
        Some(w) => d.w(px(w)).flex_none(),
        None => d.flex_1(),
    }
}

fn head_row(cols: &[(String, Option<f32>)], cx: &App) -> Div {
    let t = crate::shell::theme::tokens(cx);
    let mut r = h_flex()
        .flex_none()
        .h(px(H_ROW))
        .items_center()
        .border_b_1()
        .border_color(t.ln)
        .bg(t.hv)
        .text_size(px(TX_SM))
        .font_weight(FontWeight::MEDIUM);
    for (label, w) in cols {
        r = r.child(cell(label.clone(), *w, t.tx2, false));
    }
    r
}

fn body_row(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    let t = crate::shell::theme::tokens(cx);
    h_flex()
        .id(id)
        .flex_none()
        .h(px(H_ROW))
        .items_center()
        .border_b_1()
        .border_color(t.ln)
        .text_size(px(TX_SM))
        .text_color(t.tx1)
}

fn scroll(id: &'static str, rows: Vec<AnyElement>) -> AnyElement {
    div()
        .id(id)
        .size_full()
        .overflow_y_scrollbar()
        .child(v_flex().w_full().children(rows))
        .into_any_element()
}

fn flag(on: bool, cx: &App) -> String {
    let _ = cx;
    if on { "✓".into() } else { String::new() }
}

impl QueryTab {
    fn structure_empty(&self, text: String, cx: &App) -> AnyElement {
        crate::query::tab::empty(text, cx)
    }

    pub(crate) fn render_structure(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let Some(s) = self.structure.as_ref() else {
            return div().into_any_element();
        };
        let section = s.section;
        let weak = cx.entity().downgrade();
        let ready = if let Load::Ready(d) = &s.data {
            Some(d.clone())
        } else {
            None
        };
        let items: Vec<SegItem> = SECTIONS
            .iter()
            .map(|(sec, key)| {
                let mut label = t!(*key).into_owned();
                if let Some(n) = ready.as_ref().and_then(|d| count(d, *sec)) {
                    label = format!("{label} {n}");
                }
                let weak = weak.clone();
                let sec = *sec;
                SegItem::new(
                    SharedString::from(format!("st-{key}")),
                    label,
                    sec == section,
                    Rc::new(move |_, _, cx| {
                        let _ = weak.update(cx, |tab, cx| {
                            if let Some(s) = tab.structure.as_mut() {
                                s.section = sec;
                                super::remember_section(&s.r, sec);
                            }
                            cx.notify();
                        });
                    }),
                )
            })
            .collect();
        let body = match (&s.data, section) {
            (Load::Loading, _) => self.structure_empty(t!("structure.loading").into_owned(), cx),
            (Load::Failed(e), _) => {
                self.structure_empty(t!("structure.failed", msg = e.clone()).into_owned(), cx)
            }
            (Load::Ready(d), Section::Ddl) => self.render_ddl(d, cx),
            (Load::Ready(d), sec) => {
                let d = d.clone();
                match sec {
                    Section::Columns => self.sec_columns(&d, cx),
                    Section::Indexes => self.sec_indexes(&d, cx),
                    Section::ForeignKeys => self.sec_fks(&d, cx),
                    Section::Triggers => self.sec_triggers(&d, cx),
                    _ => self.sec_checks(&d, cx),
                }
            }
        };
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .flex_none()
                    .px(px(S5))
                    .py(px(S3))
                    .border_b_1()
                    .border_color(t.ln)
                    .child(seg(items, cx)),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    fn sec_columns(&self, d: &TableStructure, cx: &App) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        if d.columns.is_empty() {
            return self.structure_empty(t!("structure.empty_columns").into_owned(), cx);
        }
        let cols = [
            (t!("structure.col_name").into_owned(), Some(200.)),
            (t!("structure.col_type").into_owned(), Some(180.)),
            (t!("structure.col_nullable").into_owned(), Some(70.)),
            (t!("structure.col_default").into_owned(), Some(180.)),
            (t!("structure.col_pk").into_owned(), Some(50.)),
            (t!("structure.col_auto").into_owned(), Some(90.)),
            (t!("structure.col_comment").into_owned(), None),
        ];
        let mut rows = vec![head_row(&cols, cx).into_any_element()];
        for (i, c) in d.columns.iter().enumerate() {
            rows.push(
                body_row(("st-col", i), cx)
                    .child(cell(c.name.clone(), Some(200.), t.tx1, false).font_weight(
                        if c.primary_key {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        },
                    ))
                    .child(cell(c.type_name.clone(), Some(180.), t.tx2, true))
                    .child(cell(flag(c.nullable, cx), Some(70.), t.tx2, false))
                    .child(cell(
                        c.default.clone().unwrap_or_default(),
                        Some(180.),
                        t.tx2,
                        true,
                    ))
                    .child(cell(flag(c.primary_key, cx), Some(50.), t.tx2, false))
                    .child(cell(flag(c.auto_increment, cx), Some(90.), t.tx2, false))
                    .child(cell(
                        c.comment.clone().unwrap_or_default(),
                        None,
                        t.tx3,
                        false,
                    ))
                    .into_any_element(),
            );
        }
        scroll("st-columns", rows)
    }

    fn sec_indexes(&self, d: &TableStructure, cx: &App) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        if d.indexes.is_empty() {
            return self.structure_empty(t!("structure.empty_indexes").into_owned(), cx);
        }
        let cols = [
            (t!("structure.col_name").into_owned(), Some(240.)),
            (t!("structure.col_columns").into_owned(), Some(360.)),
            (t!("structure.col_unique").into_owned(), Some(70.)),
            (t!("structure.col_pk").into_owned(), Some(50.)),
            (t!("structure.col_type").into_owned(), None),
        ];
        let mut rows = vec![head_row(&cols, cx).into_any_element()];
        for (i, x) in d.indexes.iter().enumerate() {
            rows.push(
                body_row(("st-idx", i), cx)
                    .child(cell(x.name.clone(), Some(240.), t.tx1, false))
                    .child(cell(x.columns.join(", "), Some(360.), t.tx2, true))
                    .child(cell(flag(x.unique, cx), Some(70.), t.tx2, false))
                    .child(cell(flag(x.primary, cx), Some(50.), t.tx2, false))
                    .child(cell(x.kind.clone(), None, t.tx3, false))
                    .into_any_element(),
            );
        }
        scroll("st-indexes", rows)
    }

    fn fk_rows(
        &self,
        list: &[StructForeignKey],
        incoming: bool,
        base: usize,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = crate::shell::theme::tokens(cx);
        let Some(conn_id) = self.structure.as_ref().map(|s| s.r.conn_id.clone()) else {
            return Vec::new();
        };
        list.iter()
            .enumerate()
            .map(|(i, k)| {
                // The table to open on click: the other end of the key.
                let (schema, table) = if incoming {
                    (k.schema.clone(), k.table.clone())
                } else {
                    (k.ref_schema.clone(), k.ref_table.clone())
                };
                let (local, remote) = if incoming {
                    (
                        format!("{}.{}({})", k.schema, k.table, k.columns.join(", ")),
                        k.ref_columns.join(", "),
                    )
                } else {
                    (
                        k.columns.join(", "),
                        format!(
                            "{}.{}({})",
                            k.ref_schema,
                            k.ref_table,
                            k.ref_columns.join(", ")
                        ),
                    )
                };
                let conn = conn_id.clone();
                body_row(("st-fk", base + i), cx)
                    .cursor_pointer()
                    .hover(|h| h.bg(t.hv))
                    .child(cell(k.name.clone(), Some(220.), t.tx1, false))
                    .child(cell(local, Some(320.), t.tx2, true))
                    .child(cell(remote, Some(320.), t.acc_text, true))
                    .child(cell(k.on_delete.clone(), Some(110.), t.tx2, false))
                    .child(cell(k.on_update.clone(), None, t.tx2, false))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(TabEvent::ShowStructure {
                            conn_id: conn.clone(),
                            schema: schema.clone(),
                            table: table.clone(),
                        });
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn sec_fks(&self, d: &TableStructure, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        if d.foreign_keys.is_empty() && d.referenced_by.is_empty() {
            return self.structure_empty(t!("structure.empty_fks").into_owned(), cx);
        }
        let cols = [
            (t!("structure.col_name").into_owned(), Some(220.)),
            (t!("structure.col_columns").into_owned(), Some(320.)),
            (t!("structure.col_references").into_owned(), Some(320.)),
            (t!("structure.col_on_delete").into_owned(), Some(110.)),
            (t!("structure.col_on_update").into_owned(), None),
        ];
        let title = |text: String| {
            div()
                .flex_none()
                .h(px(H_ROW))
                .px(px(S5))
                .flex()
                .items_center()
                .text_size(px(TX_SM))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.tx2)
                .child(text)
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(
            title(
                t!(
                    "structure.fk_outgoing",
                    n = d.foreign_keys.len().to_string()
                )
                .into_owned(),
            )
            .into_any_element(),
        );
        rows.push(head_row(&cols, cx).into_any_element());
        rows.extend(self.fk_rows(&d.foreign_keys, false, 0, cx));
        rows.push(
            title(
                t!(
                    "structure.fk_incoming",
                    n = d.referenced_by.len().to_string()
                )
                .into_owned(),
            )
            .into_any_element(),
        );
        let mut incoming_cols = cols.clone();
        incoming_cols[1].0 = t!("structure.col_from").into_owned();
        incoming_cols[2].0 = t!("structure.col_columns").into_owned();
        rows.push(head_row(&incoming_cols, cx).into_any_element());
        rows.extend(self.fk_rows(&d.referenced_by, true, 10_000, cx));
        scroll("st-fks", rows)
    }

    fn sec_triggers(&self, d: &TableStructure, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        if d.triggers.is_empty() {
            return self.structure_empty(t!("structure.empty_triggers").into_owned(), cx);
        }
        let Some(r) = self.structure.as_ref().map(|s| s.r.clone()) else {
            return div().into_any_element();
        };
        let cols = [
            (t!("structure.col_name").into_owned(), Some(300.)),
            (t!("structure.col_event").into_owned(), None),
        ];
        let mut rows = vec![head_row(&cols, cx).into_any_element()];
        for (i, x) in d.triggers.iter().enumerate() {
            let (conn, schema, name) = (r.conn_id.clone(), r.schema.clone(), x.name.clone());
            rows.push(
                body_row(("st-trg", i), cx)
                    .cursor_pointer()
                    .hover(|h| h.bg(t.hv))
                    .child(cell(x.name.clone(), Some(300.), t.acc_text, false))
                    .child(cell(
                        x.detail.clone().unwrap_or_default(),
                        None,
                        t.tx2,
                        false,
                    ))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(TabEvent::OpenTrigger {
                            conn_id: conn.clone(),
                            schema: schema.clone(),
                            name: name.clone(),
                        });
                    }))
                    .into_any_element(),
            );
        }
        scroll("st-triggers", rows)
    }

    fn sec_checks(&self, d: &TableStructure, cx: &App) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        if d.checks.is_empty() {
            return self.structure_empty(t!("structure.empty_checks").into_owned(), cx);
        }
        let cols = [
            (t!("structure.col_name").into_owned(), Some(260.)),
            (t!("structure.col_expression").into_owned(), None),
        ];
        let mut rows = vec![head_row(&cols, cx).into_any_element()];
        for (i, x) in d.checks.iter().enumerate() {
            rows.push(
                body_row(("st-chk", i), cx)
                    .child(cell(x.name.clone(), Some(260.), t.tx1, false))
                    .child(cell(x.expression.clone(), None, t.tx2, true))
                    .into_any_element(),
            );
        }
        scroll("st-checks", rows)
    }

    fn render_ddl(&self, _d: &TableStructure, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let Some(s) = self.structure.as_ref() else {
            return div().into_any_element();
        };
        match &s.ddl {
            Load::Failed(e) => {
                self.structure_empty(t!("structure.ddl_failed", msg = e.clone()).into_owned(), cx)
            }
            Load::Loading => self.structure_empty(t!("structure.loading").into_owned(), cx),
            Load::Ready(text) => {
                let text = text.clone();
                let editor = self.editor.read(cx).state.clone();
                v_flex()
                    .size_full()
                    .child(
                        h_flex()
                            .flex_none()
                            .px(px(S5))
                            .py(px(S2))
                            .gap(px(S4))
                            .items_center()
                            .border_b_1()
                            .border_color(t.ln)
                            .child(div().flex_1())
                            .child(
                                Button::new("st-copy-ddl")
                                    .ghost()
                                    .xsmall()
                                    .label(t!("structure.copy_ddl").to_string())
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            text.clone(),
                                        ));
                                        crate::shell::toast::show(
                                            t!("structure.copied").to_string(),
                                            cx,
                                        );
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .child(Editor::new(&editor).bordered(false).h_full()),
                    )
                    .into_any_element()
            }
        }
    }
}

// ---- card header and footer -------------------------------------------------

impl Locus {
    pub(crate) fn render_structure_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let tab = self.active_tab(cx);
        let tr = tab.read(cx);
        let conn = self.active_conn(cx).cloned();
        let col = space_color(self.space().hue(), cx);
        let chevron = || {
            crate::shell::icons::icon("chevron-right")
                .size(px(10.))
                .text_color(t.tx5)
        };
        let schema = tr
            .structure
            .as_ref()
            .map(|s| s.r.schema.clone())
            .unwrap_or_default();
        let mut crumb = h_flex().flex_1().min_w_0().gap(px(S4)).items_center();
        if let Some(c) = &conn {
            crumb = crumb
                .child(self.conn_badge(c, cx))
                .child(div().flex_none().text_color(t.tx3).child(c.name.clone()))
                .child(chevron());
        }
        crumb = crumb
            .child(div().flex_none().text_color(t.tx3).child(schema))
            .child(chevron())
            .child(
                h_flex()
                    .min_w_0()
                    .gap(px(S3))
                    .items_center()
                    .child(kind_icon(tab_kind(tr)).size(px(14.)).text_color(col))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(TX_MD))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.tx1)
                            .child(tr.display_title(cx)),
                    ),
            );
        let tab2 = tab.clone();
        h_flex()
            .id("card-header")
            .flex_none()
            .h(px(H_TAB + S2))
            .pl(px(S6))
            .pr(px(S5))
            .gap(px(S5))
            .items_center()
            .border_b_1()
            .border_color(t.ln)
            .text_size(px(TX_SM))
            .child(self.sidebar_toggle(cx))
            .child(crumb)
            // ALTER would sit here, left of Refresh.
            .child(
                Button::new("st-refresh")
                    .ghost()
                    .small()
                    .label(t!("structure.refresh").to_string())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.load_structure(tab2.clone(), true, window, cx)
                    })),
            )
            .into_any_element()
    }

    pub(crate) fn render_structure_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let conn = self.active_conn(cx).cloned();
        let left = match &conn {
            Some(c) => {
                let (st, label) = self.conn_state(&c.id, cx);
                h_flex()
                    .gap(px(S6))
                    .items_center()
                    .child(
                        h_flex()
                            .gap(px(S3))
                            .items_center()
                            .text_color(t.tx2)
                            .child(
                                div()
                                    .size(px(7.))
                                    .rounded_full()
                                    .bg(crate::shell::theme::state_color(st, cx)),
                            )
                            .child(label),
                    )
                    .children(self.conn_version(&c.id))
            }
            None => h_flex().child("—"),
        };
        h_flex()
            .id("status-bar")
            .flex_none()
            .h(px(H_BAR))
            .px(px(S5))
            .gap(px(S6))
            .items_center()
            .border_t_1()
            .border_color(t.ln)
            .bg(t.foot)
            .text_size(px(TX_XS))
            .text_color(t.tx3)
            .whitespace_nowrap()
            .child(left)
            .child(div().flex_1())
            .child(t!("structure.read_only").into_owned())
            .into_any_element()
    }
}
