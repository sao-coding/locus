//! Header of the main card (48 px): breadcrumb (engine badge, connection
//! picker, database, tab), connection state, Run split button, Save and the
//! right-panel toggle.
use super::*;
use gpui_kit::component::Disableable as _;

impl Locus {
    pub(crate) fn render_card_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let tab = self.active_tab(cx);
        if let Some(page) = tab.read(cx).pageslot.kind {
            if matches!(
                page,
                locus_core::spaces::TabPage::Redis | locus_core::spaces::TabPage::Activity
            ) {
                return self.render_redis_header(&tab, cx);
            }
            return self.render_page_header(page, cx);
        }
        if tab.read(cx).diagram.is_some() {
            return self.render_diagram_header(cx);
        }
        if tab.read(cx).structure.is_some() {
            return self.render_structure_header(cx);
        }
        let tr = tab.read(cx);
        // Stop only while rows are fetched or the server executes (`runview`); a
        // paused query offers Load more / Load all / Release in the result footer.
        let rv = tr.status.read(cx).view();
        let busy = rv.button != locus_core::runview::Button::Run || tr.ap.applying;
        let stopping = rv.button == locus_core::runview::Button::Stopping;
        let modified = tr.doc.modified;
        let title = tr.display_title(cx);
        let kind = tab_kind(tr);
        let is_diagram = tr.diagram.is_some();
        let conn = self.active_conn(cx).cloned();
        let col = self.space().accent(cx);
        let crumb = self.header_crumb(&tab, conn.as_ref(), (title, kind, col, modified), cx);

        let chip = conn.as_ref().map(|c| self.render_mode_chip(c, cx));
        let tx_controls = conn.as_ref().map(|c| self.render_tx_controls(&tab, c, cx));

        // Connection state.
        let state = conn.as_ref().map(|c| {
            let (s, label) = self.conn_state(&c.id, cx);
            h_flex()
                .min_w_0()
                .overflow_hidden()
                .flex_shrink(8.)
                .gap(px(scale::S3))
                .items_center()
                .text_size(px(scale::TX_BASE))
                .text_color(t.tx3)
                .whitespace_nowrap()
                .child(
                    div()
                        .flex_none()
                        .size(px(scale::S4 - 1.))
                        .rounded_full()
                        .bg(crate::shell::theme::state_color(s, cx)),
                )
                .child(div().min_w_0().truncate().child(label))
        });

        // Run split button (Stop while a query runs or is paused).
        let prod = conn
            .as_ref()
            .is_some_and(|c| c.env == locus_core::store::Env::Prod);
        let can_run = conn.is_some() && !is_diagram;
        // Explain needs an engine with a plan statement (Oracle: hidden for now).
        let can_explain = can_run && self.explain_engine(cx).is_some();
        let run: AnyElement = if busy {
            self.header_stop_button(&tab, stopping, cx)
        } else {
            self.header_run_split(&tab, prod, can_run, can_explain, cx)
        };

        h_flex()
            .id("card-header")
            .flex_none()
            .h(px(scale::H_TAB + scale::S2))
            .pl(px(scale::S6))
            .pr(px(scale::S5))
            .gap(px(scale::S5))
            .items_center()
            .border_b_1()
            .border_color(if prod { t.red } else { t.ln })
            .child(self.sidebar_toggle(cx))
            .child(crumb)
            .children(chip)
            .children(tx_controls)
            .children(state)
            .child(div().flex_none().w(px(1.)).h(px(20.)).bg(t.on))
            .child(self.header_actions(run, &tab, can_explain, cx))
            .into_any_element()
    }

    /// Breadcrumb: engine badge, connection picker, database, tab title and the unsaved marker.
    fn header_crumb(
        &self,
        tab: &Entity<QueryTab>,
        conn: Option<&SavedConnection>,
        (title, kind, col, modified): (String, crate::shell::theme::TabKind, Hsla, bool),
        cx: &mut Context<Self>,
    ) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let conn = conn.cloned();
        let chevron = || {
            crate::shell::icons::icon("chevron-right")
                .size(px(10.))
                .text_color(t.tx4)
                .flex_none()
        };

        // Connection picker: the Space's connections.
        let me = cx.entity().downgrade();
        let items: Vec<(String, String)> = self
            .space()
            .connections
            .iter()
            .filter_map(|id| self.conns.iter().find(|c| &c.id == id))
            .map(|c| (c.id.clone(), c.name.clone()))
            .collect();
        let current = conn.as_ref().map(|c| c.id.clone());
        let tab2 = tab.clone();
        let picker = Button::new("conn-picker")
            .ghost()
            .small()
            .label(
                conn.as_ref()
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| t!("toolbar.pick_connection").into_owned()),
            )
            .tooltip(t!("toolbar.pick_connection_hint").to_string())
            .dropdown_menu(move |mut menu, _, _| {
                if items.is_empty() {
                    menu = menu.item(PopupMenuItem::label(
                        t!("toolbar.no_space_connections").to_string(),
                    ));
                }
                for (id, name) in items.iter().cloned() {
                    let me = me.clone();
                    let tab = tab2.clone();
                    let checked = current.as_deref() == Some(id.as_str());
                    menu = menu.item(PopupMenuItem::new(name).checked(checked).on_click(
                        move |_, _, cx| {
                            let _ = me.update(cx, |this, cx| {
                                this.set_tab_connection(tab.clone(), Some(id.clone()), cx)
                            });
                        },
                    ));
                }
                menu
            });

        // Priority when the window is narrow: connection and database keep a
        // readable minimum width; the tab title (already in the tab strip)
        // gives way first and can shrink to nothing.
        let database = conn
            .as_ref()
            .and_then(|c| c.database.clone())
            .unwrap_or_else(|| "—".into());
        let mut source = h_flex()
            .min_w(px(96.))
            .flex_shrink(1.)
            .overflow_hidden()
            .gap(px(scale::S2))
            .items_center();
        if let Some(c) = &conn {
            source = source.child(self.conn_badge(c, cx));
        }
        source = source.child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_size(px(scale::TX_BASE))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.tx1)
                .child(picker),
        );
        h_flex()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .gap(px(scale::S3))
            .items_center()
            .child(source)
            .child(chevron())
            .child(
                h_flex()
                    .id("crumb-db")
                    .min_w(px(72.))
                    .flex_shrink(1.)
                    .overflow_hidden()
                    .gap(px(scale::S3))
                    .items_center()
                    .text_size(px(scale::TX_BASE))
                    .text_color(t.tx2)
                    .tooltip(tip(database.clone()))
                    .child(crate::shell::icons::icon("db").size(px(14.)).flex_none())
                    .child(div().min_w_0().truncate().child(database)),
            )
            .child(chevron())
            .child(
                h_flex()
                    .id("crumb-title")
                    .min_w_0()
                    .flex_shrink(20.)
                    .overflow_hidden()
                    .gap(px(scale::S3))
                    .items_center()
                    .tooltip(tip(title.clone()))
                    .child(kind_icon(kind).size(px(14.)).text_color(col).flex_none())
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(scale::TX_BASE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.tx1)
                            .child(title),
                    ),
            )
            // Unsaved: just the dot; the words live in its tooltip.
            .when(modified, |c| {
                c.child(
                    div()
                        .id("crumb-unsaved")
                        .flex_none()
                        .size(px(16.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .tooltip(tip(t!("shell.unsaved").into_owned()))
                        .child(div().size(px(7.)).rounded_full().bg(t.acc)),
                )
            })
    }

    /// The red Stop button shown while a query runs.
    fn header_stop_button(
        &self,
        tab: &Entity<QueryTab>,
        stopping: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let tab = tab.clone();
        h_flex()
            .id("stop")
            .flex_none()
            .h(px(scale::H_CONTROL))
            .px(px(scale::S5))
            .gap(px(scale::S3))
            .items_center()
            .rounded(px(scale::R_MD))
            .border_1()
            .border_color(t.red)
            .text_color(t.red_text)
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(scale::TX_BASE))
            .cursor_pointer()
            .hover(|s| s.bg(t.hv))
            .tooltip(tip(t!("toolbar.cancel_hint").into_owned()))
            .child(crate::shell::style::spinner(t.red_text))
            .child(if stopping {
                t!("runview.status_cancelling").into_owned()
            } else {
                t!("shell.stop").into_owned()
            })
            .child(
                div()
                    .opacity(0.6)
                    .child(kbd("Esc", cx))
                    .text_color(t.red_text),
            )
            .on_click(cx.listener(move |_, _, _, cx| tab.update(cx, |t, cx| t.cancel(cx))))
            .into_any_element()
    }

    /// Run split button with its options menu.
    fn header_run_split(
        &self,
        tab: &Entity<QueryTab>,
        prod: bool,
        can_run: bool,
        can_explain: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let tab_a = tab.clone();
        let me = cx.entity().downgrade();
        let tab_m = tab.clone();
        let caret = Button::new("run-menu")
            .ghost()
            .xsmall()
            .icon(
                crate::shell::icons::icon("caret-down")
                    .size(px(11.))
                    .text_color(if prod { t.red_text } else { t.acc_fg }),
            )
            .tooltip(t!("shell.run_options").to_string())
            .disabled(!can_run)
            .dropdown_menu(move |menu, _, _| {
                let (m1, m2, t1, t2) = (me.clone(), me.clone(), tab_m.clone(), tab_m.clone());
                let (m3, m4) = (me.clone(), me.clone());
                menu.item(crate::shell::menu::row(
                    t!("shell.run_current").to_string(),
                    Some("play"),
                    Some("Ctrl+Enter"),
                    false,
                    move |window, cx| {
                        let _ = m1.update(cx, |this, cx| {
                            this.run_tab(t1.clone(), RunMode::Current, None, window, cx)
                        });
                    },
                ))
                .item(crate::shell::menu::row(
                    t!("shell.run_all").to_string(),
                    Some("script"),
                    Some("Ctrl+Shift+Enter"),
                    false,
                    move |window, cx| {
                        let _ = m2.update(cx, |this, cx| {
                            this.run_tab(t2.clone(), RunMode::Script, None, window, cx)
                        });
                    },
                ))
                .when(can_explain, |m| {
                    m.separator()
                        .item(crate::shell::menu::row(
                            t!("explain.menu").to_string(),
                            Some("explain"),
                            Some("Ctrl+E"),
                            false,
                            move |window, cx| {
                                let _ = m3.update(cx, |this, cx| this.explain(false, window, cx));
                            },
                        ))
                        .item(crate::shell::menu::item(
                            t!("explain.menu_analyze").to_string(),
                            Some("explain"),
                            move |window, cx| {
                                let _ = m4.update(cx, |this, cx| this.explain(true, window, cx));
                            },
                        ))
                })
            });
        // Production: outlined, no accent fill (the mockup's `.run.prod`).
        let fg = if prod { t.red_text } else { t.acc_fg };
        h_flex()
            .flex_none()
            .h(px(scale::H_CONTROL))
            .rounded(px(scale::R_MD))
            .overflow_hidden()
            .when(!prod, |d| d.bg(t.acc))
            .when(prod, |d| d.border_1().border_color(t.red))
            .text_color(fg)
            .when(!can_run, |d| d.opacity(0.5))
            .child(
                h_flex()
                    .id("run")
                    .h_full()
                    .px(px(scale::S5))
                    .gap(px(scale::S3))
                    .items_center()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(scale::TX_BASE))
                    .cursor_pointer()
                    .hover(move |s| {
                        s.bg(if prod {
                            t.hv
                        } else {
                            mix(t.acc, gpui_kit::white(), 0.08)
                        })
                    })
                    .tooltip(tip(t!("toolbar.run_hint").into_owned()))
                    .child(crate::shell::icons::icon("play").size(px(12.)))
                    .child(t!("toolbar.run").into_owned())
                    .child(
                        div()
                            .opacity(0.55)
                            .text_size(px(scale::TX_XS))
                            .font_family(cx.theme().mono_font_family.clone())
                            .child("Ctrl+Enter"),
                    )
                    .when(can_run, |d| {
                        d.on_click(cx.listener(move |this, _, window, cx| {
                            this.run_tab(tab_a.clone(), RunMode::Current, None, window, cx)
                        }))
                    }),
            )
            .child(div().h_full().w(px(1.)).bg(if prod {
                t.red
            } else {
                hsla(0.07, 0.24, 0.08, 0.25)
            }))
            .child(
                div()
                    .w(px(scale::H_COMPACT))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(caret),
            )
            .into_any_element()
    }

    /// Run, Explain, Format, Find, Save and the right-panel toggle.
    fn header_actions(
        &self,
        run: AnyElement,
        tab: &Entity<QueryTab>,
        can_explain: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let tab_s = tab.clone();
        h_flex()
            .flex_none()
            .gap(px(scale::S3))
            .items_center()
            .child(run)
            .when(can_explain, |h| {
                h.child(
                    h_flex()
                        .id("explain")
                        .flex_none()
                        .h(px(scale::H_CONTROL))
                        .px(px(scale::S4))
                        .gap(px(scale::S3))
                        .items_center()
                        .rounded(px(scale::R_MD))
                        .border_1()
                        .border_color(t.ln2)
                        .text_size(px(scale::TX_BASE))
                        .text_color(t.tx2)
                        .cursor_pointer()
                        .hover(|s| s.bg(t.hv))
                        .tooltip(tip(t!("explain.tooltip").into_owned()))
                        .child(crate::shell::icons::icon("explain").size(px(14.)))
                        .child(t!("explain.button").into_owned())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.explain(false, window, cx)),
                        ),
                )
            })
            .child(div().flex_none().w(px(1.)).h(px(16.)).mx(px(2.)).bg(t.on))
            .child(
                ib(
                    "format",
                    crate::shell::icons::icon("format"),
                    scale::H_CONTROL,
                    false,
                    cx,
                )
                .tooltip(tip(t!("format.tooltip").into_owned()))
                .on_click(cx.listener(|this, _, window, cx| this.format_sql(window, cx))),
            )
            .child(
                ib(
                    "find",
                    crate::shell::icons::icon("search"),
                    scale::H_CONTROL,
                    false,
                    cx,
                )
                .tooltip(tip(t!("find.tooltip").into_owned()))
                .on_click(cx.listener(|this, _, window, cx| this.find_replace(false, window, cx))),
            )
            .child(
                ib(
                    "save",
                    crate::shell::icons::icon("save"),
                    scale::H_CONTROL,
                    false,
                    cx,
                )
                .tooltip(tip(t!("shell.save").into_owned()))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.save_tab(tab_s.clone(), false, false, window, cx)
                })),
            )
            .child(
                ib(
                    "panel-toggle",
                    crate::shell::icons::icon("panel-right"),
                    scale::H_CONTROL,
                    self.right_panel.is_some(),
                    cx,
                )
                .tooltip(tip(t!("shell.right_panel").into_owned()))
                .on_click(cx.listener(|this, _, _, cx| {
                    let p = if this.right_panel.is_some() {
                        None
                    } else {
                        Some(RightPanel::Schema)
                    };
                    this.set_right_panel(p, cx);
                })),
            )
    }
}
