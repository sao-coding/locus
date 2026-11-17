//! Redis tabs in the shell: opening (one workspace tab per connection and DB, plus console / info
//! tabs), building the views of a restored tab, Production unlock requests, the card header.
use super::console::Console;
use super::info::InfoPage;
use super::workspace::RedisTab;
use super::*;
use locus_core::redis::Spec;
use locus_core::spaces::TabPage;
use locus_core::store::DbKind;

/// The DB a workspace spec stands for (`None` for console / info tabs).
fn workspace_db(s: &Spec) -> Option<u8> {
    match s {
        Spec::Browser { db, .. } | Spec::Key { db, .. } => Some(*db),
        _ => None,
    }
}

impl Locus {
    /// The tab of this connection and spec (a workspace tab matches by DB), wherever it lives.
    fn find_redis_tab(&self, conn_id: &str, spec: &Spec, cx: &App) -> Option<(usize, String)> {
        for (si, s) in self.spaces.iter().enumerate() {
            for t in &s.tabs {
                let t = t.read(cx);
                if t.pageslot.kind != Some(TabPage::Redis) || t.conn_id.as_deref() != Some(conn_id)
                {
                    continue;
                }
                let Some(other) = Spec::parse(&t.editor.read(cx).state.read(cx).value()) else {
                    continue;
                };
                let same = match (workspace_db(spec), workspace_db(&other)) {
                    (Some(a), Some(b)) => a == b,
                    _ => other == *spec,
                };
                if same {
                    return Some((si, t.id.clone()));
                }
            }
        }
        None
    }

    fn redis_title(&self, conn_id: &str, spec: &Spec) -> String {
        let name = self
            .conns
            .iter()
            .find(|c| c.id == conn_id)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        match spec {
            Spec::Browser { db, .. } | Spec::Key { db, .. } => format!("{name} · DB {db}"),
            Spec::Console { .. } => t!("redis.tab_console", name = name).into_owned(),
            Spec::Info => t!("redis.tab_info", name = name).into_owned(),
        }
    }

    /// The workspace view of an open tab, if it is one.
    fn redis_workspace(&self, tab_id: &str, cx: &App) -> Option<Entity<RedisTab>> {
        for s in &self.spaces {
            for t in &s.tabs {
                let t = t.read(cx);
                if t.id == tab_id {
                    return t
                        .pageslot
                        .view
                        .clone()
                        .and_then(|v| v.downcast::<RedisTab>().ok());
                }
            }
        }
        None
    }

    /// Opens (or shows) a Redis tab and adds the connection to the Space when needed.
    /// A key spec selects the key in the workspace tab of its DB.
    pub(crate) fn open_redis(
        &mut self,
        conn_id: String,
        spec: Spec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .conns
            .iter()
            .any(|c| c.id == conn_id && c.kind == DbKind::Redis)
        {
            return;
        }
        self.ensure_conn_in_space(&conn_id, cx);
        if let Some((si, id)) = self.find_redis_tab(&conn_id, &spec, cx) {
            if si != self.active_space {
                self.switch_space(si, window, cx);
            }
            self.show_tab(&id, window, cx);
            if let (Spec::Key { key, .. } | Spec::Browser { key: Some(key), .. }, Some(ws)) =
                (&spec, self.redis_workspace(&id, cx))
            {
                let key = key.clone();
                ws.update(cx, |w, cx| w.open_key(key, window, cx));
            }
            return;
        }
        let spec = match spec {
            Spec::Key { db, key } => Spec::Browser { db, key: Some(key) },
            s => s,
        };
        let mut data = self.blank_tab(Some(conn_id.clone()), cx);
        data.page = Some(TabPage::Redis);
        data.title = Some(self.redis_title(&conn_id, &spec));
        data.sql = spec.to_json();
        let tab = self.make_tab(&data, window, cx);
        self.space_mut().tabs.push(tab);
        self.sync_space_conns(self.active_space, cx);
        self.show_tab(&data.id, window, cx);
        self.schedule_save(cx);
    }

    fn redis_ctx(&mut self, conn_id: &str) -> Result<Ctx, String> {
        let c = self
            .conns
            .iter()
            .find(|c| c.id == conn_id && c.kind == DbKind::Redis)
            .ok_or_else(|| t!("redis.conn_missing").into_owned())?;
        let ro = self.pool.ro_flag(conn_id);
        let password = self
            .store
            .password(conn_id)
            .map_err(|e| t!("error.credential", msg = e.to_string()).into_owned())?
            .unwrap_or_default();
        Ok(Ctx {
            conn_id: c.id.clone(),
            name: c.name.clone(),
            cfg: locus_core::redis::config(c, password),
            rt: self.pool.rt().clone(),
            ro,
        })
    }

    /// Creates the view of a Redis tab (called from `attach_page`). Old saved key tabs become the
    /// workspace of their DB with that key selected.
    pub(crate) fn attach_redis(
        &mut self,
        tab: &Entity<QueryTab>,
        data: &TabData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let conn_id = data.conn_id.clone().unwrap_or_default();
        let spec = Spec::parse(&data.sql);
        let tab_id = data.id.clone();
        let (view, focus, events): (AnyView, FocusHandle, Option<Subscription>) =
            match (self.redis_ctx(&conn_id), spec) {
                (Ok(ctx), Some(s @ (Spec::Browser { .. } | Spec::Key { .. }))) => {
                    let (db, key) = match s {
                        Spec::Browser { db, key } => (db, key),
                        Spec::Key { db, key } => (db, Some(key)),
                        _ => (0, None),
                    };
                    let v = cx.new(|cx| RedisTab::new(ctx.with_db(db), key, window, cx));
                    let f = v.read(cx).focus_handle(cx);
                    let s = cx.subscribe_in(&v, window, {
                        let (id, conn) = (tab_id.clone(), conn_id.clone());
                        move |this, ws, ev: &RedisEvent, window, cx| {
                            this.handle_redis_event(&conn, &id, ws, ev, window, cx)
                        }
                    });
                    (v.into(), f, Some(s))
                }
                (Ok(ctx), Some(Spec::Console { db })) => {
                    let v = cx.new(|cx| Console::new(ctx.with_db(db), window, cx));
                    let f = v.read(cx).focus_handle(cx);
                    let s = cx.subscribe_in(&v, window, {
                        let (id, conn) = (tab_id.clone(), conn_id.clone());
                        move |this, _, ev: &RedisEvent, window, cx| {
                            if let RedisEvent::Locked = ev {
                                this.redis_locked(&conn, &id, window, cx);
                            }
                        }
                    });
                    (v.into(), f, Some(s))
                }
                (Ok(ctx), Some(Spec::Info)) => {
                    let v = cx.new(|cx| InfoPage::new(ctx, cx));
                    let f = v.read(cx).focus_handle(cx);
                    (v.into(), f, None)
                }
                (Err(e), _) => {
                    let v = cx.new(|cx| InfoPage::failed(e, cx));
                    let f = v.read(cx).focus_handle(cx);
                    (v.into(), f, None)
                }
                (_, None) => {
                    let v = cx.new(|cx| InfoPage::failed(t!("redis.bad_tab").into_owned(), cx));
                    let f = v.read(cx).focus_handle(cx);
                    (v.into(), f, None)
                }
            };
        if let Some(s) = events {
            self.tab_subs.push((data.id.clone(), s));
        }
        tab.update(cx, |t, _| {
            t.pageslot.view = Some(view);
            t.pageslot.focus = Some(focus);
        });
    }

    fn redis_locked(
        &mut self,
        conn_id: &str,
        _tab_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::shell::toast::show(t!("redis.blocked_prod").to_string(), cx);
        self.open_unlock(conn_id.to_string(), None, window, cx);
    }

    /// Writes the workspace's DB and selected key into the tab's spec (so a restart restores them).
    fn save_redis_spec(
        &mut self,
        tab_id: &str,
        spec: Spec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let json = spec.to_json();
        let tabs: Vec<Entity<QueryTab>> = self
            .spaces
            .iter()
            .flat_map(|s| s.tabs.iter().cloned())
            .collect();
        for t in tabs {
            if t.read(cx).id != tab_id {
                continue;
            }
            let state = t.read(cx).editor.read(cx).state.clone();
            state.update(cx, |s, cx| s.set_value(json.clone(), window, cx));
            t.update(cx, |t, _| {
                t.doc.baseline = json.clone();
                t.doc.dirty = false;
                t.doc.modified = false;
            });
        }
        self.schedule_save(cx);
    }

    fn handle_redis_event(
        &mut self,
        conn_id: &str,
        tab_id: &str,
        ws: &Entity<RedisTab>,
        ev: &RedisEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            RedisEvent::Locked => self.redis_locked(conn_id, tab_id, window, cx),
            RedisEvent::Status => {
                let (db, keys) = ws.read_with(cx, |w, cx| (w.db(), w.key_count(cx)));
                set_health(conn_id, |h| {
                    h.db = db;
                    h.keys = keys;
                });
                cx.notify();
            }
            RedisEvent::Selected(_) | RedisEvent::DbChanged(_) => {
                let (db, key) = ws.read_with(cx, |w, _| (w.db(), w.key()));
                self.save_redis_spec(tab_id, Spec::Browser { db, key }, window, cx);
                cx.notify();
            }
            _ => {}
        }
    }

    /// Card header of a Redis tab: icon, title and the connection line.
    pub(crate) fn render_redis_header(
        &self,
        tab: &Entity<QueryTab>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let tr = tab.read(cx);
        let title = tr.display_title(cx);
        let conn = tr
            .conn_id
            .as_ref()
            .and_then(|id| self.conns.iter().find(|c| &c.id == id))
            .cloned();
        let hint = conn
            .as_ref()
            .map(|c| format!("{} · {}", c.name, c.subtitle()))
            .unwrap_or_default();
        h_flex()
            .id("card-header")
            .flex_none()
            .h(px(H_TAB + S2))
            .pl(px(S6))
            .pr(px(S5))
            .gap(px(S5))
            .items_center()
            .border_b_1()
            .border_color(t.ln)
            .child(self.sidebar_toggle(cx))
            .child(
                crate::shell::style::kind_icon(crate::shell::theme::TabKind::Table)
                    .size(px(14.))
                    .text_color(t.tx2),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(420.))
                    .truncate()
                    .text_size(px(TX_MD))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.tx1)
                    .child(title),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(TX_SM))
                    .text_color(t.tx3)
                    .child(hint),
            )
            .when_some(
                conn.filter(|c| c.env != locus_core::store::Env::Dev),
                |h, c| {
                    let (bg, fg) = crate::shell::theme::env_colors(c.env, cx);
                    h.child(
                        div()
                            .flex_none()
                            .px(px(S3))
                            .h(px(16.))
                            .flex()
                            .items_center()
                            .rounded(px(R_XS))
                            .bg(bg)
                            .text_color(fg)
                            .text_size(px(10.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(c.env.badge()),
                    )
                },
            )
            .into_any_element()
    }

    /// Redis entries of the connection menu.
    pub(crate) fn redis_menu_items(
        menu: PopupMenu,
        cid: &str,
        me: &WeakEntity<Locus>,
    ) -> PopupMenu {
        use crate::shell::menu::item;
        let mut menu = menu;
        for (label, icon, spec) in [
            (
                t!("redis.menu_browse").into_owned(),
                "key-round",
                Spec::Browser { db: 0, key: None },
            ),
            (
                t!("redis.menu_console").into_owned(),
                "query",
                Spec::Console { db: 0 },
            ),
            (t!("redis.menu_info").into_owned(), "analysis", Spec::Info),
        ] {
            let (me, cid) = (me.clone(), cid.to_string());
            menu = menu.item(item(label, Some(icon), move |window, cx| {
                let _ = me.update(cx, |this, cx| {
                    this.open_redis(cid.clone(), spec.clone(), window, cx)
                });
            }));
        }
        menu
    }
}
