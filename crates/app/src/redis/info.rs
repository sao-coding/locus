//! Server info page: `INFO` sections as a summary (server, clients, memory,
//! stats, keyspace), with a "show all fields" switch.
use super::*;
use gpui_kit::component::{Disableable as _, Selectable as _};

/// Section -> the fields of the summary.
const SUMMARY: &[(&str, &[&str])] = &[
    (
        "Server",
        &[
            "redis_version",
            "redis_mode",
            "os",
            "tcp_port",
            "uptime_in_days",
            "process_id",
        ],
    ),
    (
        "Clients",
        &["connected_clients", "blocked_clients", "maxclients"],
    ),
    (
        "Memory",
        &[
            "used_memory_human",
            "used_memory_peak_human",
            "used_memory_rss_human",
            "maxmemory_human",
            "maxmemory_policy",
            "mem_fragmentation_ratio",
        ],
    ),
    (
        "Stats",
        &[
            "total_connections_received",
            "total_commands_processed",
            "instantaneous_ops_per_sec",
            "keyspace_hits",
            "keyspace_misses",
            "expired_keys",
            "evicted_keys",
        ],
    ),
    ("Keyspace", &[]),
];

pub struct InfoPage {
    ctx: Option<Ctx>,
    sections: Vec<(String, Vec<(String, String)>)>,
    all: bool,
    error: Option<String>,
    loading: bool,
    scroll: ScrollHandle,
    focus: FocusHandle,
    task: Option<Task<()>>,
}

impl InfoPage {
    pub fn new(ctx: Ctx, cx: &mut Context<Self>) -> Self {
        let mut me = Self {
            ctx: Some(ctx),
            sections: Vec::new(),
            all: false,
            error: None,
            loading: false,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            task: None,
        };
        me.refresh(cx);
        me
    }

    /// A page that only shows why the tab could not be opened.
    pub fn failed(msg: String, cx: &mut Context<Self>) -> Self {
        Self {
            ctx: None,
            sections: Vec::new(),
            all: false,
            error: Some(msg),
            loading: false,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            task: None,
        }
    }

    pub fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(ctx) = self.ctx.clone() else { return };
        self.loading = true;
        self.task = Some(run(
            cx,
            &ctx,
            |d| async move { d.info("everything").await },
            |this: &mut Self, r, cx| {
                this.loading = false;
                match r {
                    Ok(text) => {
                        this.sections = db_redis::parse_info(&text);
                        this.error = None;
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            },
        ));
        cx.notify();
    }

    fn rows(&self, section: &str) -> Vec<(String, String)> {
        let Some((_, all)) = self
            .sections
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(section))
        else {
            return Vec::new();
        };
        let wanted = SUMMARY
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(section))
            .map(|(_, f)| *f)
            .unwrap_or(&[]);
        if self.all || wanted.is_empty() {
            return all.clone();
        }
        wanted
            .iter()
            .filter_map(|w| all.iter().find(|(k, _)| k == w).cloned())
            .collect()
    }
}

impl Render for InfoPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let names: Vec<String> = if self.all {
            self.sections.iter().map(|(n, _)| n.clone()).collect()
        } else {
            SUMMARY.iter().map(|(n, _)| n.to_string()).collect()
        };
        let mut cards = v_flex().gap(px(S6)).p(px(S7));
        for name in names {
            let rows = self.rows(&name);
            if rows.is_empty() {
                continue;
            }
            let mut card = v_flex()
                .gap(px(S1))
                .p(px(S5))
                .rounded(px(R_MD))
                .border_1()
                .border_color(t.ln)
                .child(
                    div()
                        .mb(px(S3))
                        .text_size(px(TX_MD))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.tx1)
                        .child(name.clone()),
                );
            for (k, v) in rows {
                // db0:keys=5,expires=1,avg_ttl=0 reads better with spaces.
                let v = if name.eq_ignore_ascii_case("keyspace") {
                    v.replace(',', "  ")
                } else {
                    v
                };
                card = card.child(
                    h_flex()
                        .gap(px(S5))
                        .child(
                            div()
                                .flex_none()
                                .w(px(240.))
                                .font_family(mono.clone())
                                .text_size(px(12.))
                                .text_color(t.tx3)
                                .child(k),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .font_family(mono.clone())
                                .text_size(px(12.))
                                .text_color(t.tx)
                                .whitespace_normal()
                                .child(v),
                        ),
                );
            }
            cards = cards.child(card);
        }
        v_flex()
            .id("redis-info")
            .track_focus(&self.focus)
            .size_full()
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(S4))
                    .items_center()
                    .px(px(S6))
                    .py(px(S4))
                    .border_b_1()
                    .border_color(t.ln)
                    .child(
                        Button::new("redis-info-refresh")
                            .label(t!("redis.refresh").to_string())
                            .disabled(self.loading || self.ctx.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .child(
                        Button::new("redis-info-all")
                            .label(t!("redis.show_all").to_string())
                            .selected(self.all)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.all = !this.all;
                                cx.notify();
                            })),
                    ),
            )
            .children(self.error.as_ref().map(|e| error_box(e, cx)))
            .child(
                div()
                    .id("redis-info-body")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("redis-info-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .child(cards),
                    )
                    .vertical_scrollbar(&self.scroll),
            )
    }
}

/// `SLOWLOG GET`: the slowest recent commands, read-only.
pub struct SlowLog {
    ctx: Ctx,
    rows: Vec<locus_core::redis_edit::SlowEntry>,
    error: Option<String>,
    loading: bool,
    scroll: ScrollHandle,
    task: Option<Task<()>>,
}

impl SlowLog {
    pub fn new(ctx: Ctx, cx: &mut Context<Self>) -> Self {
        let mut me = Self {
            ctx,
            rows: Vec::new(),
            error: None,
            loading: false,
            scroll: ScrollHandle::new(),
            task: None,
        };
        me.refresh(cx);
        me
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.task = Some(run(
            cx,
            &self.ctx,
            |d| async move { d.cmd(&["SLOWLOG", "GET", "64"]).await },
            |this: &mut Self, r, cx| {
                this.loading = false;
                match r {
                    Ok(reply) => {
                        this.error = None;
                        this.rows = reply
                            .array()
                            .iter()
                            .map(|e| {
                                let f = e.array();
                                let cmd = f
                                    .get(3)
                                    .map(|c| {
                                        c.array()
                                            .iter()
                                            .map(|a| a.text())
                                            .collect::<Vec<_>>()
                                            .join(" ")
                                    })
                                    .unwrap_or_default();
                                (
                                    f.first().map_or(0, |x| x.int()),
                                    f.get(1).map_or(0, |x| x.int()),
                                    f.get(2).map_or(0, |x| x.int()),
                                    cmd,
                                )
                            })
                            .collect();
                        locus_core::redis_edit::sort_slow_newest_first(&mut this.rows);
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            },
        ));
        cx.notify();
    }
}

impl Render for SlowLog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let mut list = v_flex().gap(px(S1)).p(px(S5));
        if self.rows.is_empty() && !self.loading {
            list = list.child(
                div()
                    .text_color(t.tx3)
                    .text_size(px(TX_BASE))
                    .child(t!("redis2.slow_empty").into_owned()),
            );
        }
        for (id, ts, us, cmd) in &self.rows {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let when = t!(
                "redis2.ago",
                t = locus_core::redis::fmt_ttl_short(Some((now - *ts).max(0) * 1000))
            )
            .into_owned();
            list = list.child(
                h_flex()
                    .gap(px(S5))
                    .font_family(mono.clone())
                    .text_size(px(12.))
                    .child(
                        div()
                            .flex_none()
                            .w(px(48.))
                            .text_color(t.tx3)
                            .child(format!("#{id}")),
                    )
                    .child(div().flex_none().w(px(110.)).text_color(t.tx3).child(when))
                    .child(
                        div()
                            .flex_none()
                            .w(px(90.))
                            .text_color(t.tx)
                            .child(format!("{us} µs")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(t.tx)
                            .child(cmd.clone()),
                    ),
            );
        }
        v_flex()
            .id("redis-slowlog")
            .size_full()
            .child(
                h_flex().flex_none().px(px(S5)).py(px(S3)).child(
                    Button::new("redis-slow-refresh")
                        .label(t!("redis.refresh").to_string())
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                ),
            )
            .children(self.error.as_ref().map(|e| error_box(e, cx)))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("redis-slow-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .child(list),
                    )
                    .vertical_scrollbar(&self.scroll),
            )
    }
}
