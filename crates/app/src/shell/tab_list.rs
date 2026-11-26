//! Sidebar: tab rows, split-group row, the dragged-tab ghost.
use super::*;

// Width of the dragged-tab ghost (matches the default sidebar row).
const W_TAB_GHOST: f32 = 232.0;

/// Dragged tab row (reorder in the list).
#[derive(Clone)]
pub(crate) struct TabDrag {
    pub(crate) id: String,
    pub(crate) title: SharedString,
    /// Connection name and marker colour, so the ghost looks like the tab row.
    pub(crate) conn: Option<SharedString>,
    pub(crate) marker: Hsla,
}

impl Render for TabDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        h_flex()
            .w(px(W_TAB_GHOST))
            .h(px(scale::H_TAB))
            .gap_2()
            .px(px(scale::S4))
            .rounded(px(scale::R_MD))
            .items_center()
            .bg(t.pop)
            .border_1()
            .border_color(t.ln2)
            .shadow_lg()
            .opacity(0.95)
            .child(
                div()
                    .flex_none()
                    .size(px(8.))
                    .rounded_full()
                    .bg(self.marker),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(scale::TX_BASE))
                            .text_color(t.tx1)
                            .truncate()
                            .child(self.title.clone()),
                    )
                    .when_some(self.conn.clone(), |v, c| {
                        v.child(
                            div()
                                .text_size(px(scale::TX_SM))
                                .text_color(t.tx3)
                                .truncate()
                                .child(c),
                        )
                    }),
            )
    }
}

impl Locus {
    pub(crate) fn render_tab_row(
        &self,
        i: usize,
        tab: &Entity<QueryTab>,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let theme = cx.theme().clone();
        let tr = tab.read(cx);
        let id = tr.id.clone();
        let title = tr.display_title(cx);
        let busy = tr.busy(cx);
        let dirty = tr.doc.modified;
        let pinned = tr.pinned;
        let kind = tab_kind(tr);
        let failed = tr.status.read(cx).phase == locus_core::status::Phase::Failed;
        let focused = self.space().focused_tab_id() == id;
        let conn = tr
            .conn_id
            .as_ref()
            .and_then(|c| self.conns.iter().find(|x| &x.id == c))
            .cloned();
        let marker = match &conn {
            Some(c) => self.conn_color(&c.id, cx),
            None => t.tx5,
        };
        let prod = conn
            .as_ref()
            .is_some_and(|c| c.env == locus_core::store::Env::Prod);
        let col = self.space().accent(cx);
        let renaming = matches!(&self.renaming, Some((Rename::Tab(r), _, _)) if *r == id);
        let id2 = id.clone();
        let group = SharedString::from(format!("tab-row-{id}"));
        let mut row = h_flex()
            .id(SharedString::from(format!("tab-{id}")))
            .group(group.clone())
            .relative()
            .flex_none()
            .rounded(px(scale::R_MD))
            .mb(px(1.))
            .items_center()
            .cursor_pointer()
            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                if e.click_count() >= 2 {
                    this.start_rename(Rename::Tab(id2.clone()), window, cx);
                } else {
                    this.show_tab(&id2, window, cx);
                }
            }));
        let drag = TabDrag {
            id: id.clone(),
            title: title.clone().into(),
            conn: conn.as_ref().map(|c| c.name.clone().into()),
            marker,
        };
        let target = id.clone();
        row = row
            .on_drag(drag, |d, _, _, cx| cx.new(|_| d.clone()))
            .drag_over::<TabDrag>(move |s, _, _, cx| {
                s.border_t_2().border_color(cx.theme().primary)
            })
            .on_drop(cx.listener(move |this, d: &TabDrag, _, cx| {
                this.move_tab_before(&d.id, &target, cx)
            }));
        let icon = tab_icon(busy, failed, focused, kind, col, cx);
        if collapsed {
            return row
                .size(px(scale::H_ROW))
                .justify_center()
                .when(focused, |r| r.bg(t.tile_on))
                .hover(|s| s.bg(t.hv))
                .tooltip(tip(title.clone()))
                .child(icon)
                .when(dirty, |r| {
                    r.child(
                        div()
                            .absolute()
                            .top(px(5.))
                            .right(px(5.))
                            .size(px(5.))
                            .rounded_full()
                            .bg(t.acc),
                    )
                })
                .into_any_element();
        }
        row = row
            .w_full()
            .h(px(if focused { scale::H_TAB } else { scale::H_ROW }));
        row = if focused {
            row.bg(t.on).border_1().border_color(t.hv)
        } else {
            row.hover(|s| s.bg(t.hv))
        };
        let label = self.tab_label(&title, renaming, &conn, focused, cx);
        let me = cx.entity().downgrade();
        let spaces: Vec<(usize, String)> = self
            .spaces
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != self.active_space)
            .map(|(j, s)| (j, s.name.clone()))
            .collect();
        let cid = id.clone();
        let can_split = !self.is_page(&id, cx) && !self.space().panes().contains(&id);
        let grouped = self.space().groups.group_of(&id).is_some();
        let close = self.tab_close_button(i, &id, dirty, focused, &group, cx);
        let _ = &theme;
        let row_el = row
            .when(prod, |r| {
                r.child(
                    div()
                        .absolute()
                        .left(px(-4.))
                        .top(px(9.))
                        .bottom(px(9.))
                        .w(px(2.))
                        .rounded(px(2.))
                        .bg(t.red),
                )
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .pl(px(scale::S4))
                    .gap(px(scale::S4))
                    .items_center()
                    .child(div().flex_none().child(icon))
                    .child(div().flex_1().min_w_0().child(label)),
            )
            .children(self.git_tab_marker(tab, cx))
            .child(close)
            .context_menu(Self::tab_menu(me, cid, pinned, can_split, grouped, spaces));
        if self.reduce_motion {
            return row_el.into_any_element();
        }
        // New tab rows ease in (once per tab id).
        div()
            .child(row_el)
            .with_animation(
                SharedString::from(format!("tab-in-{id}")),
                Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
                |d, t| d.opacity(t).ml(px((1.0 - t) * -10.0)),
            )
            .into_any_element()
    }

    /// Title (or the rename field) and, for the focused tab, its connection and database.
    fn tab_label(
        &self,
        title: &str,
        renaming: bool,
        conn: &Option<SavedConnection>,
        focused: bool,
        cx: &App,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        match (&self.renaming, renaming) {
            (Some((_, input, _)), true) => Input::new(input).xsmall().into_any_element(),
            _ => v_flex()
                .min_w_0()
                .gap(px(scale::S1))
                .child(
                    div()
                        .text_size(px(scale::TX_BASE))
                        .line_height(relative(1.4))
                        .truncate()
                        .text_color(if focused { t.tx1 } else { t.tx2 })
                        .when(focused, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .child(title.to_string()),
                )
                .when_some(conn.as_ref().filter(|_| focused), |v, c| {
                    let sub = match &c.database {
                        Some(db) => format!("{} · {}", c.name, db),
                        None => c.name.clone(),
                    };
                    v.child(
                        div()
                            .text_size(px(scale::TX_SM))
                            .line_height(relative(1.4))
                            .text_color(t.tx3)
                            .truncate()
                            .child(sub),
                    )
                })
                .into_any_element(),
        }
    }

    /// Unsaved dot, which turns into the close button on hover.
    fn tab_close_button(
        &self,
        i: usize,
        id: &str,
        dirty: bool,
        focused: bool,
        group: &SharedString,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let close_id = id.to_string();
        let group = group.clone();
        div()
            .relative()
            .flex_none()
            .size(px(scale::H_COMPACT))
            .mr(px(scale::S2))
            .when(dirty, |d| {
                d.child(
                    div()
                        .absolute()
                        .left(px(8.))
                        .top(px(8.))
                        .size(px(6.))
                        .rounded_full()
                        .bg(t.acc)
                        .group_hover(group.clone(), |s| s.invisible()),
                )
            })
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .opacity(if focused && !dirty { 0.7 } else { 0.0 })
                    .group_hover(group.clone(), |s| s.opacity(1.0))
                    .child(
                        ib(
                            ("close-tab", i),
                            crate::shell::icons::icon("close"),
                            scale::H_COMPACT,
                            false,
                            cx,
                        )
                        .tooltip(tip(t!("tab.close").into_owned()))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(&close_id, window, cx);
                            },
                        )),
                    ),
            )
    }

    /// Right-click menu of a tab row: rename, pin, split, move to another Space, close.
    fn tab_menu(
        me: WeakEntity<Locus>,
        cid: String,
        pinned: bool,
        can_split: bool,
        grouped: bool,
        spaces: Vec<(usize, String)>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        move |menu: PopupMenu, window, cx| {
            use crate::shell::menu::item;
            let act = |f: Box<dyn Fn(&mut Locus, &mut Window, &mut Context<Locus>)>| {
                let me = me.clone();
                move |window: &mut Window, cx: &mut App| {
                    let _ = me.update(cx, |this, cx| f(this, window, cx));
                }
            };
            // Group 1: the tab itself.
            let (c1, c2) = (cid.clone(), cid.clone());
            let mut menu = menu
                .item(item(
                    t!("tab.rename").into_owned(),
                    Some("pencil"),
                    act(Box::new(move |this, w, cx| {
                        this.start_rename(Rename::Tab(c1.clone()), w, cx)
                    })),
                ))
                .item(item(
                    if pinned {
                        t!("tab.unpin")
                    } else {
                        t!("tab.pin")
                    }
                    .into_owned(),
                    Some("push"),
                    act(Box::new(move |this, _, cx| {
                        this.set_pinned(&c2, !pinned, cx)
                    })),
                ));
            // Group 2: layout.
            if can_split || grouped {
                menu = menu.separator();
            }
            if can_split {
                let (cid2, me2) = (cid.clone(), me.clone());
                menu = menu.submenu_with_icon(
                    Some(crate::shell::icons::icon("panel-right")),
                    t!("t4.menu_split").to_string(),
                    window,
                    cx,
                    move |mut sub, _, _| {
                        for (key, axis, before) in [
                            ("pane.split_right", SplitAxis::Row, false),
                            ("pane.split_left", SplitAxis::Row, true),
                            ("pane.split_down", SplitAxis::Column, false),
                            ("pane.split_up", SplitAxis::Column, true),
                        ] {
                            let (c, me) = (cid2.clone(), me2.clone());
                            sub = sub.item(item(t!(key).into_owned(), None, move |w, cx| {
                                let _ = me.update(cx, |this, cx| {
                                    this.split(Some(c.clone()), axis, before, w, cx)
                                });
                            }));
                        }
                        sub
                    },
                );
            }
            if grouped {
                let c = cid.clone();
                menu = menu.item(item(
                    t!("split2.remove").into_owned(),
                    Some("minus"),
                    act(Box::new(move |this, w, cx| this.unsplit_tab(&c, w, cx))),
                ));
            }
            // Group 3: Spaces (a submenu, not one row per Space).
            if !spaces.is_empty() {
                let (cid2, me2, spaces2) = (cid.clone(), me.clone(), spaces.clone());
                menu = menu.separator().submenu_with_icon(
                    Some(crate::shell::icons::icon("folder")),
                    t!("t4.menu_move_space").to_string(),
                    window,
                    cx,
                    move |mut sub, _, _| {
                        for (j, name) in spaces2.clone() {
                            let (c, me) = (cid2.clone(), me2.clone());
                            sub = sub.item(item(name, None, move |w, cx| {
                                let _ =
                                    me.update(cx, |this, cx| this.move_tab_to_space(&c, j, w, cx));
                            }));
                        }
                        sub
                    },
                );
            }
            let c5 = cid.clone();
            menu.separator().item(item(
                t!("tab.close").into_owned(),
                Some("close"),
                act(Box::new(move |this, w, cx| this.close_tab(&c5, w, cx))),
            ))
        }
    }

    /// One sidebar row for a split group: collapsed it shows one segment per
    /// pane, expanded a sub-row per pane (drag one out to leave the split).
    pub(crate) fn render_split_group(&self, gid: &str, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let s = self.space();
        let Some(g) = s.groups.get(gid).cloned() else {
            return div().into_any_element();
        };
        let col = s.accent(cx);
        let cur = s.focused_tab_id();
        let on = g.panes.contains(&cur);
        let focus_id = if on {
            cur.clone()
        } else if g.panes.contains(&g.focus) {
            g.focus.clone()
        } else {
            g.panes[0].clone()
        };
        let name = s
            .tab(&focus_id, cx)
            .map(|x| x.read(cx).display_title(cx))
            .unwrap_or_default();
        let prod = self.group_has_prod(&g.panes, cx);
        let n = g.panes.len();
        // SP2: the group row shows that its focused pane is maximised.
        let zoomed = on && s.zoomed();
        let open = g.open;
        let me = cx.entity().downgrade();
        let gid_s = gid.to_string();
        let f1 = focus_id.clone();
        let segs = h_flex()
            .gap(px(scale::S1))
            .children(g.panes.iter().map(|p| {
                div()
                    .w(px(12.))
                    .h(px(3.))
                    .rounded(px(2.))
                    .bg(if *p == cur { col } else { t.tx5 })
            }));
        let header = h_flex()
            .id(SharedString::from(format!("grp-{gid}")))
            .relative()
            .flex_none()
            .w_full()
            .h(px(if on { scale::H_TAB } else { scale::H_ROW }))
            .rounded(px(scale::R_MD))
            .items_center()
            .cursor_pointer()
            .when(on, |r| r.bg(t.on).border_1().border_color(t.hv))
            .when(!on, |r| r.hover(|s| s.bg(t.hv)))
            .tooltip(tip(t!("split2.row_tip").into_owned()))
            .drag_over::<TabDrag>(move |s, _, _, cx| s.bg(cx.theme().primary.opacity(0.12)))
            .on_drop(cx.listener({
                let f = focus_id.clone();
                move |this, d: &TabDrag, window, cx| {
                    // Dropping a tab on the group row adds it as a pane on the right.
                    let inside = this
                        .space()
                        .groups
                        .group_of(&f)
                        .is_some_and(|g| g.panes.contains(&d.id));
                    if !inside {
                        this.show_tab(&f, window, cx);
                        this.split(Some(d.id.clone()), SplitAxis::Row, false, window, cx);
                    }
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.show_tab(&f1, window, cx)))
            .when(prod, |r| {
                r.child(
                    div()
                        .absolute()
                        .left(px(-4.))
                        .top(px(9.))
                        .bottom(px(9.))
                        .w(px(2.))
                        .rounded(px(2.))
                        .bg(t.red),
                )
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .pl(px(scale::S4))
                    .gap(px(scale::S4))
                    .items_center()
                    .child(
                        crate::shell::icons::icon("panel-right")
                            .size(px(14.))
                            .text_color(if on { col } else { t.tx3 }),
                    )
                    .child(group_label(name, on, zoomed, n, segs, cx)),
            )
            .child(
                ib(
                    SharedString::from(format!("grp-open-{gid}")),
                    crate::shell::icons::icon(if open { "caret-down" } else { "chevron-right" }),
                    22.,
                    false,
                    cx,
                )
                .mr(px(scale::S2))
                .tooltip(tip(if open {
                    t!("split2.collapse")
                } else {
                    t!("split2.expand")
                }
                .into_owned()))
                .on_click({
                    let g = gid_s.clone();
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_group_open(&g, cx);
                    })
                }),
            )
            .context_menu(Self::group_menu(me, gid_s.clone(), focus_id.clone(), open));
        self.group_pane_rows(header, &g.panes, (on, &cur, open), cx)
    }

    /// Does any pane of the group belong to a Production connection?
    fn group_has_prod(&self, panes: &[String], cx: &App) -> bool {
        let s = self.space();
        panes.iter().any(|p| {
            s.tab(p, cx)
                .and_then(|x| x.read(cx).conn_id.clone())
                .is_some_and(|c| {
                    self.conns
                        .iter()
                        .any(|x| x.id == c && x.env == locus_core::store::Env::Prod)
                })
        })
    }

    /// Right-click menu of a split group row.
    fn group_menu(
        me: WeakEntity<Locus>,
        gid: String,
        focus_id: String,
        open: bool,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        move |menu: PopupMenu, _, _| {
            let act =
                |label: String,
                 icon: &'static str,
                 danger: bool,
                 f: Box<dyn Fn(&mut Locus, &mut Window, &mut Context<Locus>)>| {
                    let me = me.clone();
                    crate::shell::menu::row(label, Some(icon), None, danger, move |window, cx| {
                        let _ = me.update(cx, |this, cx| f(this, window, cx));
                    })
                };
            let (g1, g3, g4) = (gid.clone(), gid.clone(), gid.clone());
            let fid = focus_id.clone();
            menu.item(act(
                if open {
                    t!("split2.collapse")
                } else {
                    t!("split2.expand")
                }
                .into_owned(),
                "chevron-right",
                false,
                Box::new(move |this, _, cx| this.toggle_group_open(&g1, cx)),
            ))
            .item(act(
                t!("split2.menu_zoom").into_owned(),
                "maximize",
                false,
                Box::new(move |this, w, cx| {
                    this.show_tab(&fid, w, cx);
                    if !this.space().zoomed() {
                        this.toggle_zoom(cx);
                    }
                }),
            ))
            .separator()
            .item(act(
                t!("split2.dissolve").into_owned(),
                "restore",
                false,
                Box::new(move |this, w, cx| this.dissolve_group_ui(&g3, w, cx)),
            ))
            .item(act(
                t!("split2.close_group").into_owned(),
                "close",
                true,
                Box::new(move |this, w, cx| this.close_group(&g4, w, cx)),
            ))
        }
    }

    /// The group row with, when expanded, one sub-row per pane.
    fn group_pane_rows(
        &self,
        header: impl IntoElement,
        panes: &[String],
        (on, cur, open): (bool, &String, bool),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let s = self.space();
        let col = s.accent(cx);
        let mut rows = v_flex().mb(px(1.)).child(header);
        if open {
            for (ix, id) in panes.iter().enumerate() {
                let Some(tab) = s.tab(id, cx) else { continue };
                let tr = tab.read(cx);
                let title = tr.display_title(cx);
                let focused = on && *cur == *id;
                let busy = tr.busy(cx);
                let kind = tab_kind(tr);
                let conn = tr
                    .conn_id
                    .as_ref()
                    .and_then(|c| self.conns.iter().find(|x| &x.id == c))
                    .cloned();
                let marker = conn
                    .as_ref()
                    .map(|c| self.conn_color(&c.id, cx))
                    .unwrap_or(t.tx5);
                let drag = TabDrag {
                    id: id.clone(),
                    title: title.clone().into(),
                    conn: conn.as_ref().map(|c| c.name.clone().into()),
                    marker,
                };
                let (i1, i2, i3) = (id.clone(), id.clone(), id.clone());
                rows = rows.child(
                    h_flex()
                        .id(("seg", ix))
                        .relative()
                        .h(px(scale::H_CONTROL))
                        .pl(px(scale::S7 + scale::S1))
                        .pr(px(scale::S2))
                        .gap(px(scale::S3))
                        .items_center()
                        .rounded(px(scale::R_SM))
                        .cursor_pointer()
                        .hover(|st| st.bg(t.hv))
                        .tooltip(tip(t!("split2.drag_out").into_owned()))
                        .on_drag(drag, |d, _, _, cx| cx.new(|_| d.clone()))
                        .drag_over::<TabDrag>(move |s, _, _, cx| {
                            s.border_t_2().border_color(cx.theme().primary)
                        })
                        .on_drop(cx.listener(move |this, d: &TabDrag, _, cx| {
                            this.move_tab_before(&d.id, &i3, cx)
                        }))
                        .when(focused, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .left(px(18.))
                                    .top(px(8.))
                                    .w(px(3.))
                                    .h(px(10.))
                                    .rounded(px(2.))
                                    .bg(t.acc),
                            )
                        })
                        .child(if busy {
                            Icon::new(IconName::LoaderCircle)
                                .size(px(13.))
                                .text_color(t.acc)
                                .into_any_element()
                        } else {
                            kind_icon(kind)
                                .size(px(13.))
                                .text_color(if focused { col } else { kind.color(cx) })
                                .into_any_element()
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(scale::TX_SM))
                                .text_color(if focused { t.tx1 } else { t.tx3 })
                                .child(title),
                        )
                        .children(conn.map(|c| {
                            div()
                                .flex_none()
                                .text_size(px(scale::TX_SM))
                                .text_color(t.tx4)
                                .child(c.name.clone())
                        }))
                        .child(
                            ib(
                                ("seg-close", ix),
                                crate::shell::icons::icon("close"),
                                scale::H_COMPACT,
                                false,
                                cx,
                            )
                            .tooltip(tip(t!("pane.unsplit").into_owned()))
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.unsplit_tab(&i1, window, cx);
                                },
                            )),
                        )
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.show_tab(&i2, window, cx)),
                        ),
                );
            }
        }
        rows.into_any_element()
    }
}

/// Status icon of a tab row: spinner while busy, cross when failed, else its kind.
fn tab_icon(
    busy: bool,
    failed: bool,
    focused: bool,
    kind: crate::shell::theme::TabKind,
    col: Hsla,
    cx: &App,
) -> AnyElement {
    let t = crate::shell::theme::tokens(cx);
    if busy {
        Icon::new(IconName::LoaderCircle)
            .size(px(14.))
            .text_color(t.acc)
            .into_any_element()
    } else if failed {
        Icon::new(IconName::CircleX)
            .size(px(14.))
            .text_color(t.red)
            .into_any_element()
    } else {
        kind_icon(kind)
            .size(px(14.))
            .text_color(if focused { col } else { kind.color(cx) })
            .into_any_element()
    }
}

/// Name with the pane count badge, and below it the group size (focused) or one bar per pane.
fn group_label(name: String, on: bool, zoomed: bool, n: usize, segs: Div, cx: &App) -> Div {
    let t = crate::shell::theme::tokens(cx);
    v_flex()
        .min_w_0()
        .gap(px(scale::S1))
        .child(
            h_flex()
                .gap(px(scale::S3))
                .min_w_0()
                .text_size(px(scale::TX_BASE))
                .child(
                    div()
                        .truncate()
                        .text_color(if on { t.tx1 } else { t.tx2 })
                        .when(on, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .child(name),
                )
                .child(
                    div()
                        .flex_none()
                        .px(px(scale::S2))
                        .rounded(px(scale::R_XS))
                        .text_color(if zoomed { t.acc_text } else { t.tx4 })
                        .when(zoomed, |d| d.bg(t.acc.opacity(0.16)))
                        .child(format!("+{}", n - 1)),
                ),
        )
        .child(if on {
            div()
                .text_size(px(scale::TX_SM))
                .text_color(t.tx3)
                .truncate()
                .child(t!("shell.split_group", n = n.to_string()).into_owned())
                .into_any_element()
        } else {
            segs.into_any_element()
        })
}
