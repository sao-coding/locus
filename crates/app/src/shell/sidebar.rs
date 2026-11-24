//! Sidebar frame (v3): search, Connections section, pinned tiles, New query,
//! Tabs section with filter, footer with Space dots; icon mode.
use super::*;

impl Locus {
    pub(crate) fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.sidebar == SidebarMode::Icons {
            return self.render_icon_sidebar(cx);
        }
        let t = crate::shell::theme::tokens(cx);
        let s = self.space();

        let search = self.sidebar_search(cx);
        let (conn_head, conns) = self.sidebar_connections(cx);
        let ordered = s.ordered(cx);
        let pins = self.sidebar_pins(&ordered, cx);
        let new_query = self.sidebar_new_query(cx);
        let (tabs_head, list) = self.sidebar_tab_list(&ordered, cx);

        let body = v_flex()
            .id("sidebar-content")
            .flex_1()
            .min_h_0()
            .w_full()
            .child(search)
            .child(conn_head)
            .child(conns)
            .child(
                div()
                    .flex_none()
                    .h(px(1.))
                    .bg(t.ln)
                    .mx(px(scale::S5))
                    .my(px(scale::S4)),
            )
            .children(pins)
            .child(new_query)
            .child(tabs_head)
            .children(self.tab_filter.as_ref().map(|(input, _)| {
                div()
                    .flex_none()
                    .px(px(scale::S5))
                    .pb(px(scale::S3))
                    .child(Input::new(input).px(px(scale::S5)))
            }))
            .child(list);

        let outer = v_flex()
            .id("sidebar")
            .flex_none()
            .w(px(self.sidebar_width))
            .h_full()
            .text_size(px(scale::TX_BASE))
            .child(self.animate_content(body))
            .child(self.render_sidebar_footer(cx));
        self.animate_sidebar(outer)
    }

    /// The "Search ... Ctrl K" field that opens the palette.
    fn sidebar_search(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = crate::shell::theme::tokens(cx);
        h_flex()
            .id("sb-search")
            .flex_none()
            .mx(px(scale::S5))
            .mt(px(scale::S2))
            .mb(px(scale::S5))
            .h(px(scale::H_ROW))
            .px(px(scale::S4))
            .gap(px(scale::S4))
            .items_center()
            .rounded(px(scale::R_MD))
            .bg(t.hv)
            .text_color(t.tx3)
            .cursor_pointer()
            .hover(|st| st.bg(t.on))
            .child(crate::shell::icons::icon("search").size(px(14.)))
            .child(div().flex_1().child(t!("shell.search").into_owned()))
            .child(kbd("Ctrl K", cx))
            .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx)))
    }

    /// Connections section: header with the add menu, and the list of the Space's connections.
    fn sidebar_connections(&self, cx: &mut Context<Self>) -> (Div, Stateful<Div>) {
        let t = crate::shell::theme::tokens(cx);
        let s = self.space();
        let members: Vec<SavedConnection> = s
            .connections
            .iter()
            .filter_map(|id| self.conns.iter().find(|c| &c.id == id).cloned())
            .collect();
        let me = cx.entity().downgrade();
        let others: Vec<(String, String)> = self
            .conns
            .iter()
            .filter(|c| !s.connections.contains(&c.id))
            .map(|c| (c.id.clone(), c.name.clone()))
            .collect();
        let add_conn = Button::new("add-conn")
            .ghost()
            .xsmall()
            .icon(crate::shell::icons::icon("plus").size(px(13.)))
            .tooltip(t!("sidebar.add").to_string())
            .dropdown_menu(move |mut menu, _, _| {
                let me1 = me.clone();
                menu = menu.item(
                    PopupMenuItem::new(t!("sidebar.add_new").to_string()).on_click(
                        move |_, window, cx| {
                            let _ = me1.update(cx, |this, cx| this.open_form(None, window, cx));
                        },
                    ),
                );
                if !others.is_empty() {
                    menu = menu
                        .separator()
                        .label(t!("sidebar.add_existing").to_string());
                }
                for (id, name) in others.clone() {
                    let me = me.clone();
                    menu = menu.item(PopupMenuItem::new(name).on_click(move |_, _, cx| {
                        let _ = me.update(cx, |this, cx| this.add_conn_to_space(&id, cx));
                    }));
                }
                menu
            });
        let conn_head = h_flex()
            .flex_none()
            .h(px(scale::H_CONTROL))
            .mx(px(scale::S4))
            .pl(px(scale::S1))
            .pr(px(scale::S2))
            .items_center()
            .child(
                section_label(
                    "conns-toggle",
                    t!("sidebar.connections").into_owned(),
                    members.len(),
                    Some(self.conns_open),
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.conns_open = !this.conns_open;
                    cx.notify();
                })),
            )
            .child(div().flex_1())
            .child(add_conn);
        let mut conns = v_flex()
            .id("conn-list")
            .flex_none()
            .max_h(px(200.))
            .overflow_y_scroll();
        if self.conns_open {
            if members.is_empty() {
                conns = conns.child(
                    h_flex()
                        .id("conn-empty")
                        .h(px(scale::H_ROW))
                        .mx(px(scale::S4))
                        .px(px(scale::S4))
                        .rounded(px(scale::R_MD))
                        .items_center()
                        .text_color(t.tx3)
                        .cursor_pointer()
                        .hover(|st| st.bg(t.hv))
                        .child(t!("shell.no_conns").into_owned())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_form(None, window, cx)),
                        ),
                );
            }
            for (i, c) in members.iter().enumerate() {
                conns = conns.child(self.render_conn_row(i, c, false, cx));
            }
        }
        (conn_head, conns)
    }

    /// Pinned tabs as two-column tiles; `None` when nothing is pinned.
    fn sidebar_pins(&self, ordered: &[Entity<QueryTab>], cx: &mut Context<Self>) -> Option<Div> {
        let t = crate::shell::theme::tokens(cx);
        let s = self.space();
        let col = s.accent(cx);
        let focused = s.focused_tab_id();
        let tile_w = ((self.sidebar_width - 28.0) / 2.0).max(60.0);
        let mut pins = h_flex()
            .flex_none()
            .flex_wrap()
            .gap(px(scale::S2))
            .px(px(scale::S5))
            .pb(px(scale::S4));
        let mut any_pin = false;
        for (i, tab) in ordered
            .iter()
            .enumerate()
            .filter(|(_, x)| x.read(cx).pinned)
        {
            any_pin = true;
            let tr = tab.read(cx);
            let id = tr.id.clone();
            let on = id == focused;
            let kind = tab_kind(tr);
            let title = tr.display_title(cx);
            let me = cx.entity().downgrade();
            let id2 = id.clone();
            pins = pins.child(
                h_flex()
                    .id(("pin", i))
                    .w(px(tile_w))
                    .h(px(scale::H_ROW))
                    .px(px(scale::S4))
                    .gap(px(scale::S3))
                    .items_center()
                    .rounded(px(scale::R_MD))
                    .bg(if on { t.tile_on } else { t.tile })
                    .text_color(if on { t.tx1 } else { t.tx2 })
                    .when(on, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .cursor_pointer()
                    .hover(|st| st.bg(t.on))
                    .tooltip(tip(title.clone()))
                    .child(kind_icon(kind).size(px(14.)).text_color(if on {
                        col
                    } else {
                        kind.color(cx)
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(scale::TX_SM))
                            .child(title),
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.show_tab(&id, window, cx)),
                    )
                    .context_menu(move |menu: PopupMenu, _, _| {
                        let me = me.clone();
                        let id = id2.clone();
                        menu.item(crate::shell::menu::item(
                            t!("tab.unpin").to_string(),
                            Some("push"),
                            move |_, cx| {
                                let _ = me.update(cx, |this, cx| this.set_pinned(&id, false, cx));
                            },
                        ))
                    }),
            );
        }
        any_pin.then_some(pins)
    }

    /// "New query  Ctrl T" row.
    fn sidebar_new_query(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = crate::shell::theme::tokens(cx);
        h_flex()
            .id("sb-new-query")
            .flex_none()
            .mx(px(scale::S4))
            .h(px(scale::H_ROW))
            .px(px(scale::S4))
            .gap(px(scale::S4))
            .items_center()
            .rounded(px(scale::R_MD))
            .text_color(t.tx3)
            .cursor_pointer()
            .hover(|st| st.bg(t.on))
            .child(crate::shell::icons::icon("plus").size(px(14.)))
            .child(div().flex_1().child(t!("shell.new_query").into_owned()))
            .child(kbd("Ctrl T", cx))
            .on_click(cx.listener(|this, _, window, cx| this.new_tab(window, cx)))
    }

    /// Unpinned tabs: the section header with the filter button, and the list faded at the bottom.
    fn sidebar_tab_list(&self, ordered: &[Entity<QueryTab>], cx: &mut Context<Self>) -> (Div, Div) {
        let t = crate::shell::theme::tokens(cx);
        let s = self.space();
        let look = s.look();
        // ---- tabs (unpinned), filtered by title or connection name
        let q = self.tab_filter_text.trim().to_lowercase();
        let matches = |tab: &Entity<QueryTab>, cx: &App| {
            if q.is_empty() {
                return true;
            }
            let tr = tab.read(cx);
            let cname = tr
                .conn_id
                .as_ref()
                .and_then(|c| self.conns.iter().find(|x| &x.id == c))
                .map(|c| c.name.to_lowercase())
                .unwrap_or_default();
            tr.display_title(cx).to_lowercase().contains(&q) || cname.contains(&q)
        };
        let unpinned: Vec<_> = ordered
            .iter()
            .filter(|x| !x.read(cx).pinned)
            .cloned()
            .collect();
        let mut tabs = v_flex()
            .id("tab-list")
            .size_full()
            .px(px(8.))
            .pb(px(scale::S7))
            .overflow_y_scroll();
        let mut done_groups: Vec<String> = Vec::new();
        for (i, tab) in unpinned.iter().enumerate() {
            let id = tab.read(cx).id.clone();
            if let Some(g) = s.groups.group_of(&id) {
                // One row per split group, at the place of its first tab.
                if !done_groups.contains(&g.id) {
                    done_groups.push(g.id.clone());
                    if g.panes
                        .iter()
                        .any(|p| s.tab(p, cx).is_some_and(|x| matches(&x, cx)))
                    {
                        tabs = tabs.child(self.render_split_group(&g.id, cx));
                    }
                }
                continue;
            }
            if matches(tab, cx) {
                tabs = tabs.child(self.render_tab_row(i, tab, false, cx));
            }
        }
        let filter_on = self.tab_filter.is_some();
        let tabs_head = h_flex()
            .flex_none()
            .pt(px(scale::S5))
            .pb(px(scale::S2))
            .pl(px(scale::S6 + scale::S1))
            .pr(px(scale::S4))
            .items_center()
            .text_size(px(scale::TX_XS))
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.tx3)
            .child(t!("sidebar.tabs").into_owned())
            .child(
                div()
                    .ml(px(scale::S3))
                    .font_weight(FontWeight::NORMAL)
                    .text_color(t.tx4)
                    .child(unpinned.len().to_string()),
            )
            .child(div().flex_1())
            .child(
                ib(
                    "tab-filter",
                    crate::shell::icons::icon("filter"),
                    scale::H_COMPACT,
                    filter_on,
                    cx,
                )
                .tooltip(tip(t!("shell.filter_tabs").into_owned()))
                .on_click(cx.listener(|this, _, window, cx| this.toggle_tab_filter(window, cx))),
            );
        // Fade the bottom of the list into the window background.
        let fade = window_at(look, 0.95, cx);
        let mut fade0 = fade;
        fade0.a = 0.0;
        let list = div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(tabs.overflow_y_scrollbar())
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(scale::H_ROW))
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(fade0, 0.),
                        linear_color_stop(fade, 1.),
                    )),
            );
        (tabs_head, list)
    }

    /// Footer (H_ROW): a sliding window of five Space dots (the active one stays
    /// in the middle where it can, edge dots shrink when more Spaces lie beyond),
    /// new Space on the right.
    fn render_sidebar_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let collapsed = self.sidebar == SidebarMode::Icons;
        const WINDOW: usize = 5;
        let n = self.spaces.len();
        let (lo, hi) = if n <= WINDOW {
            (0, n)
        } else {
            let lo = self.active_space.saturating_sub(2).min(n - WINDOW);
            (lo, lo + WINDOW)
        };
        let mut dots = if collapsed {
            v_flex().gap(px(scale::S2)).items_center()
        } else {
            h_flex().gap(px(2.)).items_center()
        };
        for i in lo..hi {
            let sp = &self.spaces[i];
            let active = i == self.active_space;
            let more = (i == lo && lo > 0) || (i + 1 == hi && hi < n);
            let col = sp.accent(cx);
            let label = t!(
                "space.switch_tip",
                name = sp.name.clone(),
                n = (i + 1).to_string()
            )
            .into_owned();
            let size = if active {
                8.
            } else if more {
                4.
            } else {
                6.
            };
            dots = dots.child(
                div()
                    .id(("space", i))
                    .flex_none()
                    .size(px(scale::H_COMPACT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .child(
                        // A Space with an icon shows it instead of the dot; the shrunken
                        // edge dots ("more Spaces beyond") stay dots.
                        match crate::shell::space_popover::space_icon(sp.icon.as_deref())
                            .filter(|_| !more)
                        {
                            Some(i) => Icon::new(i)
                                .size(px(if active { 14. } else { 12. }))
                                .text_color(if active { col } else { col.opacity(0.55) })
                                .into_any_element(),
                            None => div()
                                .size(px(size))
                                .rounded_full()
                                .bg(if active { col } else { col.opacity(0.45) })
                                .into_any_element(),
                        },
                    )
                    .tooltip(tip(label))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.switch_space(i, window, cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                            this.open_space_popover(i, ev.position, window, cx)
                        }),
                    ),
            );
        }
        let plus = ib(
            "new-space",
            crate::shell::icons::icon("plus"),
            scale::H_CONTROL,
            false,
            cx,
        )
        .tooltip(tip(t!("space.new").into_owned()))
        .on_click(cx.listener(|this, _, window, cx| this.new_space(window, cx)));
        if collapsed {
            return v_flex()
                .flex_none()
                .py(px(scale::S4))
                .gap(px(scale::S4))
                .items_center()
                .border_t_1()
                .border_color(t.ln)
                .child(dots)
                .child(plus)
                .into_any_element();
        }
        h_flex()
            .flex_none()
            .h(px(scale::H_ROW))
            .px(px(scale::S5))
            .items_center()
            .border_t_1()
            .border_color(t.ln)
            .child(div().flex_none().w(px(scale::H_CONTROL)))
            .child(div().flex_1().flex().justify_center().child(dots))
            .child(plus)
            .into_any_element()
    }

    /// Icon mode (56 px): search, connection badges, tab markers, Space dots.
    fn render_icon_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let s = self.space();
        let mut col = v_flex()
            .id("sidebar-content")
            .flex_1()
            .min_h_0()
            .w_full()
            .gap(px(scale::S2))
            .items_center()
            .overflow_y_scroll();
        col = col.child(
            ib(
                "sb-search",
                crate::shell::icons::icon("search"),
                scale::H_ROW,
                false,
                cx,
            )
            .tooltip(tip(t!("shell.search_tip").into_owned()))
            .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx))),
        );
        let members: Vec<SavedConnection> = s
            .connections
            .iter()
            .filter_map(|id| self.conns.iter().find(|c| &c.id == id).cloned())
            .collect();
        for (i, c) in members.iter().enumerate() {
            let id = c.id.clone();
            col = col.child(
                div()
                    .id(("conn-icon", i))
                    .size(px(scale::H_ROW))
                    .rounded(px(scale::R_MD))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|st| st.bg(t.hv))
                    .tooltip(tip(c.name.clone()))
                    .child(self.conn_badge(c, cx))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.use_connection(id.clone(), cx)),
                    ),
            );
        }
        col = col.child(
            div()
                .flex_none()
                .h(px(1.))
                .w(px(scale::H_CONTROL))
                .bg(t.ln)
                .my(px(scale::S2)),
        );
        for (i, tab) in s.ordered(cx).iter().enumerate() {
            col = col.child(self.render_tab_row(i, tab, true, cx));
        }
        let outer = v_flex()
            .id("sidebar")
            .flex_none()
            .w(px(56.))
            .h_full()
            .child(self.animate_content(col))
            .child(self.render_sidebar_footer(cx));
        self.animate_sidebar(outer)
    }

    pub(crate) fn toggle_tab_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_filter.take().is_some() {
            self.tab_filter_text.clear();
        } else {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(t!("shell.filter_placeholder").to_string())
            });
            let sub = cx.subscribe_in(&input, window, |this, input, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    this.tab_filter_text = input.read(cx).value().to_string();
                    cx.notify();
                }
            });
            input.update(cx, |s, cx| s.focus(window, cx));
            self.tab_filter = Some((input, sub));
        }
        cx.notify();
    }

    pub(crate) fn set_pinned(&mut self, id: &str, pinned: bool, cx: &mut Context<Self>) {
        // A pinned tab is not part of a split group.
        if pinned {
            self.space_mut().remove_from_group(id);
        }
        if let Some(t) = self.space().tab(id, cx) {
            t.update(cx, |t, cx| {
                t.pinned = pinned;
                cx.notify();
            });
        }
        self.schedule_save(cx);
        cx.notify();
    }
}

/// A section label button ("Connections 3", with a caret when collapsible).
pub(crate) fn section_label(
    id: &'static str,
    text: String,
    n: usize,
    open: Option<bool>,
    cx: &App,
) -> Stateful<Div> {
    let t = crate::shell::theme::tokens(cx);
    h_flex()
        .id(id)
        .h(px(scale::H_COMPACT))
        .px(px(scale::S3))
        .gap(px(scale::S3))
        .items_center()
        .rounded(px(scale::R_SM))
        .text_size(px(scale::TX_XS))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.tx3)
        .cursor_pointer()
        .hover(|st| st.bg(t.hv).text_color(t.tx))
        .children(open.map(|o| {
            crate::shell::icons::icon(if o { "caret-down" } else { "chevron-right" }).size(px(10.))
        }))
        .child(text)
        .child(
            div()
                .font_weight(FontWeight::NORMAL)
                .text_color(t.tx4)
                .child(n.to_string()),
        )
}
