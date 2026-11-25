//! The Query History tab ("Strata v3", `renderHistory`): search, connection
//! filter, status chips, a virtual list grouped by day and a detail pane with
//! the formatted SQL. The filter runs on the background executor (a keystroke
//! never walks the entries on the UI thread); only the visible rows are laid
//! out.
use crate::history::{History, HistoryChanged};
use crate::shell::scale::*;
use crate::shell::seg::{SegItem, seg};
use crate::shell::*;
use gpui_kit::component::Disableable as _;
use locus_core::history::{Entry, Filter, Item, Row, Status};
use locus_core::store::{DbKind, Env};
use std::ops::Range;
use std::rc::Rc;

/// What the page needs to know about a connection (all Spaces).
#[derive(Clone, PartialEq)]
pub(crate) struct ConnInfo {
    pub id: String,
    pub name: String,
    pub kind: DbKind,
    pub maria: bool,
    pub env: Env,
}

pub(crate) enum HistoryPageEvent {
    /// "Open as new query" / double-click.
    Open {
        conn_id: String,
        sql: String,
        params: Vec<locus_core::history::HistParam>,
    },
    /// "Re-run": a new tab that runs the statement (through the Production guard).
    Rerun {
        conn_id: String,
        sql: String,
        params: Vec<locus_core::history::HistParam>,
    },
}

impl EventEmitter<HistoryPageEvent> for HistoryPage {}

/// Longest SQL shown formatted in the detail pane (longer text is cut).
const DETAIL_MAX: usize = 20_000;
const ROW_H: f32 = 32.;

pub(crate) struct HistoryPage {
    store: Entity<History>,
    conns: Vec<ConnInfo>,
    rows: Arc<Vec<Row>>,
    filter: Filter,
    items: Vec<Item>,
    count: usize,
    /// Selected entry (index into the store's entries) and its formatted SQL.
    sel: Option<u32>,
    detail: Option<(u32, SharedString)>,
    pending_reveal: Option<u32>,
    search: Entity<InputState>,
    /// Page focus target: the search box is not focused when the tab opens (audit S2).
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    detail_scroll: ScrollHandle,
    gen_: u64,
    task: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl HistoryPage {
    pub fn new(store: Entity<History>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("hp.search").to_string()));
        let mut subs = Vec::new();
        subs.push(
            cx.subscribe(&search, |this: &mut Self, input, ev: &InputEvent, cx| {
                if let InputEvent::Change = ev {
                    this.filter.text = input.read(cx).value().to_string();
                    this.refilter(cx);
                }
            }),
        );
        subs.push(
            cx.subscribe(&store, |this: &mut Self, _, _: &HistoryChanged, cx| {
                this.rebuild(cx)
            }),
        );
        let mut me = Self {
            store,
            conns: Vec::new(),
            rows: Arc::new(Vec::new()),
            filter: Filter::default(),
            items: Vec::new(),
            count: 0,
            sel: None,
            detail: None,
            pending_reveal: None,
            search,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            gen_: 0,
            task: None,
            _subs: subs,
        };
        me.rebuild(cx);
        me
    }

    pub fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }

    pub fn set_conns(&mut self, conns: Vec<ConnInfo>, cx: &mut Context<Self>) {
        if self.conns != conns {
            self.conns = conns;
            cx.notify();
        }
    }

    /// Sets the search text (bench mode).
    #[cfg(feature = "dev")]
    pub fn set_search(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |s, cx| s.set_value(text.to_string(), window, cx));
    }

    /// Shows the entry (palette "history" entry): clears the filters first.
    pub fn reveal(&mut self, entry: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.filter = Filter::default();
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        self.pending_reveal = Some(entry);
        self.refilter(cx);
    }

    /// The store changed: rebuild the lowercased rows, then filter again.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let tz = locus_core::history::local_tz();
        self.rows = Arc::new(locus_core::history::build_rows(
            &self.store.read(cx).entries,
            &tz,
        ));
        self.detail = None;
        self.refilter(cx);
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        self.gen_ += 1;
        let g = self.gen_;
        let rows = self.rows.clone();
        let f = self.filter.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let (items, n) = cx
                .background_executor()
                .spawn(async move { locus_core::history::filter_items(&rows, &f) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.gen_ != g {
                    return;
                }
                this.items = items;
                this.count = n;
                let want = this.pending_reveal.take().or(this.sel);
                let has = |i: u32, items: &[Item]| items.contains(&Item::Entry(i));
                this.sel = match want {
                    Some(i) if has(i, &this.items) => Some(i),
                    _ => this.items.iter().find_map(|x| {
                        if let Item::Entry(i) = x {
                            Some(*i)
                        } else {
                            None
                        }
                    }),
                };
                if let Some(i) = this.sel
                    && let Some(pos) = this.items.iter().position(|x| *x == Item::Entry(i))
                {
                    this.scroll.scroll_to_item(pos, ScrollStrategy::Nearest);
                }
                this.detail = None;
                cx.notify();
            });
        }));
    }

    fn select(&mut self, idx: u32, cx: &mut Context<Self>) {
        self.sel = Some(idx);
        self.detail = None;
        cx.notify();
    }

    fn step(&mut self, d: isize, cx: &mut Context<Self>) {
        let entries: Vec<(usize, u32)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(p, x)| {
                if let Item::Entry(i) = x {
                    Some((p, *i))
                } else {
                    None
                }
            })
            .collect();
        if entries.is_empty() {
            return;
        }
        let cur = entries
            .iter()
            .position(|(_, i)| Some(*i) == self.sel)
            .unwrap_or(0) as isize;
        let next = (cur + d).clamp(0, entries.len() as isize - 1) as usize;
        self.scroll
            .scroll_to_item(entries[next].0, ScrollStrategy::Nearest);
        self.select(entries[next].1, cx);
    }

    fn entry(&self, idx: u32, cx: &App) -> Option<Entry> {
        self.store.read(cx).entries.get(idx as usize).cloned()
    }

    fn conn(&self, id: &str) -> Option<&ConnInfo> {
        self.conns.iter().find(|c| c.id == id)
    }

    fn set_status(&mut self, s: Option<Status>, cx: &mut Context<Self>) {
        if self.filter.status != s {
            self.filter.status = s;
            self.refilter(cx);
        }
    }

    fn set_conn_filter(&mut self, c: Option<String>, cx: &mut Context<Self>) {
        if self.filter.conn != c {
            self.filter.conn = c;
            self.refilter(cx);
        }
    }

    /// Formatted SQL of the selection (cached until the selection changes).
    fn detail_text(&mut self, cx: &App) -> Option<(Entry, SharedString)> {
        let idx = self.sel?;
        let e = self.entry(idx, cx)?;
        if let Some((i, t)) = &self.detail
            && *i == idx
        {
            return Some((e, t.clone()));
        }
        let cut: String = e.sql.chars().take(DETAIL_MAX).collect();
        let mut text =
            db_core::format::format_sql(&cut, &db_core::format::FormatOptions::default());
        if text.trim().is_empty() {
            text = cut;
        }
        if e.sql.chars().count() > DETAIL_MAX {
            text.push_str("\n…");
        }
        let text: SharedString = text.into();
        self.detail = Some((idx, text.clone()));
        Some((e, text))
    }
}

// ---- SQL colouring ----------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Span {
    Keyword,
    Str,
    Number,
    Comment,
    Func,
}

const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IS",
    "IN",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "CROSS",
    "ON",
    "USING",
    "GROUP",
    "BY",
    "ORDER",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "AS",
    "DISTINCT",
    "UNION",
    "ALL",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "CREATE",
    "ALTER",
    "DROP",
    "TABLE",
    "INDEX",
    "VIEW",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "ASC",
    "DESC",
    "WITH",
    "SHOW",
    "DESCRIBE",
    "EXPLAIN",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "START",
    "TRANSACTION",
    "TRUNCATE",
    "REPLACE",
    "USE",
    "DATABASE",
    "SCHEMA",
    "PRIMARY",
    "KEY",
    "FOREIGN",
    "REFERENCES",
    "DEFAULT",
    "CALL",
    "GRANT",
    "REVOKE",
    "TRUE",
    "FALSE",
];

/// Byte ranges to colour. Only ASCII delimiters start a span, so every range
/// falls on a character boundary.
pub(crate) fn sql_spans(s: &str) -> Vec<(Range<usize>, Span)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let is_id = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80;
    while i < b.len() {
        let c = b[i];
        if c == b'-' && b.get(i + 1) == Some(&b'-') || c == b'#' {
            let end = s[i..].find('\n').map(|n| i + n).unwrap_or(b.len());
            out.push((i..end, Span::Comment));
            i = end;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let end = s[i + 2..]
                .find("*/")
                .map(|n| i + 2 + n + 2)
                .unwrap_or(b.len());
            out.push((i..end, Span::Comment));
            i = end;
        } else if c == b'\'' || c == b'"' || c == b'`' {
            let mut j = i + 1;
            while j < b.len() {
                if b[j] == b'\\' && c != b'`' {
                    j += 2;
                    continue;
                }
                if b[j] == c {
                    if b.get(j + 1) == Some(&c) {
                        j += 2;
                        continue;
                    }
                    j += 1;
                    break;
                }
                j += 1;
            }
            let j = j.min(b.len());
            if c != b'`' {
                out.push((i..j, Span::Str));
            }
            i = j;
        } else if c.is_ascii_digit() {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
                j += 1;
            }
            out.push((i..j, Span::Number));
            i = j;
        } else if is_id(c) {
            let mut j = i;
            while j < b.len() && is_id(b[j]) {
                j += 1;
            }
            let w = &s[i..j];
            if KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(w)) {
                out.push((i..j, Span::Keyword));
            } else if b.get(j) == Some(&b'(') {
                out.push((i..j, Span::Func));
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

fn span_color(s: Span, dark: bool) -> Hsla {
    let hex = |v: u32| Hsla::from(rgb(v));
    let c = match s {
        Span::Keyword => hex(0xe39a62),
        Span::Str => hex(0xa5bd8e),
        Span::Number => hex(0xd9b46a),
        Span::Comment => hex(0x6f675d),
        Span::Func => hex(0x8fb2cc),
    };
    if dark {
        c
    } else {
        hsla(c.h, c.s, (c.l * 0.62).max(0.2), 1.0)
    }
}

/// `text` as styled text with SQL colours.
pub(crate) fn highlighted(text: SharedString, dark: bool) -> StyledText {
    let spans = sql_spans(&text);
    let runs: Vec<(Range<usize>, HighlightStyle)> = spans
        .into_iter()
        .map(|(r, s)| {
            let mut st = HighlightStyle {
                color: Some(span_color(s, dark)),
                ..Default::default()
            };
            if s == Span::Comment {
                st.font_style = Some(FontStyle::Italic);
            }
            (r, st)
        })
        .collect();
    StyledText::new(text).with_highlights(runs)
}

/// First `max` characters of the statement on one line.
fn one_line(sql: &str, max: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    for w in sql.split_whitespace() {
        if n > 0 {
            out.push(' ');
        }
        out.push_str(w);
        n += w.chars().count() + 1;
        if n >= max {
            break;
        }
    }
    out.chars().take(max).collect()
}

// ---- view -------------------------------------------------------------------------

impl HistoryPage {
    fn day_label(day: i64) -> String {
        let today = locus_core::history::today();
        let date = locus_core::history::format_day(day);
        let wk = format!("hp.wd{}", locus_core::history::weekday(day));
        let wd = t!(&wk).into_owned();
        if day == today {
            format!("{} · {date}", t!("hp.today"))
        } else if day == today - 1 {
            format!("{} · {date}", t!("hp.yesterday"))
        } else {
            format!("{date} · {wd}")
        }
    }

    fn conn_badge(c: &ConnInfo, cx: &App) -> AnyElement {
        let (label, color) = crate::shell::theme::db_badge(c.kind, c.maria, cx);
        let (bg, fg) = crate::shell::theme::badge_colors(color, cx);
        div()
            .flex_none()
            .w(px(20.))
            .h(px(16.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .bg(bg)
            .text_color(fg)
            .text_size(px(9.))
            .font_weight(FontWeight::SEMIBOLD)
            .font_family(cx.theme().mono_font_family.clone())
            .child(label)
            .into_any_element()
    }

    fn env_pill(env: Env, cx: &App) -> AnyElement {
        let (bg, fg) = crate::shell::theme::env_colors(env, cx);
        div()
            .flex_none()
            .px(px(S3))
            .h(px(15.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .bg(bg)
            .text_color(fg)
            .text_size(px(9.5))
            .font_weight(FontWeight::SEMIBOLD)
            .child(env.badge())
            .into_any_element()
    }

    fn render_item(&self, pos: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        match self.items[pos] {
            Item::Day(d) => div()
                .id(("hday", pos))
                .h(px(ROW_H))
                .px(px(S6))
                .flex()
                .items_end()
                .pb(px(4.))
                .text_size(px(TX_XS))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.tx3)
                .child(Self::day_label(d))
                .into_any_element(),
            Item::Entry(idx) => {
                let Some(e) = self.store.read(cx).entries.get(idx as usize) else {
                    return div().h(px(ROW_H)).into_any_element();
                };
                let on = self.sel == Some(idx);
                let status = e.status();
                let (glyph, gcol) = match status {
                    Status::Ok => ("check", t.grn_text),
                    Status::Failed => ("x", t.red_text),
                    Status::Cancelled => ("square", t.tx4),
                };
                let conn = self.conn(&e.conn_id);
                let dark = t.dark;
                let mono = cx.theme().mono_font_family.clone();
                let result: AnyElement = match status {
                    Status::Failed => div()
                        .text_color(t.red_text)
                        .child(t!("hp.error").into_owned())
                        .into_any_element(),
                    Status::Cancelled => div()
                        .child(t!("hp.cancelled").into_owned())
                        .into_any_element(),
                    Status::Ok => div()
                        .child(e.rows.map(locus_core::fmt::rows).unwrap_or_default())
                        .into_any_element(),
                };
                let mut line = one_line(&e.sql, 240);
                if !e.params.is_empty() {
                    line.push_str(&format!(
                        "   ⟨{}⟩",
                        locus_core::history::params_summary(&e.params)
                    ));
                }
                let sql_el = highlighted(line.into(), dark);
                let mt = |w: f32| {
                    h_flex()
                        .flex_none()
                        .w(px(w))
                        .gap(px(S3))
                        .items_center()
                        .text_size(px(TX_XS))
                        .text_color(t.tx3)
                        .whitespace_nowrap()
                        .overflow_hidden()
                };
                h_flex()
                    .id(("hrow", pos))
                    .w_full()
                    .h(px(ROW_H))
                    .px(px(S6))
                    .gap(px(S5))
                    .items_center()
                    .rounded(px(R_MD))
                    .cursor_pointer()
                    .when(on, |r| r.bg(t.on))
                    .hover(|r| r.bg(t.hv))
                    .child(
                        div()
                            .flex_none()
                            .w(px(18.))
                            .text_center()
                            .text_color(gcol)
                            .child(crate::shell::icons::icon(glyph).size(px(13.))),
                    )
                    .children(e.mcp_client.as_ref().map(|client| {
                        // Sent by an MCP client, not typed in a tab.
                        div()
                            .id(("hmcp", pos))
                            .flex_none()
                            .px(px(S3))
                            .py(px(1.))
                            .rounded(px(4.))
                            .bg(t.acc.opacity(0.14))
                            .text_color(t.acc_text)
                            .text_size(px(TX_XS))
                            .font_weight(FontWeight::SEMIBOLD)
                            .tooltip(crate::shell::tip(
                                t!("mcp.history_tip", client = client.clone()).into_owned(),
                            ))
                            .child("MCP")
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(mono.clone())
                            .text_size(px(12.))
                            .text_color(t.tx)
                            .child(sql_el),
                    )
                    .child(
                        mt(120.)
                            .children(conn.map(|c| Self::conn_badge(c, cx)))
                            .child(
                                div().min_w_0().truncate().child(
                                    conn.map(|c| c.name.clone()).unwrap_or_else(|| "—".into()),
                                ),
                            )
                            .children(
                                conn.filter(|c| c.env == Env::Prod)
                                    .map(|c| Self::env_pill(c.env, cx)),
                            ),
                    )
                    .child(
                        mt(64.)
                            .justify_end()
                            .font_family(mono.clone())
                            .child(locus_core::fmt::duration(Duration::from_millis(e.ms))),
                    )
                    .child(mt(84.).justify_end().child(result))
                    .child(
                        mt(56.)
                            .font_family(mono)
                            .child(locus_core::history::format_clock(e.at)),
                    )
                    .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                        this.select(idx, cx);
                        if ev.click_count() >= 2
                            && let Some(e) = this.entry(idx, cx)
                            && this.conn(&e.conn_id).is_some()
                        {
                            cx.emit(HistoryPageEvent::Open {
                                conn_id: e.conn_id,
                                sql: e.sql,
                                params: e.params,
                            });
                        }
                    }))
                    .into_any_element()
            }
        }
    }

    fn render_detail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let Some((e, text)) = self.detail_text(cx) else {
            return div().into_any_element();
        };
        let conn = self.conn(&e.conn_id).cloned();
        let known = conn.is_some();
        let status = e.status();
        let result = match (status, &e.error) {
            (Status::Failed, Some(err)) => err.clone(),
            (Status::Cancelled, _) => t!("hp.cancelled").into_owned(),
            _ => e.rows.map(locus_core::fmt::rows).unwrap_or_default(),
        };
        let via = e
            .mcp_client
            .as_ref()
            .map(|c| format!(" · MCP: {c}"))
            .unwrap_or_default();
        let info = format!(
            "{} · {} · {}{}{}{via}",
            locus_core::history::format_time(e.at),
            conn.as_ref()
                .map(|c| c.name.clone())
                .unwrap_or_else(|| t!("hp.unknown_conn").into_owned()),
            locus_core::fmt::duration(Duration::from_millis(e.ms)),
            if status == Status::Failed || result.is_empty() {
                ""
            } else {
                " · "
            },
            if status == Status::Failed {
                String::new()
            } else {
                result
            },
        );
        let bar = self.detail_bar(&e, info, known, cx);
        let body = self.detail_body(&e, text, status, cx);
        v_flex()
            .flex_none()
            .h(relative(0.4))
            .border_t_1()
            .border_color(t.ln2)
            .child(bar)
            .child(body)
            .into_any_element()
    }

    /// Info line (time, connection, duration, result) with Copy / Open / Re-run.
    fn detail_bar(&self, e: &Entry, info: String, known: bool, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let (sql_copy, sql_open, sql_rerun) = (e.sql.clone(), e.sql.clone(), e.sql.clone());
        let (p_open, p_rerun) = (e.params.clone(), e.params.clone());
        let (c_open, c_rerun) = (e.conn_id.clone(), e.conn_id.clone());
        let btn = |id: &'static str, label: String| Button::new(id).label(label);
        h_flex()
            .flex_none()
            .h(px(H_TAB - S2))
            .px(px(S6))
            .gap(px(8.))
            .items_center()
            .border_b_1()
            .border_color(t.ln)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(TX_SM))
                    .line_height(relative(1.4))
                    .text_color(t.tx3)
                    .child(info),
            )
            .child(
                btn("hp-copy", t!("hp.copy").into_owned()).on_click(cx.listener(
                    move |_, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(sql_copy.clone()));
                    },
                )),
            )
            .child(
                btn("hp-open", t!("hp.open").into_owned())
                    .disabled(!known)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(HistoryPageEvent::Open {
                            conn_id: c_open.clone(),
                            sql: sql_open.clone(),
                            params: p_open.clone(),
                        });
                    })),
            )
            .child(
                btn("hp-rerun", t!("hp.rerun").into_owned())
                    .primary()
                    .disabled(!known)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(HistoryPageEvent::Rerun {
                            conn_id: c_rerun.clone(),
                            sql: sql_rerun.clone(),
                            params: p_rerun.clone(),
                        });
                    })),
            )
    }

    /// The statement (highlighted), its parameters and, for a failed run, the error.
    fn detail_body(
        &self,
        e: &Entry,
        text: SharedString,
        status: Status,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = crate::shell::theme::tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        div()
            .id("hp-detail-body")
            .flex_1()
            .min_h_0()
            .relative()
            .child(
                v_flex()
                    .id("hp-detail-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.detail_scroll)
                    .px(px(S6))
                    .py(px(S5))
                    .gap(px(8.))
                    .children((status == Status::Failed).then(|| {
                        div()
                            .flex_none()
                            .px(px(S4))
                            .py(px(S2))
                            .rounded(px(R_SM))
                            .bg(t.red.opacity(0.12))
                            .text_color(t.red_text)
                            .text_size(px(12.))
                            .whitespace_normal()
                            .child(e.error.clone().unwrap_or_default())
                    }))
                    .children((!e.params.is_empty()).then(|| {
                        let list = e
                            .params
                            .iter()
                            .map(|p| {
                                if p.ty == "null" {
                                    format!("{} = NULL", p.name)
                                } else {
                                    format!("{} = {}  ({})", p.name, p.value, p.ty)
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        v_flex()
                            .flex_none()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(TX_XS))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(t.tx3)
                                    .child(t!("hp2.params").into_owned()),
                            )
                            .child(
                                div()
                                    .font_family(mono.clone())
                                    .text_size(px(TX_SM))
                                    .text_color(t.tx2)
                                    .whitespace_normal()
                                    .child(list),
                            )
                    }))
                    .child(
                        div()
                            .font_family(mono)
                            .text_size(px(TX_BASE))
                            .line_height(px(19.))
                            .text_color(t.tx)
                            .child(highlighted(text, t.dark)),
                    ),
            )
            .vertical_scrollbar(&self.detail_scroll)
    }
}

impl Render for HistoryPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        let me = cx.entity().downgrade();
        let cur_conn = self.filter.conn.clone();
        let conn_label = cur_conn
            .as_ref()
            .and_then(|id| self.conn(id))
            .map(|c| c.name.clone())
            .unwrap_or_else(|| t!("hp.all_conns").into_owned());
        let list: Vec<(String, String)> = self
            .conns
            .iter()
            .map(|c| (c.id.clone(), c.name.clone()))
            .collect();
        let picker = Button::new("hp-conn")
            .label(conn_label)
            .icon(crate::shell::icons::icon("caret-down").size(px(11.)))
            .dropdown_menu(move |mut menu, _, _| {
                let m = me.clone();
                menu = menu.item(
                    PopupMenuItem::new(t!("hp.all_conns").to_string())
                        .checked(cur_conn.is_none())
                        .on_click(move |_, _, cx| {
                            let _ = m.update(cx, |this, cx| this.set_conn_filter(None, cx));
                        }),
                );
                for (id, name) in list.iter().cloned() {
                    let m = me.clone();
                    let checked = cur_conn.as_deref() == Some(id.as_str());
                    menu = menu.item(PopupMenuItem::new(name).checked(checked).on_click(
                        move |_, _, cx| {
                            let _ =
                                m.update(cx, |this, cx| this.set_conn_filter(Some(id.clone()), cx));
                        },
                    ));
                }
                menu
            });
        let st = self.filter.status;
        let top = h_flex()
            .flex_none()
            .gap(px(8.))
            .items_center()
            .px(px(S6))
            .py(px(S4))
            .border_b_1()
            .border_color(t.ln)
            .child(
                div().w(px(320.)).child(
                    Input::new(&self.search)
                        .px(px(crate::shell::scale::S5))
                        .cleanable(true)
                        .prefix(
                            crate::shell::icons::icon("search")
                                .size(px(14.))
                                .text_color(t.tx3),
                        ),
                ),
            )
            .child(picker)
            .child({
                let opts = [
                    ("hp-all", t!("hp.f_all").into_owned(), None),
                    ("hp-ok", t!("hp.f_ok").into_owned(), Some(Status::Ok)),
                    ("hp-err", t!("hp.f_err").into_owned(), Some(Status::Failed)),
                    (
                        "hp-cancel",
                        t!("hp.f_cancel").into_owned(),
                        Some(Status::Cancelled),
                    ),
                ];
                let items = opts
                    .into_iter()
                    .map(|(id, label, s)| {
                        let l =
                            cx.listener(move |this, _: &ClickEvent, _, cx| this.set_status(s, cx));
                        SegItem::new(id, label, st == s, Rc::new(move |e, w, cx| l(e, w, cx)))
                    })
                    .collect();
                seg(items, cx)
            })
            .child(div().flex_1())
            .child(
                div().text_size(px(TX_XS)).text_color(t.tx3).child(
                    t!("hp.count", n = locus_core::fmt::count(self.count as u64)).into_owned(),
                ),
            );
        let n = self.items.len();
        let body: AnyElement = if n == 0 {
            div()
                .flex_1()
                .min_h(px(140.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(TX_BASE))
                .text_color(t.tx3)
                .child(if self.rows.is_empty() {
                    t!("hp.empty").into_owned()
                } else {
                    t!("hp.no_match").into_owned()
                })
                .into_any_element()
        } else {
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    uniform_list(
                        "history-list",
                        n,
                        cx.processor(|this, range: Range<usize>, _, cx| {
                            range.map(|i| this.render_item(i, cx)).collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .size_full(),
                )
                .vertical_scrollbar(&self.scroll)
                .into_any_element()
        };
        let detail = if n > 0 {
            Some(self.render_detail(cx))
        } else {
            None
        };
        v_flex()
            .id("history-page")
            .track_focus(&self.focus)
            .size_full()
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                let k = &ev.keystroke;
                if k.modifiers.control || k.modifiers.alt || k.modifiers.platform {
                    return;
                }
                match k.key.as_str() {
                    "down" => this.step(1, cx),
                    "up" => this.step(-1, cx),
                    "pagedown" => this.step(10, cx),
                    "pageup" => this.step(-10, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(top)
            .child(body)
            .children(detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn spans_cover_the_obvious_tokens() {
        let s = "SELECT name, count(*) FROM t WHERE a = 'x''y' AND n > 12 -- tail";
        let spans = sql_spans(s);
        let text = |sp: Span| {
            spans
                .iter()
                .filter(|(_, k)| *k == sp)
                .map(|(r, _)| &s[r.clone()])
                .collect::<Vec<_>>()
        };
        assert_eq!(text(Span::Keyword), ["SELECT", "FROM", "WHERE", "AND"]);
        assert_eq!(text(Span::Func), ["count"]);
        assert_eq!(text(Span::Str), ["'x''y'"]);
        assert_eq!(text(Span::Number), ["12"]);
        assert_eq!(text(Span::Comment), ["-- tail"]);
        // Non-ASCII text never splits a character.
        let s = "SELECT '資料' FROM 表";
        for (r, _) in sql_spans(s) {
            assert!(s.is_char_boundary(r.start) && s.is_char_boundary(r.end));
        }
    }

    #[::core::prelude::v1::test]
    fn one_line_collapses_whitespace() {
        assert_eq!(one_line("SELECT  *\n  FROM\tt", 100), "SELECT * FROM t");
        assert_eq!(one_line("abcdef", 3), "abc");
    }
}
