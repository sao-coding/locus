//! Card header and footer of a diagram tab (Strata v3 `erHeader` / `erFoot`):
//! breadcrumb with the scope, depth, "show whole schema", zoom, auto arrange,
//! export; footer with connection state, version, counts and a hint.
use crate::shell::scale::*;
use crate::shell::*;
use er_diagram::{Density, Depth, EdgeStyle};
use gpui_kit::component::Disableable as _;

impl Locus {
    pub(crate) fn render_diagram_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let tab = self.active_tab(cx);
        let conn = self.active_conn(cx).cloned();
        let crumb = self.diagram_crumb(&tab, conn.as_ref(), cx);
        let tools = self.diagram_tools(&tab, cx);
        let prod = conn
            .as_ref()
            .is_some_and(|c| c.env == locus_core::store::Env::Prod);
        h_flex()
            .id("card-header")
            .flex_none()
            .h(px(H_TAB + S2))
            .pl(px(S6))
            .pr(px(S5))
            .gap(px(S5))
            .items_center()
            .border_b_1()
            .border_color(if prod { t.red } else { t.ln })
            .child(self.sidebar_toggle(cx))
            .child(crumb)
            .child(tools)
            .into_any_element()
    }

    /// Breadcrumb: connection, schema, tab title and the scope text.
    fn diagram_crumb(
        &self,
        tab: &Entity<QueryTab>,
        conn: Option<&SavedConnection>,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let tr = tab.read(cx);
        let col = self.space().accent(cx);
        let chevron = || {
            crate::shell::icons::icon("chevron-right")
                .size(px(10.))
                .text_color(t.tx5)
        };
        let schema = tr.diagram.as_ref().map(|d| d.1.clone()).unwrap_or_default();
        let mut crumb = h_flex().flex_1().min_w_0().gap(px(S4)).items_center();
        if let Some(c) = conn {
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
            )
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_size(px(TX_SM))
                    .text_color(t.tx3)
                    .child(tr.diagram_scope_text(cx)),
            );
        crumb
    }

    /// Depth, scope, zoom, arrange, export and the right-panel toggle.
    fn diagram_tools(&self, tab: &Entity<QueryTab>, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let sep = || div().flex_none().w(px(1.)).h(px(20.)).bg(t.on);
        let (supports_ddl, loaded) = {
            let tr = tab.read(cx);
            (tr.diagram_supports_ddl(), tr.dgm.view.is_some())
        };
        let mut tools = h_flex().flex_none().gap(px(S3)).items_center();
        tools = self.diagram_scope_buttons(tools, tab, cx);
        tools = tools.child(sep());
        tools = self.diagram_zoom_buttons(tools, tab, cx);
        tools = self.diagram_view_buttons(tools, tab, cx);
        // Export: Mermaid, DDL where the driver can. (No PNG / SVG: the diagram
        // is painted by GPUI directly and the crate cannot render it to a file.)
        let weak = tab.downgrade();
        tools = tools.child(
            Button::new("er-export")
                .ghost()
                .small()
                .icon(IconName::ExternalLink)
                .label(t!("erv3.export").to_string())
                .disabled(!loaded)
                .dropdown_menu(move |menu, _, _| {
                    let w1 = weak.clone();
                    let mut menu = menu.item(
                        PopupMenuItem::new(t!("erv3.copy_mermaid").to_string()).on_click(
                            move |_, window, cx| {
                                let _ = w1.update(cx, |t, cx| t.copy_mermaid(window, cx));
                            },
                        ),
                    );
                    if supports_ddl {
                        let w2 = weak.clone();
                        menu = menu.item(
                            PopupMenuItem::new(t!("erv3.copy_ddl").to_string()).on_click(
                                move |_, window, cx| {
                                    let _ = w2.update(cx, |t, cx| t.copy_ddl(window, cx));
                                },
                            ),
                        );
                    }
                    menu
                }),
        );
        tools = tools.child(sep()).child(
            ib(
                "panel-toggle",
                crate::shell::icons::icon("panel-right"),
                26.,
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
        );
        tools
    }

    /// Depth menu (centred diagrams only), "show whole schema" and "show hidden".
    fn diagram_scope_buttons(
        &self,
        mut tools: Div,
        tab: &Entity<QueryTab>,
        cx: &mut Context<Self>,
    ) -> Div {
        let tr = tab.read(cx);
        let depth = tr.diagram_depth(cx);
        let whole = matches!(tr.diagram_scope(cx), er_diagram::Scope::All);
        let has_hidden = tr.diagram_has_hidden(cx);
        // Depth (centred diagrams only).
        if let Some(d) = depth {
            let weak = tab.downgrade();
            let label = depth_label(d);
            tools = tools.child(
                Button::new("er-depth")
                    .ghost()
                    .small()
                    .label(label)
                    .tooltip(t!("erv3.depth_hint").to_string())
                    .dropdown_menu(move |mut menu, _, _| {
                        for opt in [Depth::Hops(1), Depth::Hops(2), Depth::All] {
                            let weak = weak.clone();
                            menu = menu.item(
                                PopupMenuItem::new(depth_label(opt))
                                    .checked(d == opt)
                                    .on_click(move |_, _, cx| {
                                        let _ =
                                            weak.update(cx, |t, cx| t.diagram_set_depth(opt, cx));
                                    }),
                            );
                        }
                        menu
                    }),
            );
        }
        if !whole {
            let tab = tab.clone();
            tools = tools.child(
                Button::new("er-whole")
                    .ghost()
                    .small()
                    .label(t!("erv3.show_whole").to_string())
                    .on_click(move |_, _, cx| tab.update(cx, |t, cx| t.diagram_show_whole(cx))),
            );
        }
        if has_hidden {
            let tab = tab.clone();
            tools = tools.child(
                Button::new("er-unhide")
                    .ghost()
                    .small()
                    .label(t!("erv3.show_hidden").to_string())
                    .on_click(move |_, _, cx| tab.update(cx, |t, cx| t.diagram_show_hidden(cx))),
            );
        }
        tools
    }

    /// Zoom: − 100 % +, bound to the view's zoom, then fit and auto arrange.
    fn diagram_zoom_buttons(
        &self,
        mut tools: Div,
        tab: &Entity<QueryTab>,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let zoom = tab.read(cx).diagram_zoom_pct(cx);
        let (t_out, t_in, t_auto, t_fit) = (tab.clone(), tab.clone(), tab.clone(), tab.clone());
        tools = tools
            .child(
                ib(
                    "er-zoom-out",
                    crate::shell::icons::icon("minus"),
                    26.,
                    false,
                    cx,
                )
                .tooltip(tip(t!("erv3.zoom_out").into_owned()))
                .on_click(move |_, _, cx| t_out.update(cx, |t, cx| t.diagram_zoom(-1, cx))),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(42.))
                    .text_center()
                    .text_size(px(TX_XS))
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_color(t.tx3)
                    .child(zoom.map(|z| format!("{z}%")).unwrap_or_else(|| "—".into())),
            )
            .child(
                ib(
                    "er-zoom-in",
                    crate::shell::icons::icon("plus"),
                    26.,
                    false,
                    cx,
                )
                .tooltip(tip(t!("erv3.zoom_in").into_owned()))
                .on_click(move |_, _, cx| t_in.update(cx, |t, cx| t.diagram_zoom(1, cx))),
            )
            .child(
                Button::new("er-fit")
                    .ghost()
                    .small()
                    .label(t!("t2.diagram_fit").to_string())
                    .tooltip(t!("t2.diagram_fit_tip").to_string())
                    .on_click(move |_, _, cx| t_fit.update(cx, |t, cx| t.diagram_fit(cx))),
            )
            .child(
                Button::new("er-arrange")
                    .ghost()
                    .small()
                    .label(t!("erv3.arrange").to_string())
                    .on_click(move |_, _, cx| {
                        t_auto.update(cx, |t, cx| t.diagram_auto_arrange(cx))
                    }),
            );
        tools
    }

    /// The View menu (card detail, line style) and the minimap.
    fn diagram_view_buttons(
        &self,
        tools: Div,
        tab: &Entity<QueryTab>,
        cx: &mut Context<Self>,
    ) -> Div {
        let (density, minimap) = {
            let tr = tab.read(cx);
            (tr.diagram_density(cx), tr.diagram_minimap(cx))
        };
        let Some(d) = density else {
            return tools;
        };
        let weak = tab.downgrade();
        let t_mm = tab.clone();
        let style = tab.read(cx).diagram_edge_style(cx).unwrap_or_default();
        tools
            .child(
                // One "View" menu: card detail, then line style.
                Button::new("er-view")
                    .ghost()
                    .small()
                    .label(t!("erv3.view").to_string())
                    .dropdown_menu(move |mut menu, _, _| {
                        menu = menu.label(t!("erv3.density").to_string());
                        for opt in [Density::Full, Density::Keys, Density::Header] {
                            let weak = weak.clone();
                            menu = menu.item(
                                PopupMenuItem::new(density_label(opt))
                                    .checked(d == opt)
                                    .on_click(move |_, _, cx| {
                                        let _ =
                                            weak.update(cx, |t, cx| t.diagram_set_density(opt, cx));
                                    }),
                            );
                        }
                        menu = menu.separator().label(t!("erv3.edge_style").to_string());
                        for opt in [EdgeStyle::Bezier, EdgeStyle::Smooth] {
                            let weak = weak.clone();
                            menu = menu.item(
                                PopupMenuItem::new(edge_label(opt))
                                    .checked(style == opt)
                                    .on_click(move |_, _, cx| {
                                        let _ = weak
                                            .update(cx, |t, cx| t.diagram_set_edge_style(opt, cx));
                                    }),
                            );
                        }
                        menu
                    }),
            )
            .child(
                ib(
                    "er-minimap",
                    crate::shell::icons::icon("map"),
                    26.,
                    minimap,
                    cx,
                )
                .tooltip(tip(t!("erv3.minimap").into_owned()))
                .on_click(move |_, _, cx| t_mm.update(cx, |t, cx| t.diagram_toggle_minimap(cx))),
            )
    }

    /// Status bar of a diagram tab: state, version | tables and keys, hint | Space.
    pub(crate) fn render_diagram_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let tab = self.active_tab(cx);
        let tr = tab.read(cx);
        let conn = self.active_conn(cx).cloned();
        let s = self.space();
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
        let counts = match tr.diagram_counts(cx) {
            Some((n, fk)) => t!("erv3.counts", n = n.to_string(), fk = fk.to_string()).into_owned(),
            None => String::new(),
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
            .overflow_hidden()
            .child(left)
            .child(div().flex_1())
            .child(counts)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(t.tx4)
                    .child(t!("erv3.hint").into_owned()),
            )
            .child(
                h_flex()
                    .gap(px(S3))
                    .items_center()
                    .child(div().size(px(6.)).rounded_full().bg(s.accent(cx)))
                    .child(s.name.clone()),
            )
            .into_any_element()
    }
}

fn depth_label(d: Depth) -> String {
    match d {
        Depth::Hops(1) => t!("erv3.depth_1").into_owned(),
        Depth::Hops(n) => t!("erv3.depth_n", n = n.to_string()).into_owned(),
        Depth::All => t!("erv3.depth_all").into_owned(),
    }
}

fn density_label(d: Density) -> String {
    match d {
        Density::Full => t!("erv3.density_full").into_owned(),
        Density::Keys => t!("erv3.density_keys").into_owned(),
        Density::Header => t!("erv3.density_header").into_owned(),
    }
}

fn edge_label(s: EdgeStyle) -> String {
    match s {
        EdgeStyle::Smooth => t!("erv3.edge_smooth").into_owned(),
        EdgeStyle::Bezier => t!("erv3.edge_curved").into_owned(),
    }
}
