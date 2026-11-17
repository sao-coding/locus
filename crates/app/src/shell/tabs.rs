//! Tabs: new / open / show / cycle / close / reorder / move to another Space.
use super::*;

impl Locus {
    // ---- tabs -------------------------------------------------------------

    pub(crate) fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let conn = self
            .active_tab(cx)
            .read(cx)
            .conn_id
            .clone()
            .or_else(|| self.space().connections.first().cloned());
        self.new_tab_on(conn, window, cx);
    }

    /// A new empty tab on `conn` (sidebar "new query here").
    pub(crate) fn new_tab_on(
        &mut self,
        conn: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = conn.as_ref().filter(|id| {
            self.conns
                .iter()
                .any(|c| &c.id == *id && c.kind == locus_core::store::DbKind::Redis)
        }) {
            self.open_redis(
                id.clone(),
                locus_core::redis::Spec::Browser { db: 0, key: None },
                window,
                cx,
            );
            return;
        }
        let data = self.blank_tab(conn, cx);
        let tab = self.make_tab(&data, window, cx);
        let s = self.space_mut();
        s.tabs.push(tab);
        s.show_alone(&data.id, cx);
        self.sync_space_conns(self.active_space, cx);
        self.focus_editor(window, cx);
        self.schedule_save(cx);
    }

    /// A new tab on `conn_id` holding `sql`, run at once (object tree double-click).
    pub(crate) fn open_query(
        &mut self,
        conn_id: String,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_query_with(conn_id, sql, Default::default(), window, cx);
    }

    /// `open_query` with the Run dialog values pre-filled (History re-run).
    pub(crate) fn open_query_with(
        &mut self,
        conn_id: String,
        sql: String,
        values: std::collections::HashMap<String, locus_core::params::ParamValue>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut data = self.blank_tab(Some(conn_id), cx);
        data.sql = sql.clone();
        let tab = self.make_tab(&data, window, cx);
        tab.update(cx, |t, _| t.bind.values = values);
        let s = self.space_mut();
        s.tabs.push(tab.clone());
        s.show_alone(&data.id, cx);
        self.sync_space_conns(self.active_space, cx);
        self.run_tab(tab, RunMode::Script, Some(sql), window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub fn show_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.recent.retain(|t| t != id);
        self.recent.insert(0, id.to_string());
        self.recent.truncate(20);
        // A page (History / Settings) takes the whole work area: it never joins a split.
        let page = self.is_page(id, cx);
        let s = self.space_mut();
        match s.panes().iter().position(|p| p == id) {
            // A pane of what is on screen: focus it (a maximised pane stays maximised).
            Some(i) => s.focused = i,
            None => {
                // Another group, or a tab outside any group: the group on screen is kept as it is.
                s.stash(cx);
                s.groups.clear_zoom();
                if page || !s.show_group_of(id, cx) {
                    s.show_alone(id, cx);
                }
            }
        }
        s.groups.set_focus(id);
        self.focus_editor(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(crate) fn cycle_tab(&mut self, d: isize, window: &mut Window, cx: &mut Context<Self>) {
        let order: Vec<String> = self
            .space()
            .ordered(cx)
            .iter()
            .map(|t| t.read(cx).id.clone())
            .collect();
        let cur = self.space().focused_tab_id();
        let i = order.iter().position(|x| x == &cur).unwrap_or(0) as isize;
        let n = order.len() as isize;
        let next = order[((i + d) % n + n) as usize % n as usize].clone();
        self.show_tab(&next, window, cx);
    }

    pub(crate) fn close_tab_now(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let si = self.active_space;
        let Some(tab) = self.spaces[si].tab(id, cx) else {
            return;
        };
        // Cancel first; dropping the tab then drops its query handle.
        tab.update(cx, |t, cx| t.cancel(cx));
        let order: Vec<String> = self.spaces[si]
            .ordered(cx)
            .iter()
            .map(|t| t.read(cx).id.clone())
            .collect();
        let pos = order.iter().position(|x| x == id).unwrap_or(0);
        self.spaces[si].tabs.retain(|t| t.read(cx).id != id);
        self.tab_subs.retain(|(t, _)| t != id);
        if self.spaces[si].tabs.is_empty() {
            let conn = self.spaces[si].connections.first().cloned();
            let data = self.blank_tab(conn, cx);
            let t = self.make_tab(&data, window, cx);
            self.spaces[si].tabs.push(t);
        }
        let remaining: Vec<String> = self.spaces[si]
            .ordered(cx)
            .iter()
            .map(|t| t.read(cx).id.clone())
            .collect();
        // Closing a pane's tab takes it out of its group first.
        self.spaces[si].remove_from_group(id);
        // Panes that showed it: close the pane if there are others, else show a neighbour.
        loop {
            let panes = self.spaces[si].panes();
            let Some(p) = panes.iter().position(|x| x == id) else {
                break;
            };
            if panes.len() > 1 {
                self.spaces[si].layout.remove_leaf(p, &mut 0);
                let n = self.spaces[si].panes().len();
                self.spaces[si].focused = self.spaces[si].focused.min(n - 1);
            } else {
                let next = remaining
                    .get(pos.min(remaining.len() - 1))
                    .cloned()
                    .unwrap();
                self.spaces[si].layout.set_leaf(p, &next);
            }
        }
        self.sync_space_conns(si, cx);
        self.focus_editor(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(crate) fn move_tab_before(&mut self, dragged: &str, target: &str, cx: &mut Context<Self>) {
        if dragged == target {
            return;
        }
        // Dragging a tab out of its group onto a row outside it leaves the split.
        let s = self.space_mut();
        if s.groups
            .group_of(dragged)
            .is_some_and(|g| !g.panes.iter().any(|p| p == target))
        {
            s.remove_from_group(dragged);
        }
        let Some(from) = s.tabs.iter().position(|t| t.read(cx).id == dragged) else {
            return;
        };
        let t = s.tabs.remove(from);
        let to = s
            .tabs
            .iter()
            .position(|t| t.read(cx).id == target)
            .unwrap_or(s.tabs.len());
        // Dropping onto a pinned tab pins, onto an unpinned one unpins.
        let pin = s.tabs.get(to).is_some_and(|x| x.read(cx).pinned);
        t.update(cx, |t, _| t.pinned = pin);
        s.tabs.insert(to, t);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(crate) fn move_tab_to_space(
        &mut self,
        id: &str,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let from = self.active_space;
        if to == from {
            return;
        }
        let Some(tab) = self.spaces[from].tab(id, cx) else {
            return;
        };
        // Keep the tab (and a running query) alive while it moves.
        self.spaces[from].tabs.retain(|t| t.read(cx).id != id);
        if let Some(c) = tab.read(cx).conn_id.clone()
            && !self.spaces[to].connections.contains(&c)
        {
            self.spaces[to].connections.push(c);
        }
        self.spaces[to].tabs.push(tab.clone());
        self.spaces[to].show_alone(id, cx);
        // Fix the panes of the old Space.
        self.spaces[from].remove_from_group(id);
        if self.spaces[from].tabs.is_empty() {
            let conn = self.spaces[from].connections.first().cloned();
            let data = self.blank_tab(conn, cx);
            let t = self.make_tab(&data, window, cx);
            self.spaces[from].tabs.push(t);
        }
        let first = self.spaces[from].ordered(cx)[0].read(cx).id.clone();
        loop {
            let panes = self.spaces[from].panes();
            let Some(p) = panes.iter().position(|x| x == id) else {
                break;
            };
            if panes.len() > 1 {
                self.spaces[from].layout.remove_leaf(p, &mut 0);
                let n = self.spaces[from].panes().len();
                self.spaces[from].focused = self.spaces[from].focused.min(n - 1);
            } else {
                self.spaces[from].layout.set_leaf(p, &first);
            }
        }
        self.sync_space_conns(from, cx);
        self.sync_space_conns(to, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Close, asking first when the tab has unsaved changes.
    pub(crate) fn close_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.space().tab(id, cx) else {
            return;
        };
        if tab.read(cx).tx.needs_prompt() {
            let id = id.to_string();
            let then: crate::prod::Cont =
                std::rc::Rc::new(move |wb, window, cx| wb.close_tab(&id, window, cx));
            self.confirm_end_tx(vec![tab], then, window, cx);
            return;
        }
        let (modified, empty, title) = {
            let t = tab.read(cx);
            (
                t.doc.modified,
                t.auto_title.is_empty() && t.doc.path.is_none(),
                t.display_title(cx),
            )
        };
        if !modified || empty {
            self.close_tab_now(id, window, cx);
            return;
        }
        let me = cx.entity().downgrade();
        let id = id.to_string();
        window.open_pop_dialog(cx, move |dialog, _, _| {
            let (m1, m2, i1, i2) = (me.clone(), me.clone(), id.clone(), id.clone());
            dialog
                .title(t!("file.close_title", name = title.clone()).to_string())
                .w(px(scale::W_DLG_S))
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .whitespace_normal()
                                .child(t!("file.close_body").to_string()),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().flex_1())
                                .child(
                                    Button::new("close-discard")
                                        .label(t!("file.dont_save").to_string())
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let _ = m1.update(cx, |this, cx| {
                                                this.close_tab_now(&i1, window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("close-save")
                                        .primary()
                                        .label(t!("file.save").to_string())
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let _ = m2.update(cx, |this, cx| {
                                                if let Some(tab) = this.space().tab(&i2, cx) {
                                                    this.save_tab(tab, false, true, window, cx);
                                                }
                                            });
                                        }),
                                ),
                        ),
                )
        });
    }
}
