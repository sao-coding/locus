//! Server activity page tab (one per connection): sessions with auto-refresh
//! and kill, server variables and status counters, read-only. The SQL and the
//! row models live in `locus_core::activity`.
use crate::shell::scale::*;
use crate::shell::seg::{SegItem, seg};
use crate::shell::*;
use gpui_kit::component::Disableable as _;
use locus_core::activity::{self as act, Engine, KillCmd, KillMode, Session, SortKey};
use locus_core::pool::Slot;
use locus_core::store::SavedConnection;
use std::rc::Rc;
use std::time::{Duration, Instant};

mod ops;

/// What the page asks the shell for.
pub(crate) enum ActivityEvent {
    /// Kill: the shell confirms, runs the Production gate, then calls `run_kill`.
    Kill {
        session: Session,
        mode: KillMode,
        cmd: KillCmd,
    },
    /// Open the statement in a new query tab.
    OpenSql(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Sessions,
    Variables,
    Status,
}

/// Everything the page needs to read from one connection.
#[derive(Clone)]
pub(crate) struct Source {
    pub engine: Engine,
    pub conn: SavedConnection,
    pub password: String,
    pub slot: Slot,
    pub ro: Arc<std::sync::atomic::AtomicBool>,
    pub rt: tokio::runtime::Handle,
    pub redis: Option<crate::redis::Ctx>,
}

enum Loaded {
    Sessions(Vec<Session>),
    Pairs(Vec<(String, String)>),
}

async fn driver_of(src: &Source) -> Result<Arc<locus_core::driver::AnyDriver>, String> {
    let mut g = src.slot.lock().await;
    if let Some(d) = g.as_ref() {
        return Ok(d.clone());
    }
    let d = locus_core::driver::AnyDriver::connect(
        &src.rt,
        &src.conn,
        src.password.clone(),
        src.ro.clone(),
    )
    .await
    .map_err(|e| format!("{}: {}", locus_core::status::error_title(e.kind), e.message))?;
    let d = Arc::new(d);
    *g = Some(d.clone());
    Ok(d)
}

async fn load(src: Source, section: Section) -> Result<Loaded, String> {
    let rt = src.rt.clone();
    // The tokio runtime drives the sockets; the UI executor only awaits the join handle.
    rt.spawn(async move {
        if let Some(rc) = src.redis.clone() {
            return crate::redis::exec(&rc, move |d| async move {
                match section {
                    Section::Sessions => Ok(Loaded::Sessions(act::parse_client_list(
                        &d.cmd(&["CLIENT", "LIST"]).await?.text(),
                    ))),
                    Section::Variables => {
                        let r = d.cmd(&["CONFIG", "GET", "*"]).await?;
                        let items = r.array();
                        Ok(Loaded::Pairs(
                            items
                                .chunks(2)
                                .filter(|c| c.len() == 2)
                                .map(|c| (c[0].text(), c[1].text()))
                                .collect(),
                        ))
                    }
                    Section::Status => {
                        let text = d.info("everything").await?;
                        let mut out = Vec::new();
                        for (sec, kvs) in db_redis::parse_info(&text) {
                            out.extend(kvs.into_iter().map(|(k, v)| (format!("{sec}.{k}"), v)));
                        }
                        Ok(Loaded::Pairs(out))
                    }
                }
            })
            .await;
        }
        let d = driver_of(&src).await?;
        let sql = match section {
            Section::Sessions => act::sessions_sql(src.engine),
            Section::Variables => act::variables_sql(src.engine),
            Section::Status => act::status_sql(src.engine),
        }
        .ok_or_else(|| "unsupported".to_string())?;
        let rows = act::fetch(&d, sql).await?;
        Ok(match section {
            Section::Sessions => Loaded::Sessions(act::parse_sessions(src.engine, &rows)),
            _ => Loaded::Pairs(act::parse_pairs(&rows)),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn kill(src: Source, cmd: KillCmd) -> Result<(), String> {
    let rt = src.rt.clone();
    rt.spawn(async move {
        match cmd {
            KillCmd::Redis(args) => {
                let rc = src
                    .redis
                    .clone()
                    .ok_or_else(|| "no Redis connection".to_string())?;
                crate::redis::exec(&rc, move |d| async move { d.cmd(&args).await.map(|_| ()) })
                    .await
            }
            KillCmd::Sql(sql) => {
                let d = driver_of(&src).await?;
                act::fetch(&d, sql).await.map(|_| ())
            }
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

const INTERVALS: [u64; 5] = [0, 2, 5, 10, 30];

pub(crate) struct ActivityPage {
    src: Source,
    section: Section,
    sessions: Vec<Session>,
    vars: Option<Vec<(String, String)>>,
    stats: Option<Vec<(String, String)>>,
    filter: Entity<InputState>,
    filter_text: String,
    sort: Option<(SortKey, bool)>,
    selected: Option<String>,
    interval: u64,
    last_refresh: Option<Instant>,
    hidden: bool,
    loading: bool,
    error: Option<String>,
    scroll: ScrollHandle,
    focus: FocusHandle,
    task: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ActivityEvent> for ActivityPage {}

impl ActivityPage {
    pub fn new(src: Source, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("act.filter_ph").to_string()));
        let sub = cx.subscribe(&filter, |this: &mut Self, input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.filter_text = input.read(cx).value().to_string();
                cx.notify();
            }
        });
        let mut me = Self {
            src,
            section: Section::Sessions,
            sessions: Vec::new(),
            vars: None,
            stats: None,
            filter,
            filter_text: String::new(),
            sort: None,
            selected: None,
            interval: 5,
            last_refresh: None,
            hidden: false,
            loading: false,
            error: None,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            task: None,
            _subs: vec![sub],
        };
        me.refresh(cx);
        me
    }

    pub fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }

    /// Called once a second by the shell with whether the tab is on screen.
    /// A hidden tab does nothing, so an idle window costs no CPU.
    pub fn tick(&mut self, visible: bool, cx: &mut Context<Self>) {
        if !visible {
            self.hidden = true;
            return;
        }
        let stale = self
            .last_refresh
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(self.interval.max(1)));
        if self.section != Section::Sessions || self.loading {
            self.hidden = false;
            return;
        }
        let back = std::mem::replace(&mut self.hidden, false);
        if (back
            && self
                .last_refresh
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(1)))
            || (self.interval > 0 && stale)
        {
            self.refresh(cx);
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let (src, section) = (self.src.clone(), self.section);
        self.loading = true;
        self.task = Some(cx.spawn(async move |this, cx| {
            let res = load(src, section).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.last_refresh = Some(Instant::now());
                match res {
                    Ok(Loaded::Sessions(s)) => {
                        this.sessions = s;
                        this.error = None;
                    }
                    Ok(Loaded::Pairs(p)) => {
                        match this.section {
                            Section::Variables => this.vars = Some(p),
                            _ => this.stats = Some(p),
                        }
                        this.error = None;
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn set_section(&mut self, s: Section, cx: &mut Context<Self>) {
        self.section = s;
        self.error = None;
        let missing = match s {
            Section::Sessions => false,
            Section::Variables => self.vars.is_none(),
            Section::Status => self.stats.is_none(),
        };
        if missing {
            self.refresh(cx);
        }
        cx.notify();
    }

    /// Runs the kill the shell confirmed (and the gate let through).
    pub fn run_kill(&mut self, cmd: KillCmd, cx: &mut Context<Self>) {
        let src = self.src.clone();
        cx.spawn(async move |this, cx| {
            let res = kill(src, cmd).await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(()) => crate::shell::toast::show(t!("act.killed").to_string(), cx),
                    Err(e) => {
                        crate::shell::toast::show(t!("act.kill_failed", msg = e).to_string(), cx)
                    }
                }
                this.refresh(cx);
            });
        })
        .detach();
    }

    fn selected_session(&self) -> Option<&Session> {
        let id = self.selected.as_ref()?;
        self.sessions.iter().find(|s| &s.label() == id)
    }

    fn start_kill(&mut self, mode: KillMode, cx: &mut Context<Self>) {
        let Some(s) = self.selected_session().cloned() else {
            return;
        };
        match act::kill_cmd(self.src.engine, &s, mode) {
            Ok(cmd) => cx.emit(ActivityEvent::Kill {
                session: s,
                mode,
                cmd,
            }),
            Err(act::KillError::Own) => {
                crate::shell::toast::show(t!("act.own_refused").to_string(), cx)
            }
            Err(_) => crate::shell::toast::show(t!("act.kill_unsupported").to_string(), cx),
        }
    }

    fn copy(text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        crate::shell::toast::show(t!("act.copied").to_string(), cx);
    }

    fn sort_by(&mut self, key: SortKey, cx: &mut Context<Self>) {
        self.sort = match self.sort {
            Some((k, false)) if k == key => Some((k, true)),
            Some((k, true)) if k == key => None,
            _ => Some((key, false)),
        };
        cx.notify();
    }

    // ---- rendering ---------------------------------------------------------------

    fn toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let sections = [
            (Section::Sessions, "act.sessions"),
            (Section::Variables, "act.variables"),
            (Section::Status, "act.status"),
        ]
        .into_iter()
        .map(|(s, key)| {
            let me = cx.entity().downgrade();
            SegItem::new(
                format!("act-sec-{key}"),
                t!(key).to_string(),
                self.section == s,
                Rc::new(move |_, _, cx| {
                    let _ = me.update(cx, |this, cx| this.set_section(s, cx));
                }),
            )
        })
        .collect();
        let mut row = h_flex()
            .flex_none()
            .gap(px(S5))
            .items_center()
            .px(px(S6))
            .py(px(S4))
            .border_b_1()
            .border_color(t.ln);
        row = row.child(seg(sections, cx));
        row = row.child(
            div().w(px(260.)).child(
                Input::new(&self.filter).px(px(S5)).cleanable(true).prefix(
                    crate::shell::icons::icon("search")
                        .size(px(14.))
                        .text_color(t.tx3),
                ),
            ),
        );
        row = row.child(
            Button::new("act-refresh")
                .label(t!("act.refresh").to_string())
                .disabled(self.loading)
                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
        );
        if self.section == Section::Sessions {
            let items = INTERVALS
                .iter()
                .map(|&n| {
                    let me = cx.entity().downgrade();
                    let label = if n == 0 {
                        t!("act.interval_off").to_string()
                    } else {
                        format!("{n} s")
                    };
                    SegItem::new(
                        format!("act-iv-{n}"),
                        label,
                        self.interval == n,
                        Rc::new(move |_, _, cx| {
                            let _ = me.update(cx, |this, cx| {
                                this.interval = n;
                                cx.notify();
                            });
                        }),
                    )
                })
                .collect();
            row = row
                .child(
                    div()
                        .text_size(px(TX_SM))
                        .text_color(t.tx3)
                        .child(t!("act.auto").to_string()),
                )
                .child(seg(items, cx));
        }
        row = row.child(div().flex_1());
        let count = match self.section {
            Section::Sessions => t!(
                "act.n_sessions",
                n = act::view(&self.sessions, &self.filter_text, None)
                    .len()
                    .to_string()
            )
            .to_string(),
            Section::Variables => t!(
                "act.n_rows",
                n = self
                    .pairs()
                    .map(|p| act::filter_pairs(p, &self.filter_text).len())
                    .unwrap_or(0)
                    .to_string()
            )
            .to_string(),
            Section::Status => t!(
                "act.n_rows",
                n = self
                    .pairs()
                    .map(|p| act::filter_pairs(p, &self.filter_text).len())
                    .unwrap_or(0)
                    .to_string()
            )
            .to_string(),
        };
        row.child(div().text_size(px(TX_SM)).text_color(t.tx3).child(count))
            .into_any_element()
    }

    fn pairs(&self) -> Option<&Vec<(String, String)>> {
        match self.section {
            Section::Variables => self.vars.as_ref(),
            Section::Status => self.stats.as_ref(),
            Section::Sessions => None,
        }
    }

    fn sessions_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let cols: [(SortKey, &str, Option<f32>); 7] = [
            (SortKey::Id, "act.col_id", Some(90.)),
            (SortKey::User, "act.col_user", Some(110.)),
            (SortKey::Host, "act.col_host", Some(150.)),
            (SortKey::Db, "act.col_db", Some(110.)),
            (SortKey::State, "act.col_state", Some(130.)),
            (SortKey::Time, "act.col_time", Some(80.)),
            (SortKey::Sql, "act.col_sql", None),
        ];
        let mut head = h_flex()
            .flex_none()
            .px(px(S6))
            .h(px(H_COMPACT + S2))
            .items_center()
            .border_b_1()
            .border_color(t.ln);
        for (key, label, w) in cols {
            let arrow = match self.sort {
                Some((k, desc)) if k == key => {
                    if desc {
                        " ▼"
                    } else {
                        " ▲"
                    }
                }
                _ => "",
            };
            let cell = div()
                .id(SharedString::from(format!("act-h-{label}")))
                .when_some(w, |d, w| d.flex_none().w(px(w)))
                .when(w.is_none(), |d| d.flex_1().min_w_0())
                .cursor_pointer()
                .text_size(px(TX_SM))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.tx2)
                .on_click(cx.listener(move |this, _, _, cx| this.sort_by(key, cx)))
                .child(format!("{}{arrow}", t!(label)));
            head = head.child(cell);
        }
        let order = act::view(&self.sessions, &self.filter_text, self.sort);
        let mut list = v_flex();
        for &i in &order {
            let s = &self.sessions[i];
            let label = s.label();
            let sel = self.selected.as_ref() == Some(&label);
            let long = s.long_running();
            let id_text = if s.mine {
                format!("{} {}", s.id, t!("act.mine"))
            } else {
                s.id.clone()
            };
            let click_label = label.clone();
            let cell =
                |w: f32, text: String| div().flex_none().w(px(w)).pr(px(S3)).truncate().child(text);
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("act-r-{label}")))
                    .flex_none()
                    .px(px(S6))
                    .h(px(H_ROW - S2))
                    .items_center()
                    .cursor_pointer()
                    .font_family(mono.clone())
                    .text_size(px(TX_SM))
                    .text_color(if long { t.red_text } else { t.tx })
                    .when(s.mine, |d| d.text_color(t.grn_text))
                    .when(sel, |d| d.bg(t.on))
                    .when(!sel, |d| d.hover(|h| h.bg(t.hv)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = Some(click_label.clone());
                        cx.notify();
                    }))
                    .child(cell(90., id_text))
                    .child(cell(110., s.user.clone()))
                    .child(cell(150., s.host.clone()))
                    .child(cell(110., s.db.clone()))
                    .child(cell(130., s.state.clone()))
                    .child(cell(80., s.secs.map(act::fmt_secs).unwrap_or_default()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(act::one_line(&s.sql, 240)),
                    ),
            );
        }
        let empty = order.is_empty().then(|| {
            div()
                .p(px(S7))
                .text_color(t.tx3)
                .text_size(px(TX_SM))
                .child(
                    if self.loading {
                        t!("act.loading")
                    } else {
                        t!("act.no_rows")
                    }
                    .to_string(),
                )
        });
        v_flex()
            .flex_1()
            .min_h_0()
            .child(head)
            .child(
                div()
                    .id("act-list")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("act-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .child(list)
                            .children(empty),
                    )
                    .vertical_scrollbar(&self.scroll),
            )
            .child(self.detail(cx))
            .into_any_element()
    }

    fn detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let Some(s) = self.selected_session().cloned() else {
            return div()
                .flex_none()
                .px(px(S6))
                .py(px(S4))
                .border_t_1()
                .border_color(t.ln)
                .text_size(px(TX_SM))
                .text_color(t.tx3)
                .child(t!("act.pick").to_string())
                .into_any_element();
        };
        let engine = self.src.engine;
        let kill_text = act::kill_cmd(engine, &s, KillMode::Connection)
            .ok()
            .map(|c| c.text());
        let sql = s.sql.clone();
        let sql_copy = sql.clone();
        let sql_open = sql.clone();
        let mut bar = h_flex().gap(px(S4)).items_center();
        bar = bar.child(
            div()
                .text_size(px(TX_SM))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.tx1)
                .child(format!("{} · {} · {}", s.label(), s.user, s.host)),
        );
        bar = bar.child(div().flex_1());
        bar = bar.child(
            Button::new("act-copy-sql")
                .label(t!("act.copy_sql").to_string())
                .disabled(sql.is_empty())
                .on_click(cx.listener(move |_, _, _, cx| Self::copy(sql_copy.clone(), cx))),
        );
        bar = bar.child(
            Button::new("act-open-sql")
                .label(t!("act.open_in_tab").to_string())
                .disabled(sql.is_empty())
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(ActivityEvent::OpenSql(sql_open.clone()))
                })),
        );
        if let Some(text) = kill_text {
            bar = bar.child(
                Button::new("act-copy-kill")
                    .label(t!("act.copy_kill").to_string())
                    .on_click(cx.listener(move |_, _, _, cx| Self::copy(text.clone(), cx))),
            );
        }
        if engine.can_cancel() {
            bar = bar.child(
                Button::new("act-kill-q")
                    .danger()
                    .label(t!("act.kill_query").to_string())
                    .disabled(s.mine || !s.active)
                    .on_click(cx.listener(|this, _, _, cx| this.start_kill(KillMode::Query, cx))),
            );
        }
        bar = bar.child(
            Button::new("act-kill-c")
                .danger()
                .label(t!("act.kill_session").to_string())
                .disabled(s.mine)
                .on_click(cx.listener(|this, _, _, cx| this.start_kill(KillMode::Connection, cx))),
        );
        v_flex()
            .flex_none()
            .gap(px(S4))
            .px(px(S6))
            .py(px(S4))
            .border_t_1()
            .border_color(t.ln)
            .child(bar)
            .child(
                div()
                    .id("act-detail-sql")
                    .max_h(px(140.))
                    .overflow_y_scroll()
                    .font_family(mono)
                    .text_size(px(TX_SM))
                    .text_color(t.tx)
                    .whitespace_normal()
                    .child(if sql.is_empty() {
                        t!("act.no_sql").to_string()
                    } else {
                        sql
                    }),
            )
            .into_any_element()
    }

    fn pairs_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let Some(rows) = self.pairs() else {
            return div()
                .p(px(S7))
                .text_color(t.tx3)
                .text_size(px(TX_SM))
                .child(
                    if self.loading {
                        t!("act.loading")
                    } else {
                        t!("act.no_rows")
                    }
                    .to_string(),
                )
                .into_any_element();
        };
        let ix = act::filter_pairs(rows, &self.filter_text);
        let all: String = ix
            .iter()
            .map(|&i| format!("{}\t{}", rows[i].0, rows[i].1))
            .collect::<Vec<_>>()
            .join("\n");
        let mut list = v_flex();
        for &i in &ix {
            let (k, v) = rows[i].clone();
            let both = format!("{k} = {v}");
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("act-p-{i}")))
                    .flex_none()
                    .px(px(S6))
                    .min_h(px(H_ROW - S2))
                    .items_center()
                    .gap(px(S5))
                    .cursor_pointer()
                    .font_family(mono.clone())
                    .text_size(px(TX_SM))
                    .hover(|h| h.bg(t.hv))
                    .tooltip(tip(t!("act.click_copy").to_string()))
                    .on_click(cx.listener(move |_, _, _, cx| Self::copy(both.clone(), cx)))
                    .child(
                        div()
                            .flex_none()
                            .w(px(320.))
                            .truncate()
                            .text_color(t.tx2)
                            .child(k),
                    )
                    .child(div().flex_1().min_w_0().whitespace_normal().child(v)),
            );
        }
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .flex_none()
                    .px(px(S6))
                    .py(px(S3))
                    .border_b_1()
                    .border_color(t.ln)
                    .child(
                        Button::new("act-copy-all")
                            .label(t!("act.copy_all").to_string())
                            .disabled(ix.is_empty())
                            .on_click(cx.listener(move |_, _, _, cx| Self::copy(all.clone(), cx))),
                    ),
            )
            .child(
                div()
                    .id("act-pairs")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("act-pairs-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .child(list),
                    )
                    .vertical_scrollbar(&self.scroll),
            )
            .into_any_element()
    }
}

impl Render for ActivityPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = if self.section == Section::Sessions {
            self.sessions_body(cx)
        } else {
            self.pairs_body(cx)
        };
        v_flex()
            .id("server-activity")
            .track_focus(&self.focus)
            .size_full()
            .child(self.toolbar(cx))
            .children(self.error.as_ref().map(|e| crate::redis::error_box(e, cx)))
            .child(body)
    }
}
