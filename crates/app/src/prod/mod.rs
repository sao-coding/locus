//! Production protection, the app side: which Production connections are
//! unlocked (in memory only), the gate every SQL-sending path goes through,
//! the unlock / blocked / confirm dialogs, the header chip and the bar.
//! The decisions themselves are `locus_core::guard` (pure, unit-tested); the log is
//! `locus_core::audit`.
use crate::shell::scale::*;
use crate::shell::seg::{SegItem, seg};
use crate::shell::*;
use db_core::classify::{Classified, classify, classify_script};
use gpui_kit::component::Disableable as _;
use locus_core::audit;
use locus_core::guard::{self, Decision, UnlockMode, WriteInfo};
use locus_core::store::{DbKind, Env};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

/// What to do once the gate lets a run through.
pub(crate) type Cont = Rc<dyn Fn(&mut Locus, &mut Window, &mut Context<Locus>)>;

/// One attempt to send SQL on a connection.
#[derive(Clone)]
pub(crate) struct Gated {
    pub tab: Option<Entity<QueryTab>>,
    pub conn: SavedConnection,
    pub classified: Vec<Classified>,
    /// The SQL as the dialogs and the audit log show it.
    pub preview: String,
    pub go: Cont,
}

struct Unlock {
    mode: UnlockMode,
    until: Option<Instant>,
    /// "Next run only": the write was sent; lock again when that run ends.
    spent: bool,
}

#[derive(Default)]
pub(crate) struct ProdState {
    unlocked: HashMap<String, Unlock>,
    ticker: Option<Task<()>>,
    ticker_alive: bool,
    /// The MCP confirmation shown now (its request id).
    pub(crate) mcp_open: Option<u64>,
}

fn alpha(mut c: Hsla, a: f32) -> Hsla {
    c.a = a;
    c
}

fn engine_label(kind: DbKind) -> &'static str {
    match kind {
        DbKind::Mysql => "MySQL / MariaDB",
        DbKind::Postgres => "PostgreSQL",
        DbKind::Oracle => "Oracle",
        DbKind::SqlServer => "SQL Server",
        DbKind::Sqlite => "SQLite",
        DbKind::Redis => "Redis",
    }
}

fn env_str(env: Env) -> &'static str {
    match env {
        Env::Local => "local",
        Env::Dev => "dev",
        Env::Staging => "staging",
        Env::Team => "team",
        Env::Prod => "prod",
    }
}

/// "DELETE" or "DELETE, UPDATE".
fn keywords_text(i: &WriteInfo) -> String {
    i.keywords.join(", ")
}

impl Locus {
    // ---- lock state --------------------------------------------------------

    /// Production and not unlocked.
    pub(crate) fn conn_locked(&self, c: &SavedConnection) -> bool {
        c.env == Env::Prod && !self.prod.unlocked.contains_key(&c.id)
    }

    /// Every Production connection starts locked: its sessions are read-only
    /// from the first connect.
    pub(crate) fn init_prod_locks(&mut self) {
        let ids: Vec<String> = self
            .conns
            .iter()
            .filter(|c| c.env == Env::Prod)
            .map(|c| c.id.clone())
            .collect();
        for id in ids {
            self.pool
                .ro_flag(&id)
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Puts the connection's open sessions (and ones still connecting) in the
    /// mode its lock state asks for.
    fn sync_ro(&mut self, id: &str) {
        let on = self
            .conns
            .iter()
            .find(|c| c.id == id)
            .is_some_and(|c| self.conn_locked(c));
        self.pool.set_read_only(id, on);
    }

    /// The connection was edited, closed or deleted: lock it again (it is
    /// reconnected on the next run) and recompute its read-only mode.
    pub(crate) fn prod_conn_closed(&mut self, id: &str, cx: &mut Context<Self>) {
        self.relock(id, "disconnect", cx);
        self.sync_ro(id);
    }

    pub(crate) fn relock(&mut self, id: &str, cause: &str, cx: &mut Context<Self>) {
        if self.prod.unlocked.remove(id).is_none() {
            return;
        }
        if let Some(c) = self.conns.iter().find(|c| c.id == id) {
            let mut e = audit::Entry::new(&c.name, env_str(c.env), "relock");
            e.cause = Some(cause);
            audit::append(&e);
        }
        self.sync_ro(id);
        cx.notify();
    }

    pub(crate) fn lock_all_prod(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<String> = self.prod.unlocked.keys().cloned().collect();
        for id in ids {
            self.relock(&id, "lock_all", cx);
        }
    }

    fn do_unlock(&mut self, id: &str, mode: UnlockMode, reason: &str, cx: &mut Context<Self>) {
        let Some(c) = self.conns.iter().find(|c| c.id == id).cloned() else {
            return;
        };
        let until = (mode == UnlockMode::Timed).then(|| Instant::now() + guard::unlock_duration());
        let dur = match mode {
            UnlockMode::Timed => format!("{}s", guard::unlock_duration().as_secs()),
            UnlockMode::NextRun => "next_run".into(),
            UnlockMode::UntilDisconnect => "until_disconnect".into(),
        };
        let mut e = audit::Entry::new(&c.name, env_str(c.env), "unlock");
        e.reason = Some(reason);
        e.duration = Some(&dur);
        audit::append(&e);
        self.prod.unlocked.insert(
            id.to_string(),
            Unlock {
                mode,
                until,
                spent: false,
            },
        );
        self.sync_ro(id);
        self.ensure_ticker(cx);
        cx.notify();
    }

    /// A confirmed write was handed to the run: "next run only" ends with it.
    fn mark_sent(&mut self, id: &str) {
        if let Some(u) = self.prod.unlocked.get_mut(id)
            && u.mode == UnlockMode::NextRun
        {
            u.spent = true;
        }
    }

    /// A run or Apply ended on `conn_id`.
    pub(crate) fn after_run(&mut self, conn_id: &str, cx: &mut Context<Self>) {
        if self.prod.unlocked.get(conn_id).is_some_and(|u| u.spent) {
            self.relock(conn_id, "next_run_used", cx);
        }
    }

    /// One timer task while any timed unlock exists: it sleeps until the next
    /// deadline (or 30 s, to refresh the minutes shown) and does nothing else.
    fn ensure_ticker(&mut self, cx: &mut Context<Self>) {
        if self.prod.ticker_alive || !self.prod.unlocked.values().any(|u| u.until.is_some()) {
            return;
        }
        self.prod.ticker_alive = true;
        self.prod.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                let wait = this.update(cx, |this, cx| this.prod_tick(cx));
                match wait {
                    Ok(Some(d)) => cx.background_executor().timer(d).await,
                    _ => break,
                }
            }
        }));
    }

    fn prod_tick(&mut self, cx: &mut Context<Self>) -> Option<Duration> {
        let now = Instant::now();
        let expired: Vec<String> = self
            .prod
            .unlocked
            .iter()
            .filter(|(_, u)| u.until.is_some_and(|t| t <= now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.relock(&id, "expired", cx);
        }
        let next = self.prod.unlocked.values().filter_map(|u| u.until).min();
        let Some(next) = next else {
            self.prod.ticker_alive = false;
            cx.notify();
            return None;
        };
        cx.notify();
        Some(
            next.saturating_duration_since(now)
                .min(Duration::from_secs(30))
                .max(Duration::from_millis(50)),
        )
    }

    fn secs_left(&self, id: &str) -> Option<u64> {
        let t = self.prod.unlocked.get(id)?.until?;
        Some(t.saturating_duration_since(Instant::now()).as_secs())
    }

    // ---- the gate ----------------------------------------------------------

    /// Entry of every path that sends SQL on a connection.
    pub(crate) fn gate(&mut self, g: Gated, window: &mut Window, cx: &mut Context<Self>) {
        // A read-only SQLite file refuses every write: say so before any confirm dialog.
        if g.conn.kind == locus_core::store::DbKind::Sqlite
            && g.conn.read_only
            && guard::WriteInfo::of(&g.classified).is_some()
        {
            crate::shell::toast::show(t!("wavea.sqlite_ro_blocked").to_string(), cx);
            return;
        }
        let confirm = crate::settings::get(cx).confirm_nonprod();
        let locked = self.conn_locked(&g.conn);
        match guard::decide(g.conn.env, locked, &g.classified, confirm) {
            Decision::Send => (g.go.clone())(self, window, cx),
            Decision::Blocked(info) => {
                self.audit_write(&g.conn, "blocked", &info, &g.preview);
                self.open_blocked(g, info, window, cx);
            }
            Decision::Confirm { info, type_keyword } => {
                self.open_confirm(g, info, type_keyword, window, cx)
            }
        }
    }

    /// Like `gate`, for an action the user already confirmed in its own dialog (the activity
    /// page's kill): a lock still blocks it, but the second Production confirmation is
    /// skipped. The send is audited as a confirmed one.
    pub(crate) fn gate_confirmed(&mut self, g: Gated, window: &mut Window, cx: &mut Context<Self>) {
        let confirm = crate::settings::get(cx).confirm_nonprod();
        let locked = self.conn_locked(&g.conn);
        if let Decision::Confirm { info, .. } =
            guard::decide(g.conn.env, locked, &g.classified, confirm)
        {
            self.audit_write(&g.conn, "confirmed_sent", &info, &g.preview);
            (g.go.clone())(self, window, cx);
            return;
        }
        self.gate(g, window, cx);
    }

    fn audit_write(&self, c: &SavedConnection, event: &str, info: &WriteInfo, sql: &str) {
        let mut e = audit::Entry::new(&c.name, env_str(c.env), event);
        e.keywords = info.keywords.clone();
        e.sql = Some(sql);
        audit::append(&e);
    }

    /// Apply of staged grid edits.
    pub(crate) fn request_apply(
        &mut self,
        tab: Entity<QueryTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stmts = tab.read(cx).staged_statements(cx);
        if stmts.is_empty() {
            return;
        }
        let go: Cont = {
            let tab = tab.clone();
            Rc::new(move |_, window, cx| tab.update(cx, |t, cx| t.apply_edits(window, cx)))
        };
        let conn = tab
            .read(cx)
            .conn_id
            .as_ref()
            .and_then(|id| self.conns.iter().find(|c| &c.id == id))
            .cloned();
        let Some(conn) = conn else {
            return go(self, window, cx);
        };
        let classified: Vec<Classified> = stmts.iter().map(|s| classify(&s.sql)).collect();
        let preview = crate::query::tab::preview_text(&stmts, tab.read(cx).tx.manual());
        self.gate(
            Gated {
                tab: Some(tab),
                conn,
                classified,
                preview,
                go,
            },
            window,
            cx,
        );
    }

    /// Classifies `sql` for a run.
    pub(crate) fn classify_run(sql: &str) -> Vec<Classified> {
        classify_script(sql)
    }

    // ---- dialogs -----------------------------------------------------------

    fn first_non_prod(&self) -> Option<String> {
        self.space()
            .connections
            .iter()
            .filter_map(|id| self.conns.iter().find(|c| &c.id == id))
            .find(|c| c.env != Env::Prod)
            .map(|c| c.id.clone())
    }

    /// Opens a copy of the tab on another connection (not run).
    fn duplicate_tab_to(
        &mut self,
        tab: &Entity<QueryTab>,
        conn_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = tab
            .read(cx)
            .editor
            .read(cx)
            .state
            .read(cx)
            .value()
            .to_string();
        let mut data = self.blank_tab(Some(conn_id), cx);
        data.sql = text;
        let new = self.make_tab(&data, window, cx);
        let s = self.space_mut();
        s.tabs.push(new);
        s.show_alone(&data.id, cx);
        self.sync_space_conns(self.active_space, cx);
        self.focus_editor(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    fn open_blocked(
        &mut self,
        g: Gated,
        info: WriteInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let me = cx.entity().downgrade();
        let alt = self.first_non_prod();
        window.open_pop_dialog(cx, move |dialog, _, cx| {
            let t = crate::shell::theme::tokens(cx);
            let kw = keywords_text(&info);
            let body = t!("prod.blocked_body", kw = kw, name = g.conn.name.clone()).into_owned();
            let (me1, me2, me3) = (me.clone(), me.clone(), me.clone());
            let (g1, g2) = (g.clone(), g.clone());
            let mut buttons = h_flex().gap(px(S4)).child(div().flex_1()).child(
                Button::new("blk-cancel")
                    .label(t!("common.cancel").to_string())
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            );
            if let (Some(alt), Some(_)) = (alt.clone(), g.tab.as_ref()) {
                buttons = buttons.child(
                    Button::new("blk-elsewhere")
                        .label(t!("prod.run_elsewhere").to_string())
                        .on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            let (alt, Some(tab)) = (alt.clone(), g1.tab.clone()) else {
                                return;
                            };
                            let _ = me1.update(cx, |this, cx| {
                                this.duplicate_tab_to(&tab, alt, window, cx)
                            });
                        }),
                );
            }
            buttons = buttons.child(danger_button(
                "blk-unlock",
                t!("prod.unlock_more").to_string(),
                true,
                t,
                move |_, window, cx| {
                    window.close_dialog(cx);
                    let g = g2.clone();
                    let then: Cont = Rc::new(move |wb, window, cx| wb.gate(g.clone(), window, cx));
                    let id = g2.conn.id.clone();
                    let _ = me2.update(cx, |this, cx| this.open_unlock(id, Some(then), window, cx));
                },
            ));
            let _ = &me3;
            dialog
                .title(t!("prod.blocked_title").to_string())
                .w(px(W_DLG_M))
                .child(
                    v_flex()
                        .gap(px(S5))
                        .child(warn_box("lock", body, t, cx))
                        .child(sql_preview(&g.preview, t, cx))
                        .child(buttons),
                )
        });
    }

    fn open_confirm(
        &mut self,
        g: Gated,
        info: WriteInfo,
        type_keyword: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let me = cx.entity().downgrade();
        let db = g.conn.database.clone().unwrap_or_else(|| "—".into());
        let maria = self
            .conn_version(&g.conn.id)
            .is_some_and(|v| v.contains("MariaDB"));
        let typed = type_keyword
            .as_ref()
            .map(|_| cx.new(|cx| InputState::new(window, cx)));
        let first = Cell::new(true);
        let keyword = type_keyword.clone().unwrap_or_default();
        let prod = g.conn.env == Env::Prod;
        window.open_pop_dialog(cx, move |dialog, window, cx| {
            let t = crate::shell::theme::tokens(cx);
            if first.replace(false)
                && let Some(i) = &typed
            {
                i.read(cx).focus_handle(cx).focus(window, cx);
            }
            let kw_shown = info.keyword().to_string();
            let target = confirm_target(&g, maria, &db, cx);
            let mut body = v_flex().gap_3().child(target);
            if info.count > 1 || info.keywords.len() > 1 {
                body = body.child(
                    div().text_sm().child(
                        t!(
                            "prod.confirm_multi",
                            n = info.count.to_string(),
                            kws = keywords_text(&info)
                        )
                        .into_owned(),
                    ),
                );
            }
            if info.missing_where {
                body = body.child(warn_box(
                    "warn",
                    t!("prod.confirm_no_where").into_owned(),
                    t,
                    cx,
                ));
            }
            body = body.child(sql_preview(&g.preview, t, cx));
            if prod {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(t.tx3)
                        .child(t!("gate.fn_note").into_owned()),
                );
            }
            let ok = match &typed {
                Some(i) => guard::keyword_ok(&i.read(cx).value(), &keyword),
                None => true,
            };
            if let Some(i) = &typed {
                body = body.child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .text_color(t.tx3)
                                .child(t!("prod.confirm_type", kw = keyword.clone()).into_owned()),
                        )
                        .child(Input::new(i).px(px(crate::shell::scale::S5))),
                );
            }
            let run_label = t!("prod.confirm_btn", kw = kw_shown.clone()).to_string();
            let run_click = confirm_run_click(me.clone(), g.clone(), info.clone());
            let enter = run_click.clone();
            let run_btn = run_click.clone();
            let run: AnyElement = if prod {
                danger_button("confirm-run", run_label, ok, t, move |e, w, cx| {
                    run_btn(e, w, cx)
                })
                .into_any_element()
            } else {
                Button::new("confirm-run")
                    .h(px(H_ROW))
                    .primary()
                    .label(run_label)
                    .disabled(!ok)
                    .on_click(move |e, w, cx| run_btn(e, w, cx))
                    .into_any_element()
            };
            body = body.child(
                h_flex()
                    .gap(px(S4))
                    .child(div().flex_1())
                    .child(
                        Button::new("confirm-cancel")
                            .h(px(H_ROW))
                            .label(t!("common.cancel").to_string())
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(run),
            );
            // Enter activates the primary button (once its guard is satisfied), never just closes.
            dialog
                .title(t!("prod.confirm_title", kw = kw_shown).to_string())
                .w(px(W_DLG_M))
                .child(body)
                .on_ok(move |e, w, cx| {
                    if ok {
                        enter(e, w, cx);
                    }
                    false
                })
        });
    }

    /// A Production write requested by an MCP client: Allow runs it, Deny / closing /
    /// 60 s without an answer refuses it. The `slot` answers the waiting request;
    /// dropping it (dialog gone) is a Deny.
    pub(crate) fn open_mcp_confirm(
        &mut self,
        conn: SavedConnection,
        client: String,
        sql: String,
        info: WriteInfo,
        slot: Rc<std::cell::RefCell<crate::mcp::Slot>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = slot.borrow().id;
        self.prod.mcp_open = Some(id);
        let db = conn.database.clone().unwrap_or_else(|| "—".into());
        let maria = self
            .conn_version(&conn.id)
            .is_some_and(|v| v.contains("MariaDB"));
        let g = Gated {
            tab: None,
            conn,
            classified: Vec::new(),
            preview: sql,
            go: Rc::new(|_, _, _| {}),
        };
        let secs = locus_mcp::tools::CONFIRM_TIMEOUT.as_secs();
        window.open_pop_dialog(cx, move |dialog, _, cx| {
            let t = crate::shell::theme::tokens(cx);
            let answer = |slot: &Rc<std::cell::RefCell<crate::mcp::Slot>>, yes: bool| {
                if let Some(tx) = slot.borrow_mut().reply.take() {
                    let _ = tx.send(yes);
                }
            };
            let mut body = v_flex()
                .gap(px(S5))
                .child(confirm_target(&g, maria, &db, cx))
                .child(warn_box(
                    "warn",
                    t!(
                        "mcp.confirm_body",
                        client = client.clone(),
                        n = info.count.to_string(),
                        kws = keywords_text(&info)
                    )
                    .into_owned(),
                    t,
                    cx,
                ));
            if info.missing_where {
                body = body.child(warn_box(
                    "warn",
                    t!("prod.confirm_no_where").into_owned(),
                    t,
                    cx,
                ));
            }
            let (s1, s2) = (slot.clone(), slot.clone());
            body = body
                .child(sql_preview(&g.preview, t, cx))
                .child(
                    div()
                        .text_xs()
                        .text_color(t.tx3)
                        .child(t!("mcp.confirm_timeout", n = secs.to_string()).into_owned()),
                )
                .child(
                    h_flex()
                        .gap(px(S4))
                        .child(div().flex_1())
                        .child(
                            Button::new("mcp-deny")
                                .h(px(H_ROW))
                                .label(t!("mcp.deny").into_owned())
                                .on_click(move |_, window, cx| {
                                    answer(&s1, false);
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(danger_button(
                            "mcp-allow",
                            t!("mcp.allow").into_owned(),
                            true,
                            t,
                            move |_, window, cx| {
                                answer(&s2, true);
                                window.close_dialog(cx);
                            },
                        )),
                );
            dialog
                .title(t!("mcp.confirm_title").into_owned())
                .w(px(W_DLG_M))
                .child(body)
        });
    }

    /// The unlock dialog. `then` runs after a successful unlock.
    pub(crate) fn open_unlock(
        &mut self,
        conn_id: String,
        then: Option<Cont>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(c) = self.conns.iter().find(|c| c.id == conn_id).cloned() else {
            return;
        };
        let engine = self
            .conn_version(&c.id)
            .unwrap_or_else(|| engine_label(c.kind).to_string());
        let host = format!("{}:{}", c.host, c.port);
        let name_in = cx.new(|cx| InputState::new(window, cx));
        let reason_in =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("prod.reason_ph").to_string()));
        let mode = Rc::new(Cell::new(UnlockMode::Timed));
        let first = Cell::new(true);
        let me = cx.entity().downgrade();
        let secs = guard::unlock_duration().as_secs();
        let timed_label = match secs {
            60 => t!("prod.dur_minute"),
            s if s >= 60 => t!("prod.dur_minutes", n = (s / 60).to_string()),
            s => t!("prod.dur_seconds", n = s.to_string()),
        }
        .into_owned();
        window.open_pop_dialog(cx, move |dialog, window, cx| {
            let t = crate::shell::theme::tokens(cx);
            if first.replace(false) {
                reason_in.read(cx).focus_handle(cx).focus(window, cx);
            }
            let typed = name_in.read(cx).value().to_string();
            let reason = reason_in.read(cx).value().to_string();
            let ok = guard::unlock_ok(&typed, &c.name, &reason);
            let warn = t!(
                "prod.unlock_warn",
                name = c.name.clone(),
                engine = engine.clone(),
                host = host.clone()
            )
            .into_owned();
            let mut segs = Vec::new();
            for (i, (m, label)) in [
                (UnlockMode::Timed, timed_label.clone()),
                (UnlockMode::NextRun, t!("prod.dur_next").into_owned()),
                (
                    UnlockMode::UntilDisconnect,
                    t!("prod.dur_disc").into_owned(),
                ),
            ]
            .into_iter()
            .enumerate()
            {
                let cell = mode.clone();
                segs.push(SegItem::new(
                    SharedString::from(format!("unlock-dur-{i}")),
                    label,
                    mode.get() == m,
                    Rc::new(move |_, _, cx| {
                        cell.set(m);
                        cx.refresh_windows();
                    }),
                ));
            }
            let seg = seg(segs, cx);
            let (me, id, then, mode, reason2) = (
                me.clone(),
                c.id.clone(),
                then.clone(),
                mode.clone(),
                reason.clone(),
            );
            let field = |label: String, child: AnyElement| {
                v_flex()
                    .gap(px(S3))
                    .child(div().text_size(px(TX_SM)).text_color(t.tx2).child(label))
                    .child(child)
            };
            dialog
                .title(t!("prod.unlock_title").to_string())
                .w(px(W_DLG_M))
                .child(
                    v_flex()
                        .gap(px(S5))
                        .child(warn_box("warn", warn, t, cx))
                        .child(field(t!("prod.unlock_for").into_owned(), seg))
                        .child(field(
                            t!("prod.reason").into_owned(),
                            Input::new(&reason_in)
                                .px(px(crate::shell::scale::S5))
                                .into_any_element(),
                        ))
                        .child(field(
                            t!("prod.type_name", name = c.name.clone()).into_owned(),
                            Input::new(&name_in)
                                .px(px(crate::shell::scale::S5))
                                .into_any_element(),
                        ))
                        .child(
                            h_flex()
                                .gap(px(S4))
                                .child(div().flex_1())
                                .child(
                                    Button::new("unlock-cancel")
                                        .label(t!("common.cancel").to_string())
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                )
                                .child(danger_button(
                                    "unlock-ok",
                                    t!("prod.unlock_btn").to_string(),
                                    ok,
                                    t,
                                    move |_, window, cx| {
                                        window.close_dialog(cx);
                                        let (id, then, m, reason) =
                                            (id.clone(), then.clone(), mode.get(), reason2.clone());
                                        let _ = me.update(cx, |this, cx| {
                                            this.do_unlock(&id, m, reason.trim(), cx);
                                            if let Some(then) = then {
                                                then(this, window, cx);
                                            }
                                        });
                                    },
                                )),
                        ),
                )
        });
    }

    /// Chip / bar action: unlock dialog when locked, lock now when unlocked.
    pub(crate) fn toggle_prod_lock(
        &mut self,
        conn_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(c) = self.conns.iter().find(|c| c.id == conn_id).cloned() else {
            return;
        };
        if c.env != Env::Prod {
            return;
        }
        if self.conn_locked(&c) {
            self.open_unlock(conn_id.to_string(), None, window, cx);
        } else {
            self.relock(conn_id, "manual", cx);
        }
    }

    // ---- header chip and bar -------------------------------------------------

    fn chip_text(&self, c: &SavedConnection) -> String {
        if c.kind == locus_core::store::DbKind::Sqlite && c.read_only {
            return t!("wavea.chip_ro").into_owned();
        }
        if c.env != Env::Prod {
            return t!("prod.chip_plain").into_owned();
        }
        match self.prod.unlocked.get(&c.id) {
            None => t!("prod.chip_ro").into_owned(),
            Some(u) => match u.mode {
                UnlockMode::Timed => t!(
                    "prod.chip_rw_min",
                    n = guard::minutes_left(self.secs_left(&c.id).unwrap_or(0)).to_string()
                )
                .into_owned(),
                UnlockMode::NextRun => t!("prod.chip_rw_next").into_owned(),
                UnlockMode::UntilDisconnect => t!("prod.chip_rw").into_owned(),
            },
        }
    }

    pub(crate) fn render_mode_chip(
        &self,
        c: &SavedConnection,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let prod = c.env == Env::Prod;
        let locked = self.conn_locked(c);
        let sqlite_ro = c.kind == locus_core::store::DbKind::Sqlite && c.read_only;
        let (bg, fg, icon) = if sqlite_ro {
            (t.on, t.tx3, "lock")
        } else if !prod {
            (t.on, t.tx3, "pencil")
        } else if locked {
            (alpha(t.red, 0.14), t.red_text, "lock")
        } else {
            (alpha(t.yel, 0.14), t.yel_text, "unlock")
        };
        let id = c.id.clone();
        h_flex()
            .id("mode-chip")
            .flex_none()
            .h(px(22.))
            .px(px(9.))
            .gap(px(6.))
            .items_center()
            .rounded(px(11.))
            .bg(bg)
            .text_color(fg)
            .text_size(px(11.5))
            .font_weight(FontWeight::MEDIUM)
            .whitespace_nowrap()
            .when(prod, |d| {
                d.cursor_pointer()
                    .tooltip(tip(if locked {
                        t!("prod.chip_tip_locked")
                    } else {
                        t!("prod.chip_tip_unlocked")
                    }
                    .into_owned()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_prod_lock(&id, window, cx)
                    }))
            })
            .child(crate::shell::icons::icon(icon).size(px(12.)))
            .child(self.chip_text(c))
            .into_any_element()
    }

    /// The 28 px bar under the header of a Production tab.
    pub(crate) fn render_prodbar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = self.active_conn(cx).filter(|c| c.env == Env::Prod)?.clone();
        let t = crate::shell::theme::tokens(cx);
        let locked = self.conn_locked(&c);
        let (bg, fg) = if locked {
            (alpha(t.red, 0.10), t.red_text)
        } else {
            (alpha(t.yel, 0.10), t.yel_text)
        };
        let id = c.id.clone();
        Some(
            h_flex()
                .id("prodbar")
                .flex_none()
                .h(px(crate::shell::scale::H_BAR))
                .px(px(crate::shell::scale::S6))
                .gap(px(crate::shell::scale::S4))
                .items_center()
                .bg(bg)
                .text_color(fg)
                .text_size(px(crate::shell::scale::TX_SM))
                .child(
                    crate::shell::icons::icon(if locked { "lock" } else { "unlock" }).size(px(12.)),
                )
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t!("prod.bar_name").into_owned()),
                )
                .child(
                    div().flex_1().min_w_0().truncate().opacity(0.85).child(
                        if locked {
                            t!("prod.bar_locked")
                        } else {
                            t!("prod.bar_unlocked")
                        }
                        .into_owned(),
                    ),
                )
                .child(
                    div()
                        .id("prodbar-link")
                        .flex_none()
                        .h(px(crate::shell::scale::H_COMPACT))
                        .px(px(crate::shell::scale::S4))
                        .flex()
                        .items_center()
                        .rounded(px(crate::shell::scale::R_SM))
                        .border_1()
                        .border_color(fg)
                        .cursor_pointer()
                        .hover(|s| s.bg(t.hv))
                        .font_weight(FontWeight::MEDIUM)
                        .child(
                            if locked {
                                t!("prod.bar_unlock")
                            } else {
                                t!("prod.bar_lock")
                            }
                            .into_owned(),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.toggle_prod_lock(&id, window, cx)
                        })),
                )
                .into_any_element(),
        )
    }
}

/// Red warning box (`.warn` in the mockup).
fn warn_box(icon: &str, text: String, t: &crate::shell::theme::Tokens, _cx: &App) -> Div {
    h_flex()
        .gap(px(S5))
        .items_start()
        .px(px(S5))
        .py(px(S5))
        .rounded(px(R_MD))
        .border_1()
        .border_color(alpha(t.red, 0.3))
        .bg(alpha(t.red, 0.1))
        .text_color(t.tx)
        .child(
            div()
                .flex_none()
                .mt(px(2.))
                .text_color(t.red_text)
                .child(crate::shell::icons::icon(icon).size(px(14.))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .text_size(px(TX_BASE))
                .child(text),
        )
}

/// Danger action button (audit D4): `red` fill with white text (4.6 : 1 on both themes);
/// disabled = `hv` fill with `tx4` text. 28 px high.
fn danger_button(
    id: &'static str,
    label: String,
    enabled: bool,
    t: &crate::shell::theme::Tokens,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let (bg, fg, hover) = (t.red, gpui_kit::white(), t.red.opacity(0.88));
    div()
        .id(id)
        .flex_none()
        .h(px(H_CONTROL))
        .px(px(S5))
        .flex()
        .items_center()
        .rounded(px(R_SM))
        .text_size(px(TX_SM))
        .font_weight(FontWeight::MEDIUM)
        .when(enabled, |d| {
            d.bg(bg)
                .text_color(fg)
                .cursor_pointer()
                .hover(|h| h.bg(hover))
                .on_click(on_click)
        })
        .when(!enabled, |d| d.bg(t.hv).text_color(t.tx4))
        .child(label)
}

/// SQL preview (`.sqlp`): monospace, first 40 lines.
fn sql_preview(sql: &str, t: &crate::shell::theme::Tokens, cx: &App) -> Stateful<Div> {
    let mut lines: Vec<String> = sql
        .lines()
        .take(40)
        .map(|l| {
            if l.is_empty() {
                " ".to_string()
            } else {
                l.chars().take(400).collect()
            }
        })
        .collect();
    if sql.lines().count() > 40 {
        lines.push("…".into());
    }
    v_flex()
        .id("sqlp")
        .max_h(px(150.))
        .overflow_y_scroll()
        .px(px(S5))
        .py(px(S4))
        .rounded(px(R_MD))
        .border_1()
        .border_color(t.ln)
        .bg(t.hv)
        .text_size(px(12.))
        .font_family(cx.theme().mono_font_family.clone())
        .children(
            lines
                .into_iter()
                .map(|l| div().whitespace_normal().child(l)),
        )
}

/// Engine badge (same look as the connection list's).
fn engine_badge(kind: DbKind, maria: bool, cx: &App) -> Div {
    let (label, color) = crate::shell::theme::db_badge(kind, maria, cx);
    let (bg, fg) = crate::shell::theme::badge_colors(color, cx);
    div()
        .flex_none()
        .w(px(20.))
        .h(px(18.))
        .rounded(px(5.))
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .text_color(fg)
        .text_size(px(9.5))
        .font_weight(FontWeight::SEMIBOLD)
        .font_family(cx.theme().mono_font_family.clone())
        .child(label)
}

/// "Target: engine badge, connection name, environment, database" row of the confirm dialog.
fn confirm_target(g: &Gated, maria: bool, db: &str, cx: &App) -> Div {
    let t = crate::shell::theme::tokens(cx);
    let (ebg, efg) = crate::shell::theme::env_colors(g.conn.env, cx);
    h_flex()
        .gap(px(S4))
        .text_size(px(TX_SM))
        .items_center()
        .child(
            div()
                .text_color(t.tx3)
                .child(t!("prod.confirm_target").into_owned()),
        )
        .child(engine_badge(g.conn.kind, maria, cx))
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(g.conn.name.clone()),
        )
        .child(
            div()
                .px(px(S3))
                .py(px(1.))
                .rounded(px(4.))
                .bg(ebg)
                .text_color(efg)
                .text_size(px(TX_XS))
                .font_weight(FontWeight::SEMIBOLD)
                .child(g.conn.env.badge()),
        )
        .child(div().text_color(t.tx3).child(format!("· {db}")))
}

/// The confirm button: decides again when the lock came back, audits and runs the write.
fn confirm_run_click(
    me: WeakEntity<Locus>,
    g: Gated,
    info: WriteInfo,
) -> Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)> {
    let info2 = info;
    Rc::new(move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
        window.close_dialog(cx);
        let (g, info) = (g.clone(), info2.clone());
        let _ = me.update(cx, |this, cx| {
            // The lock may have come back while the dialog was open
            // (time is up, or Lock now): decide again.
            if g.conn.env == Env::Prod && this.conn_locked(&g.conn) {
                return this.gate(g, window, cx);
            }
            this.audit_write(&g.conn, "confirmed_sent", &info, &g.preview);
            this.mark_sent(&g.conn.id);
            (g.go.clone())(this, window, cx);
        });
    })
}
