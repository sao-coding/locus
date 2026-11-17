//! Split groups (Strata v3) on screen: the rules live in `locus_core::groups`;
//! this keeps the visible layout tree and the groups in step, and holds the
//! actions (join / leave / replace / dissolve / maximise / close the group).
use super::*;
use locus_core::groups::{Groups, MAX_PANES, default_layout};

impl Space {
    /// The focused pane is maximised (temporary, never saved).
    pub(crate) fn zoomed(&self) -> bool {
        self.panes().len() > 1 && self.groups.zoomed(&self.focused_tab_id())
    }

    /// Keeps the on-screen layout in its group before another tab is shown.
    pub(crate) fn stash(&mut self, cx: &App) {
        let leaves = self.panes();
        if leaves.len() < 2 {
            return;
        }
        let data = self.layout.data(cx);
        let focus = self.focused_tab_id();
        if let Some(g) = self.groups.group_of_mut(&leaves[0]) {
            g.layout = Some(data);
            g.focus = focus;
            g.zoom = false;
        }
    }

    /// Shows `id` on its own (a new tab, a page, a tab outside any group).
    pub(crate) fn show_alone(&mut self, id: &str, cx: &App) {
        self.stash(cx);
        self.layout = Layout::Tab(id.to_string());
        self.focused = 0;
    }

    /// Shows the group `id` belongs to, focused on `id`. False when it has none.
    pub(crate) fn show_group_of(&mut self, id: &str, cx: &mut App) -> bool {
        let Some(g) = self
            .groups
            .group_of(id)
            .filter(|g| g.panes.len() >= 2)
            .cloned()
        else {
            return false;
        };
        let same = |l: &LayoutData| {
            let mut v = Vec::new();
            locus_core::groups::collect_leaves(l, &mut v);
            let (mut a, mut b) = (v.clone(), g.panes.clone());
            a.sort();
            b.sort();
            a == b && v.len() == g.panes.len()
        };
        let data = g
            .layout
            .clone()
            .filter(same)
            .unwrap_or_else(|| default_layout(&g.panes, SplitAxis::Row));
        self.layout = Layout::from_data(&data, cx);
        self.focused = self.panes().iter().position(|p| p == id).unwrap_or(0);
        true
    }

    /// The layout tree changed: make the groups say the same. Panes beyond the cap leave the screen.
    pub(crate) fn sync_groups(&mut self) {
        let leaves = self.panes();
        let focus = self.focused_tab_id();
        let extra = self.groups.sync_visible(&leaves, &focus);
        for x in extra {
            if let Some(ix) = self.panes().iter().position(|p| *p == x) {
                self.layout.remove_leaf(ix, &mut 0);
            }
        }
        let n = self.panes().len();
        self.focused = self.focused.min(n.saturating_sub(1));
    }

    /// Takes `id` out of its group (the tab stays). Focus goes to a remaining pane.
    pub(crate) fn remove_from_group(&mut self, id: &str) -> bool {
        let leaves = self.panes();
        let focused = self.focused_tab_id();
        let Some(r) = self.groups.remove(id) else {
            return false;
        };
        if leaves.len() > 1
            && let Some(ix) = leaves.iter().position(|p| p == id)
        {
            self.layout.remove_leaf(ix, &mut 0);
            let now = self.panes();
            let want = if focused == id { r.focus } else { focused };
            self.focused = now.iter().position(|p| *p == want).unwrap_or(0);
        }
        true
    }

    /// Ends group `gid`, keeping its tabs. On screen it leaves the focused tab alone.
    pub(crate) fn dissolve_group(&mut self, gid: &str) {
        let focused = self.focused_tab_id();
        let panes = self.groups.dissolve(gid);
        if panes.contains(&focused) && self.panes().len() > 1 {
            self.layout = Layout::Tab(focused);
            self.focused = 0;
        }
    }

    /// Groups as saved: the visible one with its live layout.
    pub(crate) fn groups_data(&self, cx: &App) -> Groups {
        let mut g = self.groups.clone();
        let leaves = self.panes();
        if leaves.len() > 1
            && let Some(x) = g.group_of_mut(&leaves[0])
        {
            x.layout = Some(self.layout.data(cx));
        }
        g
    }
}

impl Locus {
    /// After the layout tree changed through a split / drop: groups follow, joined tabs are unpinned.
    pub(crate) fn after_layout_change(&mut self, cx: &mut Context<Self>) {
        self.space_mut().sync_groups();
        let panes = self.space().panes();
        if panes.len() > 1 {
            for id in &panes {
                if let Some(t) = self.space().tab(id, cx) {
                    t.update(cx, |t, _| t.pinned = false);
                }
            }
        }
    }

    pub(crate) fn toggle_zoom(&mut self, cx: &mut Context<Self>) {
        let id = self.space().focused_tab_id();
        self.space_mut().groups.toggle_zoom(&id);
        cx.notify();
    }

    /// Restores a maximised pane; false when nothing was maximised.
    pub(crate) fn leave_zoom(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.space().zoomed() {
            return false;
        }
        self.space_mut().groups.clear_zoom();
        cx.notify();
        true
    }

    /// Menu / × / drag out: the tab leaves its group and stays open.
    pub(crate) fn unsplit_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.space_mut().remove_from_group(id) {
            self.focus_editor(window, cx);
            self.schedule_save(cx);
            cx.notify();
        }
    }

    pub(crate) fn dissolve_group_ui(
        &mut self,
        gid: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.space_mut().dissolve_group(gid);
        self.focus_editor(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Focused pane's group: dissolve it (palette).
    pub(crate) fn unsplit_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.space().focused_tab_id();
        if let Some(g) = self.space().groups.group_of(&id).map(|g| g.id.clone()) {
            self.dissolve_group_ui(&g, window, cx);
        }
    }

    /// A new query in a new pane on the right (palette: new split).
    pub(crate) fn new_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let anchor = self.space().focused_tab_id();
        if self.is_page(&anchor, cx) {
            return;
        }
        let conn = self.active_tab(cx).read(cx).conn_id.clone();
        let data = self.blank_tab(conn, cx);
        let t = self.make_tab(&data, window, cx);
        self.space_mut().tabs.push(t);
        self.sync_space_conns(self.active_space, cx);
        self.split(Some(data.id), SplitAxis::Row, false, window, cx);
    }

    pub(crate) fn toggle_group_open(&mut self, gid: &str, cx: &mut Context<Self>) {
        if let Some(g) = self.space_mut().groups.get_mut(gid) {
            g.open = !g.open;
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// "Close the whole group": every tab of it.
    pub(crate) fn close_group(&mut self, gid: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(g) = self.space().groups.get(gid).cloned() else {
            return;
        };
        for id in g.panes {
            self.close_tab(&id, window, cx);
        }
    }

    /// The tab does not fit (4 panes): replace the focused pane, or open it alone.
    pub(crate) fn group_full_dialog(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.space().tab(id, cx) else {
            return;
        };
        let name = tab.read(cx).display_title(cx);
        let me = cx.entity().downgrade();
        let id = id.to_string();
        window.open_pop_dialog(cx, move |dialog, _, _| {
            let (m1, m2, i1, i2) = (me.clone(), me.clone(), id.clone(), id.clone());
            dialog
                .title(t!("split2.full_title", n = MAX_PANES.to_string()).to_string())
                .w(px(scale::W_DLG_S))
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div().text_sm().whitespace_normal().child(
                                t!("split2.full_body", n = MAX_PANES.to_string()).to_string(),
                            ),
                        )
                        .child(
                            div()
                                .text_sm()
                                .whitespace_normal()
                                .child(t!("split2.full_ask", name = name.clone()).to_string()),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().flex_1())
                                .child(
                                    Button::new("split-new-group")
                                        .label(t!("split2.new_group").to_string())
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let _ = m1.update(cx, |this, cx| {
                                                this.space_mut().remove_from_group(&i1);
                                                this.show_tab(&i1, window, cx);
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("split-replace")
                                        .primary()
                                        .label(t!("split2.replace").to_string())
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let _ = m2.update(cx, |this, cx| {
                                                this.replace_focused_pane(&i2, window, cx)
                                            });
                                        }),
                                ),
                        ),
                )
        });
    }

    /// The focused pane shows `id` instead; the old tab stays open outside the group.
    pub(crate) fn replace_focused_pane(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old = self.space().focused_tab_id();
        if old == id {
            return;
        }
        let s = self.space_mut();
        // `id` leaves any other group (the dialog only opens for a tab outside this one).
        s.groups.replace(&old, id).ok();
        let f = s.focused;
        s.layout.set_leaf(f, id);
        self.after_layout_change(cx);
        self.focus_editor(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }
}
