//! Connections of a Space: add / remove / move, connection dialog, delete, object-tree connect.
use crate::shell::scale::*;
use crate::shell::*;

impl Locus {
    // ---- connections ------------------------------------------------------

    pub(crate) fn move_conn_to_space(&mut self, conn: &str, to: usize, cx: &mut Context<Self>) {
        let from = self.active_space;
        if from == to {
            return;
        }
        self.spaces[from].connections.retain(|c| c != conn);
        if !self.spaces[to].connections.iter().any(|c| c == conn) {
            self.spaces[to].connections.push(conn.to_string());
        }
        self.sync_space_conns(from, cx);
        self.sync_space_conns(to, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(crate) fn add_conn_to_space(&mut self, conn: &str, cx: &mut Context<Self>) {
        let si = self.active_space;
        if !self.spaces[si].connections.iter().any(|c| c == conn) {
            self.spaces[si].connections.push(conn.to_string());
        }
        self.sync_space_conns(si, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(crate) fn remove_conn_from_space(&mut self, conn: &str, cx: &mut Context<Self>) {
        let si = self.active_space;
        let open: Vec<_> = self.spaces[si]
            .tabs
            .iter()
            .filter(|t| t.read(cx).tx.needs_prompt() && t.read(cx).conn_id.as_deref() == Some(conn))
            .cloned()
            .collect();
        if !open.is_empty() {
            let c = conn.to_string();
            let then: crate::prod::Cont =
                std::rc::Rc::new(move |wb, _, cx| wb.remove_conn_from_space(&c, cx));
            self.confirm_end_tx_later(open, then, cx);
            return;
        }
        self.spaces[si].connections.retain(|c| c != conn);
        self.sync_space_conns(si, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Clicking a connection in the sidebar: the tab in front uses it.
    pub(crate) fn use_connection(&mut self, id: String, cx: &mut Context<Self>) {
        let tab = self.active_tab(cx);
        self.set_tab_connection(tab, Some(id), cx);
    }

    pub(crate) fn open_form(
        &mut self,
        editing: Option<SavedConnection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let has_pw = editing
            .as_ref()
            .is_some_and(|c| self.store.has_password(&c.id));
        let rt = self.pool.rt().clone();
        let is_new = editing.is_none();
        let form = cx.new(|cx| ConnForm::new(editing, has_pw, rt, window, cx));
        if is_new {
            let here = this_space_conns(self);
            let others: Vec<SavedConnection> = self
                .conns
                .iter()
                .filter(|c| !here.contains(&c.id))
                .cloned()
                .collect();
            form.update(cx, |f, _| f.existing = others);
        }
        let sub = cx.subscribe_in(
            &form,
            window,
            |this, form, ev: &ConnFormEvent, window, cx| match ev {
                ConnFormEvent::Saved(c, pw) => {
                    if let Err(e) = this.store.set_password(c, pw.as_deref()) {
                        let msg = t!("error.credential", msg = e.to_string()).into_owned();
                        form.update(cx, |f, cx| f.set_error(msg, cx));
                        return;
                    }
                    let mut list = this.conns.clone();
                    let new = !list.iter().any(|x| x.id == c.id);
                    match list.iter_mut().find(|x| x.id == c.id) {
                        Some(x) => *x = c.clone(),
                        None => list.push(c.clone()),
                    }
                    if let Err(e) = this.store.save_list(&list) {
                        let msg = t!("error.save", msg = e.to_string()).into_owned();
                        form.update(cx, |f, cx| f.set_error(msg, cx));
                        return;
                    }
                    this.conns = list;
                    this.sync_mcp_conns(cx);
                    // Settings changed: the next Run opens a new connection.
                    this.pool.retire(&c.id);
                    this.prod_conn_closed(&c.id, cx);
                    for t in this
                        .spaces
                        .iter()
                        .flat_map(|s| s.tabs.iter())
                        .cloned()
                        .collect::<Vec<_>>()
                    {
                        if t.read(cx).conn_id.as_deref() == Some(&c.id) {
                            t.update(cx, |t, cx| t.abandon(cx));
                        }
                    }
                    if new {
                        this.add_conn_to_space(&c.id, cx);
                        let tab = this.active_tab(cx);
                        if tab.read(cx).conn_id.is_none() {
                            this.set_tab_connection(tab, Some(c.id.clone()), cx);
                        }
                    }
                    for i in 0..this.spaces.len() {
                        this.sync_space_conns(i, cx);
                    }
                    this.form = None;
                    window.close_dialog(cx);
                    cx.notify();
                }
                ConnFormEvent::Deleted(id) => {
                    this.delete_connection(id, cx);
                    this.form = None;
                    window.close_dialog(cx);
                    cx.notify();
                }
                ConnFormEvent::UseExisting(id) => {
                    this.add_conn_to_space(id, cx);
                    let tab = this.active_tab(cx);
                    if tab.read(cx).conn_id.is_none() {
                        this.set_tab_connection(tab, Some(id.clone()), cx);
                    }
                    this.form = None;
                    window.close_dialog(cx);
                    cx.notify();
                }
                ConnFormEvent::Cancelled => {
                    this.form = None;
                    window.close_dialog(cx);
                }
                ConnFormEvent::Test(c, pw) => {
                    // Password left empty while one is stored: test with the stored one.
                    let pw = match pw {
                        Some(p) => Ok(p.clone()),
                        None => this
                            .store
                            .password(&c.id)
                            .map(|p| p.unwrap_or_default())
                            .map_err(|e| e.to_string()),
                    };
                    form.update(cx, |f, cx| f.run_test(c.clone(), pw, cx));
                }
            },
        );
        self._subs.push(sub);
        self.form = Some(form.clone());
        let first = std::cell::Cell::new(true);
        window.open_pop_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                let h = form.read(cx).first_focus(cx);
                h.focus(window, cx);
            }
            let title = if form.read(cx).is_edit() {
                t!("conn.edit_title")
            } else {
                t!("conn.add_title")
            };
            dialog
                .title(title.to_string())
                .w(px(W_DLG_L))
                .child(form.clone())
                // Enter saves (the form decides whether it can); the dialog stays open either way.
                .on_ok({
                    let form = form.clone();
                    move |_, _, cx| {
                        form.update(cx, |f, cx| f.save(cx));
                        false
                    }
                })
        });
    }

    /// Opens a connection for the object tree (no query runs).
    pub(crate) fn connect_for_tree(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(conn) = self.conns.iter().find(|c| c.id == id).cloned() else {
            return;
        };
        if conn.kind == locus_core::store::DbKind::Redis {
            let msg = t!("redis.use_browser").into_owned();
            self.objects
                .update(cx, |o, cx| o.connect_failed(&id, msg, cx));
            return;
        }
        let password = match self.store.password(&id) {
            Ok(p) => p.unwrap_or_default(),
            Err(e) => {
                let msg = t!("error.credential", msg = e.to_string()).into_owned();
                self.objects
                    .update(cx, |o, cx| o.connect_failed(&id, msg, cx));
                return;
            }
        };
        let slot = self.pool.slot(&id);
        let ro = self.pool.ro_flag(&id);
        let rt = self.pool.rt().clone();
        cx.spawn(async move |this, cx| {
            let res = {
                let mut g = slot.lock().await;
                if g.is_some() {
                    Ok(())
                } else {
                    match locus_core::driver::AnyDriver::connect(&rt, &conn, password, ro).await {
                        Ok(d) => {
                            *g = Some(Arc::new(d));
                            Ok(())
                        }
                        Err(e) => Err(e),
                    }
                }
            };
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(()) => this.sync_objects(cx),
                    Err(e) => {
                        let msg =
                            format!("{}: {}", locus_core::status::error_title(e.kind), e.message);
                        this.objects
                            .update(cx, |o, cx| o.connect_failed(&id, msg, cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Hands the object tree the active Space's connections and their open drivers.
    pub(crate) fn sync_objects(&self, cx: &mut App) {
        let list = self
            .space()
            .connections
            .iter()
            .filter_map(|id| self.conns.iter().find(|c| &c.id == id))
            .map(|c| (c.id.clone(), c.name.clone(), self.pool.open_driver(&c.id)))
            .collect();
        self.objects.update(cx, |o, cx| o.set_conns(list, cx));
    }
}

// ---- delete connection -------------------------------------------------------

impl Locus {
    /// Removes a saved connection, its password, and its use in every Space and tab.
    pub(crate) fn delete_connection(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Err(e) = self.store.delete(id) {
            log::warn!("credential for connection {id} could not be deleted: {e}");
            crate::shell::toast::show(
                t!("dataerr.cred_delete", msg = e.to_string()).to_string(),
                cx,
            );
        }
        let list: Vec<_> = self.conns.iter().filter(|c| c.id != id).cloned().collect();
        if let Err(e) = self.store.save_list(&list) {
            log_error(&t!("error.save", msg = e.to_string()));
            return;
        }
        self.conns = list;
        self.sync_mcp_conns(cx);
        self.pool.retire(id);
        self.prod_conn_closed(id, cx);
        for s in &mut self.spaces {
            s.connections.retain(|c| c != id);
        }
        for t in self
            .spaces
            .iter()
            .flat_map(|s| s.tabs.iter())
            .cloned()
            .collect::<Vec<_>>()
        {
            if t.read(cx).conn_id.as_deref() == Some(id) {
                t.update(cx, |t, cx| {
                    t.abandon(cx);
                    t.conn_id = None;
                });
            }
        }
        for i in 0..self.spaces.len() {
            self.sync_space_conns(i, cx);
        }
        self.schedule_save(cx);
        cx.notify();
    }

    pub(crate) fn confirm_delete_connection(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self
            .conns
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone())
        else {
            return;
        };
        let me = cx.entity().downgrade();
        window.open_pop_dialog(cx, move |dialog, _, _| {
            let me = me.clone();
            let id = id.clone();
            dialog
                .title(t!("conn.delete_title", name = name.clone()).to_string())
                .w(px(W_DLG_S))
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .whitespace_normal()
                                .child(t!("conn.delete_body").to_string()),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().flex_1())
                                .child(
                                    Button::new("del-conn-cancel")
                                        .label(t!("common.cancel").to_string())
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                )
                                .child(
                                    Button::new("del-conn-ok")
                                        .danger()
                                        .label(t!("conn.delete_confirm").to_string())
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let _ = me.update(cx, |this, cx| {
                                                this.delete_connection(&id, cx)
                                            });
                                        }),
                                ),
                        ),
                )
        });
    }
}

pub(crate) fn log_error(msg: &str) {
    log::warn!("{msg}");
    eprintln!("{msg}");
}

impl Locus {
    /// A connection's accent: its chosen colour, else one derived from its id.
    pub(crate) fn conn_color(&self, id: &str, cx: &App) -> Hsla {
        self.conns
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| {
                c.color
                    .as_deref()
                    .and_then(crate::connections::form::parse_hex)
            })
            .unwrap_or_else(|| space_color(conn_hue(id), cx))
    }
}

/// Ids of the connections the active Space already lists.
fn this_space_conns(wb: &Locus) -> Vec<String> {
    wb.spaces[wb.active_space].connections.clone()
}
