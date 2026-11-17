//! Split layout tree: Layout, splitting / closing / focusing panes, rendering panes.
use super::*;

pub enum Layout {
    Tab(String),
    Split {
        axis: SplitAxis,
        children: Vec<Layout>,
        sizes: Vec<f32>,
        state: Entity<ResizableState>,
    },
}

impl Layout {
    pub(crate) fn leaves(&self, out: &mut Vec<String>) {
        match self {
            Layout::Tab(t) => out.push(t.clone()),
            Layout::Split { children, .. } => children.iter().for_each(|c| c.leaves(out)),
        }
    }

    pub(crate) fn data(&self, cx: &App) -> LayoutData {
        match self {
            Layout::Tab(t) => LayoutData::Tab { tab: t.clone() },
            Layout::Split {
                axis,
                children,
                state,
                sizes,
            } => {
                let live: Vec<f32> = state
                    .read(cx)
                    .sizes()
                    .iter()
                    .map(|p| f32::from(*p))
                    .collect();
                LayoutData::Split {
                    axis: *axis,
                    children: children.iter().map(|c| c.data(cx)).collect(),
                    sizes: if live.len() == children.len() {
                        live
                    } else {
                        sizes.clone()
                    },
                }
            }
        }
    }

    pub(crate) fn from_data(d: &LayoutData, cx: &mut App) -> Layout {
        match d {
            LayoutData::Tab { tab } => Layout::Tab(tab.clone()),
            LayoutData::Split {
                axis,
                children,
                sizes,
            } => Layout::Split {
                axis: *axis,
                children: children.iter().map(|c| Layout::from_data(c, cx)).collect(),
                sizes: sizes.clone(),
                state: cx.new(|_| ResizableState::default()),
            },
        }
    }

    /// Replaces leaf number `ix` (leaf order) with `f(leaf)`. Returns true if found.
    pub(crate) fn map_leaf(
        &mut self,
        ix: usize,
        counter: &mut usize,
        f: &mut dyn FnMut(Layout) -> Layout,
    ) -> bool {
        match self {
            Layout::Tab(_) => {
                if *counter == ix {
                    let me = std::mem::replace(self, Layout::Tab(String::new()));
                    *self = f(me);
                    return true;
                }
                *counter += 1;
                false
            }
            Layout::Split { children, .. } => {
                children.iter_mut().any(|c| c.map_leaf(ix, counter, f))
            }
        }
    }

    /// Removes leaf `ix`; a split left with one child is replaced by it.
    pub(crate) fn remove_leaf(&mut self, ix: usize, counter: &mut usize) -> bool {
        let Layout::Split { children, .. } = self else {
            return false;
        };
        let mut found = None;
        for (i, c) in children.iter_mut().enumerate() {
            if let Layout::Tab(_) = c {
                if *counter == ix {
                    found = Some(i);
                    break;
                }
                *counter += 1;
            } else if c.remove_leaf(ix, counter) {
                return true;
            }
        }
        match found {
            Some(i) => {
                children.remove(i);
                if children.len() == 1 {
                    *self = children.pop().unwrap();
                }
                true
            }
            None => false,
        }
    }

    pub(crate) fn set_leaf(&mut self, ix: usize, tab: &str) {
        let tab = tab.to_string();
        self.map_leaf(ix, &mut 0, &mut |_| Layout::Tab(tab.clone()));
    }
}

impl Locus {
    // ---- panes ------------------------------------------------------------

    /// Splits the focused pane; the new pane shows `tab`, or a new tab.
    pub(crate) fn split(
        &mut self,
        tab: Option<String>,
        axis: SplitAxis,
        before: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A page (History / Settings) never joins a split.
        if self.is_page(&self.space().focused_tab_id(), cx) {
            return;
        }
        let panes = self.space().panes();
        let id = match tab.filter(|t| !panes.contains(t) && !self.is_page(t, cx)) {
            Some(t) => t,
            None => {
                // Prefer a tab not on screen, else a new one.
                let hidden = self
                    .space()
                    .ordered(cx)
                    .iter()
                    .map(|t| t.read(cx).id.clone())
                    .find(|t| {
                        !panes.contains(t)
                            && !self.is_page(t, cx)
                            && self.space().groups.group_of(t).is_none()
                    });
                match hidden {
                    Some(t) => t,
                    None => {
                        let conn = self.active_tab(cx).read(cx).conn_id.clone();
                        let data = self.blank_tab(conn, cx);
                        let t = self.make_tab(&data, window, cx);
                        self.space_mut().tabs.push(t);
                        self.sync_space_conns(self.active_space, cx);
                        data.id
                    }
                }
            }
        };
        // A group holds at most 4 panes: ask what to do with the 5th.
        let anchor = self.space().focused_tab_id();
        if !panes.contains(&id) && self.space().groups.is_full(&anchor) {
            self.group_full_dialog(&id, window, cx);
            return;
        }
        // A tab of another group leaves it.
        self.space_mut().remove_from_group(&id);
        let state = cx.new(|_| ResizableState::default());
        let f = self.space().focused;
        let new_id = id.clone();
        self.space_mut()
            .layout
            .map_leaf(f, &mut 0, &mut |leaf| Layout::Split {
                axis,
                children: if before {
                    vec![Layout::Tab(new_id.clone()), leaf]
                } else {
                    vec![leaf, Layout::Tab(new_id.clone())]
                },
                sizes: Vec::new(),
                state: state.clone(),
            });
        self.space_mut().focused = if before { f } else { f + 1 };
        self.after_layout_change(cx);
        self.focus_editor(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(crate) fn close_pane(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.space().panes().len() < 2 {
            return;
        }
        // The pane's tab leaves its group and stays open; focus moves to a remaining pane.
        let id = self.space().panes()[ix].clone();
        self.space_mut().remove_from_group(&id);
        self.focus_editor(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(crate) fn focus_pane(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.space_mut().focused = ix;
        let id = self.space().focused_tab_id();
        self.space_mut().groups.set_focus(&id);
        self.focus_editor(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(crate) fn focus_editor(&self, window: &mut Window, cx: &mut App) {
        let tab = self.active_tab(cx);
        // A page focuses its search box.
        if let Some(h) = tab.read(cx).pageslot.focus.clone() {
            h.focus(window, cx);
            return;
        }
        let h = tab.read(cx).editor.read(cx).state.read(cx).focus_handle(cx);
        h.focus(window, cx);
    }
}

impl Locus {
    pub(crate) fn render_layout(
        &self,
        layout: &Layout,
        counter: &mut usize,
        path: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match layout {
            Layout::Tab(id) => {
                let ix = *counter;
                *counter += 1;
                self.render_pane(ix, id, cx)
            }
            Layout::Split {
                axis,
                children,
                sizes,
                state,
            } => {
                let id = SharedString::from(format!("split-{path}"));
                let mut group = match axis {
                    SplitAxis::Row => h_resizable(id),
                    SplitAxis::Column => v_resizable(id),
                }
                .with_state(state);
                for (i, c) in children.iter().enumerate() {
                    let el = self.render_layout(c, counter, &format!("{path}.{i}"), cx);
                    let mut p = resizable_panel().child(el);
                    if let Some(sz) = sizes.get(i) {
                        p = p.size(px(*sz));
                    }
                    group = group.child(p);
                }
                let me = cx.entity().downgrade();
                group
                    .on_resize(move |_, _, cx| {
                        let _ = me.update(cx, |this, cx| this.schedule_save(cx));
                    })
                    .into_any_element()
            }
        }
    }

    pub(crate) fn render_pane(
        &self,
        ix: usize,
        tab_id: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let s = self.space();
        let Some(tab) = s.tab(tab_id, cx) else {
            return div().into_any_element();
        };
        let multi = s.panes().len() > 1;
        let drops = cx.has_active_drag();
        if !multi {
            // One pane: the card is the pane, no chrome.
            return div()
                .id(("pane", ix))
                .size_full()
                .child(tab)
                .when(drops, |d| d.relative().child(self.drop_zones(tab_id, cx)))
                .into_any_element();
        }
        let focused = s.focused == ix;
        let header = self.pane_header(ix, tab_id, &tab, cx);
        div()
            .id(("pane", ix))
            .size_full()
            .p(px(scale::S2))
            .child(
                v_flex()
                    .size_full()
                    .rounded(px(scale::R_MD))
                    .border(px(if focused { 1.5 } else { 1. }))
                    .border_color(if focused { t.acc.opacity(0.7) } else { t.ln2 })
                    .overflow_hidden()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            if this.space().focused != ix {
                                this.space_mut().focused = ix;
                                this.schedule_save(cx);
                                cx.notify();
                            }
                        }),
                    )
                    .child(header)
                    .child(div().flex_1().min_h_0().child(tab)),
            )
            .when(drops, |d| d.relative().child(self.drop_zones(tab_id, cx)))
            .into_any_element()
    }

    /// Header of a pane in a split: kind icon, title, connection, zoom and close buttons; drag it to move the tab.
    fn pane_header(
        &self,
        ix: usize,
        tab_id: &str,
        tab: &Entity<QueryTab>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = crate::shell::theme::tokens(cx);
        let s = self.space();
        let focused = s.focused == ix;
        let zoomed = s.zoomed();
        let tr = tab.read(cx);
        let title = tr.display_title(cx);
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
        let prod = conn
            .as_ref()
            .is_some_and(|c| c.env == locus_core::store::Env::Prod);
        let col = space_color(s.hue(), cx);
        let drag = TabDrag {
            id: tab_id.to_string(),
            title: title.clone().into(),
            conn: conn.as_ref().map(|c| c.name.clone().into()),
            marker,
        };
        h_flex()
            .id(("pane-head", ix))
            .group(SharedString::from(format!("pane-head-{ix}")))
            .cursor_grab()
            .on_drag(drag, |d, _, _, cx| cx.new(|_| d.clone()))
            .flex_none()
            .h(px(scale::H_ROW))
            .pl(px(scale::S4))
            .pr(px(scale::S2))
            .gap(px(scale::S3))
            .items_center()
            .text_size(px(scale::TX_SM))
            .border_b_1()
            .border_color(t.ln)
            .when(prod, |h| h.border_t_2().border_color(t.red))
            .child(kind_icon(kind).size(px(13.)).text_color(if focused {
                col
            } else {
                kind.color(cx)
            }))
            .child(
                div()
                    .min_w(px(scale::S8 + scale::S4))
                    .truncate()
                    .text_color(if focused { t.tx1 } else { t.tx2 })
                    .font_weight(if focused {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .child(title),
            )
            .children(conn.as_ref().map(|c| {
                div()
                    .min_w_0()
                    .flex_shrink(4.)
                    .max_w(px(120.))
                    .truncate()
                    .text_size(px(scale::TX_XS))
                    .text_color(t.tx4)
                    .child(c.name.clone())
            }))
            .child(div().flex_1())
            .child(
                h_flex()
                    .flex_none()
                    // E6: the buttons stay quiet until the pane is hovered or focused.
                    .opacity(if focused { 1.0 } else { 0.55 })
                    .group_hover(SharedString::from(format!("pane-head-{ix}")), |s| {
                        s.opacity(1.0)
                    })
                    .child(
                        ib(
                            ("zoom-pane", ix),
                            crate::shell::icons::icon(if zoomed { "restore" } else { "maximize" }),
                            scale::H_COMPACT,
                            false,
                            cx,
                        )
                        .tooltip(tip(if zoomed {
                            t!("split2.restore_tip")
                        } else {
                            t!("split2.zoom_tip")
                        }
                        .into_owned()))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.space_mut().focused = ix;
                                this.toggle_zoom(cx);
                                this.focus_editor(window, cx);
                            },
                        )),
                    )
                    .child(
                        ib(
                            ("close-pane", ix),
                            crate::shell::icons::icon("close"),
                            scale::H_COMPACT,
                            false,
                            cx,
                        )
                        .tooltip(tip(t!("pane.close").into_owned()))
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.close_pane(ix, window, cx)),
                        ),
                    ),
            )
    }
}
