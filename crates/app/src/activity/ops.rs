//! Server activity tabs in the shell: opening (one per connection), the view
//! of a restored tab, the visibility timer, the kill confirm and the gate.
use super::*;
use locus_core::spaces::TabPage;

impl Locus {
    /// Opens (or shows) the Server activity tab of a connection.
    pub(crate) fn open_activity(
        &mut self,
        conn_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.conns.iter().find(|c| c.id == conn_id).cloned() else {
            return;
        };
        if Engine::of(conn.kind).is_none() {
            return;
        }
        self.ensure_conn_in_space(&conn_id, cx);
        for (si, s) in self.spaces.iter().enumerate() {
            for t in &s.tabs {
                let t = t.read(cx);
                if t.pageslot.kind == Some(TabPage::Activity)
                    && t.conn_id.as_deref() == Some(conn_id.as_str())
                {
                    let id = t.id.clone();
                    if si != self.active_space {
                        self.switch_space(si, window, cx);
                    }
                    self.show_tab(&id, window, cx);
                    return;
                }
            }
        }
        let mut data = self.blank_tab(Some(conn_id), cx);
        data.page = Some(TabPage::Activity);
        data.title = Some(t!("act.tab_title", name = conn.name.clone()).into_owned());
        let tab = self.make_tab(&data, window, cx);
        self.space_mut().tabs.push(tab);
        self.sync_space_conns(self.active_space, cx);
        self.show_tab(&data.id, window, cx);
        self.schedule_save(cx);
    }

    /// Creates the view of an activity tab (called from `attach_page`).
    pub(crate) fn attach_activity(
        &mut self,
        tab: &Entity<QueryTab>,
        data: &TabData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let conn_id = data.conn_id.clone().unwrap_or_default();
        let conn = self.conns.iter().find(|c| c.id == conn_id).cloned();
        let Some((conn, engine)) = conn.and_then(|c| Engine::of(c.kind).map(|e| (c, e))) else {
            let view = cx.new(|cx| {
                crate::redis::info::InfoPage::failed(t!("act.conn_missing").into_owned(), cx)
            });
            let focus = view.read(cx).focus_handle(cx);
            tab.update(cx, |t, _| {
                t.pageslot.view = Some(view.into());
                t.pageslot.focus = Some(focus);
            });
            return;
        };
        let password = self
            .store
            .password(&conn_id)
            .ok()
            .flatten()
            .unwrap_or_default();
        let ro = self.pool.ro_flag(&conn_id);
        let rt = self.pool.rt().clone();
        let redis = (engine == Engine::Redis).then(|| crate::redis::Ctx {
            conn_id: conn.id.clone(),
            name: conn.name.clone(),
            cfg: locus_core::redis::config(&conn, password.clone()),
            rt: rt.clone(),
            ro: ro.clone(),
        });
        let src = Source {
            engine,
            conn,
            password,
            slot: self.pool.slot(&conn_id),
            ro,
            rt,
            redis,
        };
        let view = cx.new(|cx| ActivityPage::new(src, window, cx));
        let focus = view.read(cx).focus_handle(cx);
        let tab_id = data.id.clone();
        let sub = cx.subscribe_in(&view, window, {
            let (tab, view) = (tab.clone(), view.clone());
            move |this, _, ev: &ActivityEvent, window, cx| {
                this.handle_activity_event(&tab, &view, ev, window, cx)
            }
        });
        self.tab_subs.push((data.id.clone(), sub));
        // Once a second: tell the page whether it is on screen (hidden = no refresh).
        let weak = view.downgrade();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Some(page) = weak.upgrade() else { break };
                let alive = this.update(cx, |wb, cx| {
                    let visible = wb.space().panes().contains(&tab_id);
                    page.update(cx, |p, cx| p.tick(visible, cx));
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        tab.update(cx, |t, _| {
            t.pageslot.view = Some(view.into());
            t.pageslot.focus = Some(focus);
        });
    }

    fn handle_activity_event(
        &mut self,
        tab: &Entity<QueryTab>,
        view: &Entity<ActivityPage>,
        ev: &ActivityEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conn_id) = tab.read(cx).conn_id.clone() else {
            return;
        };
        match ev {
            ActivityEvent::OpenSql(sql) => {
                self.open_sql_tab(conn_id, sql.clone(), false, window, cx)
            }
            ActivityEvent::Kill { session, mode, cmd } => self.confirm_kill(
                tab.clone(),
                view.clone(),
                conn_id,
                session.clone(),
                *mode,
                cmd.clone(),
                window,
                cx,
            ),
        }
    }

    /// Confirm (Oracle: type the session), then the Production gate, then run.
    #[allow(clippy::too_many_arguments)]
    fn confirm_kill(
        &mut self,
        tab: Entity<QueryTab>,
        view: Entity<ActivityPage>,
        conn_id: String,
        s: Session,
        mode: KillMode,
        cmd: KillCmd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.conns.iter().find(|c| c.id == conn_id).cloned() else {
            return;
        };
        let me = cx.entity().downgrade();
        let typed_name = (conn.kind == locus_core::store::DbKind::Oracle).then(|| s.label());
        let typed = cx.new(|cx| InputState::new(window, cx));
        let first = std::cell::Cell::new(true);
        let title = if mode == KillMode::Query {
            t!("act.confirm_query")
        } else {
            t!("act.confirm_session")
        }
        .to_string();
        let body = t!(
            "act.confirm_body",
            id = s.label(),
            user = s.user.clone(),
            host = s.host.clone(),
            sql = act::one_line(&s.sql, 300)
        )
        .to_string();
        window.open_pop_dialog(cx, move |dialog, window, cx| {
            let t = crate::shell::theme::tokens(cx);
            if first.replace(false) && typed_name.is_some() {
                typed.read(cx).focus_handle(cx).focus(window, cx);
            }
            let ok = typed_name
                .as_ref()
                .is_none_or(|n| typed.read(cx).value().trim() == n);
            let go: Rc<dyn Fn(&mut Window, &mut App)> = {
                let (me, tab, view, cmd, conn) = (
                    me.clone(),
                    tab.clone(),
                    view.clone(),
                    cmd.clone(),
                    conn.clone(),
                );
                Rc::new(move |window, cx| {
                    window.close_dialog(cx);
                    let (tab, view, cmd, conn) =
                        (tab.clone(), view.clone(), cmd.clone(), conn.clone());
                    let _ = me.update(cx, |this, cx| {
                        this.gate_kill(tab, view, conn, cmd, window, cx)
                    });
                })
            };
            let (go_click, go_enter) = (go.clone(), go);
            dialog
                .title(title.clone())
                .w(px(W_DLG_M))
                .child(
                    v_flex()
                        .gap(px(S5))
                        .child(
                            div()
                                .text_size(px(TX_SM))
                                .whitespace_normal()
                                .child(body.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(TX_SM))
                                .text_color(t.tx3)
                                .whitespace_normal()
                                .child(cmd.text()),
                        )
                        .children(typed_name.as_ref().map(|n| {
                            v_flex()
                                .gap(px(S3))
                                .child(
                                    div()
                                        .text_size(px(TX_SM))
                                        .text_color(t.red_text)
                                        .child(t!("act.oracle_type", name = n.clone()).to_string()),
                                )
                                .child(Input::new(&typed).px(px(S5)))
                        }))
                        .child(
                            h_flex()
                                .gap(px(S4))
                                .child(div().flex_1())
                                .child(
                                    Button::new("kill-cancel")
                                        .h(px(H_ROW))
                                        .label(t!("common.cancel").to_string())
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                )
                                .child(
                                    Button::new("kill-go")
                                        .h(px(H_ROW))
                                        .danger()
                                        .disabled(!ok)
                                        .label(t!("act.kill_go").to_string())
                                        .on_click(move |_, window, cx| go_click(window, cx)),
                                ),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    if ok {
                        go_enter(window, cx);
                    }
                    false
                })
        });
    }

    fn gate_kill(
        &mut self,
        tab: Entity<QueryTab>,
        view: Entity<ActivityPage>,
        conn: locus_core::store::SavedConnection,
        cmd: KillCmd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The activity dialog already confirmed; the gate only adds the lock and the audit.
        // A kill changes server state whatever its text says (a SELECT of pg_terminate_backend).
        let mut c = db_core::classify::classify("KILL 1");
        c.writes = true;
        c.keyword = "KILL".into();
        let preview = cmd.text();
        let go: crate::prod::Cont =
            Rc::new(move |_, _, cx| view.update(cx, |p, cx| p.run_kill(cmd.clone(), cx)));
        self.gate_confirmed(
            crate::prod::Gated {
                tab: Some(tab),
                conn,
                classified: vec![c],
                preview,
                go,
            },
            window,
            cx,
        );
    }

    /// Menu entry of a connection (not SQLite).
    pub(crate) fn activity_menu_item(
        menu: PopupMenu,
        cid: &str,
        me: &WeakEntity<Locus>,
    ) -> PopupMenu {
        use crate::shell::menu::item;
        let (me, cid) = (me.clone(), cid.to_string());
        menu.item(item(
            t!("act.menu").into_owned(),
            Some("analysis"),
            move |window, cx| {
                let _ = me.update(cx, |this, cx| this.open_activity(cid.clone(), window, cx));
            },
        ))
    }
}
