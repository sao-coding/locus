//! Status bar of the main card (H_BAR): connection state, server version,
//! database | Ln/Col, UTF-8, last run, rows, row limit, unsaved tabs of the Space.
use super::*;

impl Locus {
    pub(crate) fn render_status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let tab = self.active_tab(cx);
        if tab.read(cx).diagram.is_some() {
            return self.render_diagram_footer(cx);
        }
        if tab.read(cx).structure.is_some() {
            return self.render_structure_footer(cx);
        }
        let conn = self.active_conn(cx).cloned();
        let left = self.status_connection(conn.as_ref(), cx);
        let tr = tab.read(cx);
        let mono = cx.theme().mono_font_family.clone();
        let s = self.space();
        let unsaved = s.tabs.iter().filter(|x| x.read(cx).doc.modified).count();

        // Row limit of the tab in front.
        let limit = row_limit_button(&tab, tr.limit);

        // S8: caret, encoding, status and row limit only mean something for an editor tab with a connection.
        let editor_info = conn.is_some() && tr.pageslot.kind.is_none();
        let col = s.accent(cx);
        h_flex()
            .id("status-bar")
            .flex_none()
            .h(px(scale::H_BAR))
            .px(px(scale::S5))
            .gap(px(scale::S6))
            .items_center()
            .border_t_1()
            .border_color(t.ln)
            .bg(t.foot)
            .text_size(px(scale::TX_XS))
            .text_color(t.tx3)
            .whitespace_nowrap()
            .overflow_hidden()
            .child(left)
            .child(div().flex_1())
            .when(editor_info, |h| {
                h.child(
                    div()
                        .text_size(px(scale::TX_XS))
                        .font_family(mono.clone())
                        .child(tr.caret.clone()),
                )
            })
            .when(editor_info, |h| {
                h.child(
                    div()
                        .text_size(px(scale::TX_XS))
                        .font_family(mono)
                        .child("UTF-8"),
                )
            })
            .when(editor_info, |h| h.child(tr.status.clone()))
            .when(editor_info, |h| {
                h.child(div().text_color(t.tx3).child(limit))
            })
            .children(self.update_hint(col, cx))
            .child(
                h_flex()
                    .gap(px(scale::S3))
                    .items_center()
                    .child(div().size(px(6.)).rounded_full().bg(col))
                    .child(
                        t!(
                            "shell.space_unsaved",
                            space = s.name.clone(),
                            n = unsaved.to_string()
                        )
                        .into_owned(),
                    ),
            )
            .into_any_element()
    }

    /// "Locus x.y.z available" when the update check found a newer release that was not
    /// dismissed; opens Settings › About.
    fn update_hint(&self, accent: Hsla, cx: &mut Context<Self>) -> Option<AnyElement> {
        let r = crate::update::entity(cx)?.read(cx).hint()?.clone();
        let t = crate::shell::theme::tokens(cx);
        Some(
            h_flex()
                .id("status-update")
                .gap(px(scale::S3))
                .items_center()
                .cursor_pointer()
                .text_color(t.tx2)
                .hover(|h| h.text_color(t.tx))
                .tooltip(tip(t!("upd.hint_tip").into_owned()))
                .child(
                    crate::shell::icons::icon("download")
                        .size(px(12.))
                        .text_color(accent),
                )
                .child(t!("upd.hint", v = r.version).into_owned())
                .on_click(cx.listener(|this, _, window, cx| this.open_settings_about(window, cx)))
                .into_any_element(),
        )
    }

    /// Connection state, server version (opens the activity monitor) and the database or Redis keys.
    fn status_connection(&self, conn: Option<&SavedConnection>, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        match conn {
            Some(c) => {
                let (st, label) = self.conn_state(&c.id, cx);
                h_flex()
                    .gap(px(scale::S6))
                    .items_center()
                    .child(
                        h_flex()
                            .gap(px(scale::S3))
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
                    .children(self.conn_version(&c.id).map(|v| {
                        let cid = c.id.clone();
                        let can = locus_core::activity::Engine::of(c.kind).is_some();
                        div()
                            .id("status-version")
                            .when(can, |d| {
                                d.cursor_pointer()
                                    .hover(|h| h.text_color(t.tx))
                                    .tooltip(tip(t!("act.menu").into_owned()))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_activity(cid.clone(), window, cx)
                                    }))
                            })
                            .child(v)
                    }))
                    .children(
                        match crate::redis::health(&c.id)
                            .filter(|_| c.kind == locus_core::store::DbKind::Redis)
                        {
                            Some(h) => Some(match h.keys {
                                Some(n) => t!(
                                    "redis2.status_keys",
                                    db = h.db,
                                    n = locus_core::fmt::count(n)
                                )
                                .into_owned(),
                                None => format!("DB {}", h.db),
                            }),
                            None => c.database.clone(),
                        },
                    )
            }
            None => h_flex().child("—"),
        }
    }
}

/// Row limit menu of the status bar.
fn row_limit_button(tab: &Entity<QueryTab>, cur: Option<u64>) -> impl IntoElement {
    let limit_label = match cur {
        Some(n) => t!("limit.rows", n = locus_core::fmt::count(n)).into_owned(),
        None => t!("limit.unlimited").into_owned(),
    };
    let tab_l = tab.downgrade();
    Button::new("row-limit")
        .ghost()
        .xsmall()
        .label(limit_label)
        .tooltip(locus_core::runview::limit_hint(cur, &rust_i18n::locale()))
        .dropdown_menu(move |mut menu, _, _| {
            for v in [
                Some(1_000u64),
                Some(10_000),
                Some(100_000),
                Some(1_000_000),
                None,
            ] {
                let tab = tab_l.clone();
                let label = match v {
                    Some(n) => t!("limit.rows", n = locus_core::fmt::count(n)).into_owned(),
                    None => t!("limit.unlimited").into_owned(),
                };
                menu = menu.item(PopupMenuItem::new(label).checked(cur == v).on_click(
                    move |_, _, cx| {
                        let _ = tab.update(cx, |t, cx| t.set_limit(v, cx));
                    },
                ));
            }
            menu
        })
}
