//! The result area of a query tab: the strip (Data / Query Plan / Messages /
//! Statistics), the row filter, "Copy as", the Query Plan and Statistics
//! panes, and the Explain run. State lives in `ResultsUi` inside `QueryTab`.

use crate::query::tab::{MsgKind, QueryTab};
use crate::shell::scale::*;
use db_core::{Cell, DbError, Driver, ErrorKind, Outcome, QueryEvent, QueryOptions};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::table::{TableSelection, TableState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use locus_core::copyfmt::{self, Dialect, Format};
use locus_core::fmt;
use locus_core::plan::{self, Basis, Engine, Plan};
use locus_core::pool::Slot;
use locus_core::status::Phase;
use locus_core::store::SavedConnection;
use rust_i18n::t;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A pane of the strip that is not simply "the result set in front".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Special {
    /// Data with no result set (a write, or nothing returned).
    Data,
    Plan,
    Stats,
}

pub enum PlanState {
    Idle,
    Running {
        analyze: bool,
    },
    Done {
        plan: Plan,
        sql: String,
        elapsed: Duration,
    },
    Failed {
        msg: String,
    },
}

/// Times measured while the last run went on (relative to when the query started).
#[derive(Default)]
pub struct Timing {
    pub exec_start: Option<Instant>,
    pub first_row: Option<Duration>,
    pub end: Option<Duration>,
}

pub struct ResultsUi {
    pub special: Option<Special>,
    pub plan: PlanState,
    plan_task: Option<Task<()>>,
    pub filter_input: Entity<InputState>,
    debounce: Option<Task<()>>,
    /// Width of the strip as last painted (drives the icon-only / menu breakpoints).
    pub strip_w: std::rc::Rc<std::cell::Cell<f32>>,
    /// Width of the area the result tabs share, as last painted.
    pub tabs_w: std::rc::Rc<std::cell::Cell<f32>>,
    scan: Option<Task<()>>,
    pub timing: Timing,
    _sub: Subscription,
}

impl ResultsUi {
    pub fn new(window: &mut Window, cx: &mut Context<QueryTab>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("filter.placeholder").to_string())
        });
        let sub = cx.subscribe(&input, |this: &mut QueryTab, _, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.filter_changed(cx);
            }
        });
        Self {
            special: None,
            plan: PlanState::Idle,
            plan_task: None,
            filter_input: input,
            debounce: None,
            strip_w: std::rc::Rc::new(std::cell::Cell::new(800.0)),
            tabs_w: std::rc::Rc::new(std::cell::Cell::new(400.0)),
            scan: None,
            timing: Timing::default(),
            _sub: sub,
        }
    }
}

fn cell_text(c: Cell<'_>) -> Option<String> {
    match c {
        Cell::Null => None,
        Cell::Text(s) => Some(s.to_string()),
        Cell::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
    }
}

pub(crate) fn dialect_of(d: Option<&Arc<locus_core::driver::AnyDriver>>) -> Dialect {
    match d.map(|d| &**d) {
        Some(locus_core::driver::AnyDriver::Postgres(_)) => Dialect::Postgres,
        Some(locus_core::driver::AnyDriver::Oracle(_)) => Dialect::Oracle,
        Some(locus_core::driver::AnyDriver::Sqlite(_)) => Dialect::Sqlite,
        _ => Dialect::Mysql,
    }
}

/// Which pane the strip shows.
#[derive(Clone, Copy)]
pub enum Pane {
    Data(usize),
    NoData,
    Messages,
    Plan,
    Stats,
}

impl QueryTab {
    pub fn pane(&self) -> Pane {
        match self.ui.special {
            Some(Special::Plan) => Pane::Plan,
            Some(Special::Stats) => Pane::Stats,
            Some(Special::Data) => Pane::NoData,
            None if self.out.active < self.out.results.len() => Pane::Data(self.out.active),
            None => Pane::Messages,
        }
    }

    /// Fresh run: clear the filter and the timing, show the data again.
    pub fn ui_on_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ui.special = None;
        self.ui.timing = Timing::default();
        self.ui.scan = None;
        self.ui.debounce = None;
        self.ui
            .filter_input
            .update(cx, |s, cx| s.set_value("", window, cx));
    }

    pub fn show_result(&mut self, i: usize, cx: &mut Context<Self>) {
        self.ui.special = None;
        let changed = self.out.active != i;
        self.out.active = i;
        if changed && !self.ui.filter_input.read(cx).value().is_empty() {
            self.apply_filter(cx);
        }
        cx.notify();
    }

    pub fn show_special(&mut self, s: Special, cx: &mut Context<Self>) {
        self.ui.special = Some(s);
        cx.notify();
    }

    // ---- row filter ----------------------------------------------------------

    fn filter_changed(&mut self, cx: &mut Context<Self>) {
        self.ui.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            let _ = this.update(cx, |this, cx| this.apply_filter(cx));
        }));
    }

    /// Applies the input's text to the result in front (and clears the others).
    pub fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let text = self.ui.filter_input.read(cx).value().to_string();
        for (i, r) in self.out.results.iter().enumerate() {
            let text = if i == self.out.active {
                text.as_str()
            } else {
                ""
            };
            r.table.update(cx, |t, cx| {
                if t.delegate().filter_text != text {
                    t.delegate_mut().set_filter(text);
                    t.clear_selection(cx);
                }
                cx.notify();
            });
        }
        self.pg.page = 0;
        self.apply_page(cx);
        self.ui.scan = None;
        self.kick_filter(cx);
        cx.notify();
    }

    /// Reads rows through the filter in short slices on the UI thread (about
    /// 3 ms each), so typing and scrolling stay smooth on 100,000 rows. Called
    /// again whenever new rows arrive.
    pub fn kick_filter(&mut self, cx: &mut Context<Self>) {
        let Some(table) = self.current_table().cloned() else {
            return;
        };
        if !table.read(cx).delegate().filter_active() || self.ui.scan.is_some() {
            return;
        }
        let weak = table.downgrade();
        self.ui.scan = Some(cx.spawn(async move |this, cx| {
            loop {
                let step = weak.update(cx, |t, cx| {
                    let done = t.delegate_mut().filter_step(Duration::from_millis(3));
                    cx.notify();
                    done
                });
                let Ok(done) = step else { break };
                let _ = this.update(cx, |this, cx| {
                    if done {
                        this.ui.scan = None;
                    }
                    cx.notify();
                });
                if done {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(1))
                    .await;
            }
        }));
    }

    pub fn filter_scanning(&self) -> bool {
        self.ui.scan.is_some()
    }

    // ---- copy as ---------------------------------------------------------------

    pub fn copy_as(&mut self, fmt_: Format, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(table) = self.current_table().cloned() else {
            crate::shell::toast::show(t!("copy.nothing").to_string(), cx);
            return;
        };
        let (text, shown, total, label) = {
            let t = table.read(cx);
            let g = t.delegate();
            let n = g.rs.columns().len();
            let range = g
                .range_bounds()
                .filter(|(rs, cs)| rs.start() != rs.end() || cs.start() != cs.end());
            let (rows, cols): (Option<Vec<usize>>, std::ops::Range<usize>) =
                match (range, t.selection()) {
                    (Some((rs, cs)), _) => (Some(rs.collect()), *cs.start()..*cs.end() + 1),
                    (None, TableSelection::Cell(r, _) | TableSelection::Row(r)) => {
                        (Some(vec![r]), 0..n)
                    }
                    (None, TableSelection::Column(c)) => (None, c..c + 1),
                    (None, TableSelection::None) => (None, 0..n),
                };
            let (data, cut) = g.copy_table(rows.as_deref(), cols, copyfmt::COPY_CAP);
            let total = rows.as_ref().map(|r| r.len()).unwrap_or(g.matched());
            let name = g.origin_table().unwrap_or_else(|| "table".into());
            let driver = self.driver.clone();
            let quote = |s: &str| match &driver {
                Some(d) => d.quote_identifier(s),
                None => format!("`{}`", s.replace('`', "``")),
            };
            let text = copyfmt::render(fmt_, &data, &name, dialect_of(driver.as_ref()), &quote);
            let label = match fmt_ {
                Format::Csv => "CSV",
                Format::Json => "JSON",
                Format::Insert => "INSERT",
                Format::Markdown => "Markdown",
                Format::Tsv => "TSV",
                Format::JsonLines => "JSON Lines",
                Format::Html => "HTML",
            };
            (
                text,
                data.rows.len(),
                if cut { total } else { data.rows.len() },
                label,
            )
        };
        if shown == 0 {
            crate::shell::toast::show(t!("copy.nothing").to_string(), cx);
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        let msg = if shown < total {
            t!(
                "copy.done_capped",
                n = fmt::count(shown as u64),
                total = fmt::count(total as u64),
                fmt = label,
                cap = fmt::count(copyfmt::COPY_CAP as u64)
            )
            .into_owned()
        } else {
            t!("copy.done", rows = fmt::rows(shown as u64), fmt = label).into_owned()
        };
        crate::shell::toast::show(msg, cx);
    }

    // ---- explain ---------------------------------------------------------------

    /// Runs the plan statement for `stmt` on the tab's connection and shows it
    /// in the Query Plan pane. The caller has already passed the Production gate
    /// when `analyze` (it executes the statement).
    pub fn run_explain(
        &mut self,
        stmt: String,
        analyze: bool,
        show: bool,
        engine: Engine,
        conn: SavedConnection,
        password: Result<String, DbError>,
        slot: Slot,
        ro: Arc<std::sync::atomic::AtomicBool>,
        rt: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) {
        if show {
            self.ui.special = Some(Special::Plan);
        }
        let password = match password {
            Ok(p) => p,
            Err(e) => {
                self.ui.plan = PlanState::Failed { msg: e.message };
                cx.notify();
                return;
            }
        };
        self.ui.plan = PlanState::Running { analyze };
        let variants = plan::explain_sql(engine, &stmt, analyze);
        self.ui.plan_task = Some(cx.spawn(async move |this, cx| {
            let t0 = Instant::now();
            let driver = {
                let mut g = slot.lock().await;
                match g.as_ref() {
                    Some(d) => d.clone(),
                    None => {
                        match locus_core::driver::AnyDriver::connect(&rt, &conn, password, ro).await
                        {
                            Ok(d) => {
                                let d = Arc::new(d);
                                *g = Some(d.clone());
                                d
                            }
                            Err(e) => {
                                drop(g);
                                let _ = this.update(cx, |this, cx| {
                                    this.ui.plan = PlanState::Failed { msg: e.message };
                                    this.ui.plan_task = None;
                                    cx.notify();
                                });
                                return;
                            }
                        }
                    }
                }
            };
            let mut found: Option<Plan> = None;
            let mut last: Option<String> = None;
            let n = variants.len();
            for (i, sql) in variants.into_iter().enumerate() {
                let mut h = driver.query(sql, QueryOptions::default());
                let mut ans = plan::Answer::default();
                let mut outcome = None;
                while let Some(ev) = h.next().await {
                    match ev {
                        QueryEvent::Columns(c) => {
                            ans.columns = c.iter().map(|c| c.name.clone()).collect()
                        }
                        QueryEvent::Batch(b) => {
                            for r in 0..b.rows() {
                                ans.rows.push(
                                    (0..b.columns()).map(|c| cell_text(b.cell(r, c))).collect(),
                                );
                            }
                        }
                        QueryEvent::Finished(o) => {
                            outcome = Some(o);
                            break;
                        }
                        _ => {}
                    }
                }
                match outcome {
                    Some(Outcome::Done { .. }) => match plan::parse(engine, analyze, &ans) {
                        Ok(p) => {
                            found = Some(p);
                            break;
                        }
                        Err(m) => {
                            last = Some(m);
                            // EXPLAIN ANALYZE already ran the statement: never run it twice.
                            if analyze {
                                break;
                            }
                        }
                    },
                    Some(Outcome::Failed(e)) => {
                        let syntax = e.code == Some(1064);
                        last = Some(match e.code {
                            Some(c) => format!("[{c}] {}", e.message),
                            None => e.message,
                        });
                        // Only a syntax error (the server does not know that form) tries the next form.
                        if !(syntax && i + 1 < n) {
                            break;
                        }
                    }
                    _ => {
                        last = Some(t!("state.cancelled").into_owned());
                        break;
                    }
                }
            }
            let elapsed = t0.elapsed();
            let _ = this.update(cx, |this, cx| {
                this.ui.plan = match found {
                    Some(plan) => PlanState::Done {
                        plan,
                        sql: stmt,
                        elapsed,
                    },
                    None => PlanState::Failed {
                        msg: last.unwrap_or_else(|| ErrorKind::Internal.to_string_lossy()),
                    },
                };
                this.ui.plan_task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }
}

trait KindText {
    fn to_string_lossy(self) -> String;
}

impl KindText for ErrorKind {
    fn to_string_lossy(self) -> String {
        locus_core::status::error_title(self)
    }
}

// ---- rendering ---------------------------------------------------------------------

fn strip_tab(
    id: impl Into<ElementId>,
    label: String,
    count: Option<String>,
    tip: Option<String>,
    width: Option<f32>,
    pad: f32,
    icon: Option<&'static str>,
    on: bool,
    cx: &App,
) -> Stateful<Div> {
    // An icon-only tab carries its name in the tooltip.
    let tip = if icon.is_some() {
        Some(label.clone())
    } else {
        tip
    };
    let t = crate::shell::theme::tokens(cx);
    h_flex()
        .id(id)
        .flex_none()
        .when_some(width, |d, w| d.w(px(w)))
        .h(px(H_ROW + S2))
        .px(px(pad))
        .gap(px(S3))
        .items_center()
        .whitespace_nowrap()
        .text_size(px(TX_BASE))
        .font_weight(if on {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::MEDIUM
        })
        .text_color(if on { t.tx1 } else { t.tx3 })
        .cursor_pointer()
        .when(!on, |d| d.hover(|s| s.text_color(t.tx2)))
        .border_b_2()
        .border_color(if on { t.acc } else { transparent_black() })
        .when_some(tip, |d, tip| {
            d.tooltip(move |w, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)
            })
        })
        // Label and count share a baseline; the pair is centred in the tab.
        .child(
            h_flex()
                .min_w_0()
                .when(width.is_some(), |d| d.flex_1())
                .gap(px(S3))
                .items_baseline()
                .when_some(icon, |d, name| {
                    d.child(crate::shell::icons::icon(name).size(px(14.)).flex_none())
                })
                .when(icon.is_none(), |d| {
                    d.child(
                        div()
                            .flex_shrink(1.)
                            .min_w_0()
                            .truncate()
                            .child(label.clone()),
                    )
                })
                .when_some(count, |d, c| {
                    d.child(
                        div()
                            .flex_none()
                            .text_size(px(TX_XS))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(t.tx4)
                            .child(c),
                    )
                }),
        )
}

/// A tab of the strip, as data so it can be drawn inline or in the overflow menu.
#[derive(Clone, Copy)]
enum StripAct {
    Data,
    Result(usize),
    Plan,
    Messages,
    Stats,
}

/// Below this width the toolbar buttons show their icon only (tooltip carries the label).
const STRIP_COMPACT_W: f32 = 1000.0;
/// Below this width Query Plan, Messages and Statistics show an icon only. As the strip narrows the
/// order is: those icons, then the filter down to its minimum, then the buttons fold into one
/// "⋯" menu, and only then the result tabs give way (the active one never below its first
/// dozen characters and count).
const STRIP_ICONTABS_W: f32 = 834.0;
/// Below this width the toolbar buttons collapse into one "⋯" menu.
const STRIP_MENU_W: f32 = 596.0;
/// Narrowest the row filter gets.
const FILTER_MIN_W: f32 = 160.0;

fn pane_footer(text: String, cx: &App) -> impl IntoElement {
    let t = crate::shell::theme::tokens(cx);
    h_flex()
        .id("pane-footer")
        .flex_none()
        .h(px(H_BAR + S1))
        .px(px(S6))
        .gap(px(S5))
        .items_center()
        .border_t_1()
        .border_color(t.ln)
        .text_size(px(TX_XS))
        .text_color(t.tx3)
        .child(div().min_w_0().truncate().child(text))
}

impl QueryTab {
    fn do_strip_act(&mut self, act: StripAct, cx: &mut Context<Self>) {
        match act {
            StripAct::Data => self.show_special(Special::Data, cx),
            StripAct::Result(i) => self.show_result(i, cx),
            StripAct::Plan => self.show_special(Special::Plan, cx),
            StripAct::Stats => self.show_special(Special::Stats, cx),
            StripAct::Messages => {
                self.ui.special = None;
                self.out.active = self.out.results.len();
                cx.notify();
            }
        }
    }

    /// The strip: tabs on the left, filter / copy / export / relations on the right.
    /// The result tabs share the left room (`locus_core::tabfit`); the right side keeps its size:
    /// narrower, the buttons lose their labels, then fold into a "⋯" menu.
    pub fn render_strip(
        &self,
        query_tables: Vec<(Option<String>, String)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let pane = self.pane();
        let width = self.ui.strip_w.get();
        let compact = width < STRIP_COMPACT_W;
        let menu = width < STRIP_MENU_W;
        let icon_tabs = width < STRIP_ICONTABS_W;
        let probe = self.ui.strip_w.clone();
        let me_id = cx.entity_id();
        let mut strip = h_flex()
            .id("result-tabs")
            .relative()
            .flex_none()
            .h(px(H_ROW + S2))
            .pl(px(S4))
            .pr(px(if menu { S3 } else { S5 }))
            .gap_0()
            .items_center()
            .border_b_1()
            .border_color(t.ln)
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let w = f32::from(bounds.size.width);
                        let old = probe.get();
                        let bucket = |w: f32| {
                            (
                                w >= STRIP_MENU_W,
                                w >= STRIP_ICONTABS_W,
                                w >= STRIP_COMPACT_W,
                            )
                        };
                        probe.set(w);
                        // Re-render only when a breakpoint is crossed.
                        if bucket(old) != bucket(w) {
                            cx.notify(me_id);
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );
        strip = self.strip_tabs(strip, pane, icon_tabs, cx);
        strip = self.strip_tools(strip, pane, compact, menu, query_tables, cx);
        strip.into_any_element()
    }

    /// Left: the result tabs (each shrinks evenly down to 96 px, the rest sit behind a "+n"
    /// button). Right, never shrinking: Query Plan, Messages and Statistics.
    fn strip_tabs(
        &self,
        mut strip: Stateful<Div>,
        pane: Pane,
        icon_tabs: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = crate::shell::theme::tokens(cx);
        let mut tabs: Vec<(StripAct, String, Option<String>, bool)> = Vec::new();
        let mut tips: Vec<Option<String>> = Vec::new();
        if self.out.results.is_empty() {
            tabs.push((
                StripAct::Data,
                t!("rt.data").into_owned(),
                None,
                matches!(pane, Pane::NoData),
            ));
            tips.push(None);
        }
        let several = self.out.results.len() > 1;
        let names: Vec<Option<String>> = self.out.results.iter().map(|r| r.name.clone()).collect();
        let names = if several {
            db_core::tabname::dedupe(&names)
        } else {
            names
        };
        for (i, r) in self.out.results.iter().enumerate() {
            let label = if !several {
                t!("rt.data").into_owned()
            } else if let Some(n) = names[i].clone() {
                n
            } else {
                t!("results.result_n", n = (i + 1).to_string()).into_owned()
            };
            tips.push(Some(if r.tip.is_empty() {
                label.clone()
            } else {
                format!(
                    "{label}
{}",
                    r.tip
                )
            }));
            tabs.push((
                StripAct::Result(i),
                label,
                Some(fmt::count(r.rows)),
                matches!(pane, Pane::Data(x) if x == i),
            ));
        }
        let errors = self
            .out
            .messages
            .iter()
            .any(|m| matches!(m.kind, MsgKind::Error(..)));
        tabs.push((
            StripAct::Plan,
            t!("rt.plan").into_owned(),
            None,
            matches!(pane, Pane::Plan),
        ));
        tabs.push((
            StripAct::Messages,
            t!("rt.messages").into_owned(),
            Some(self.out.messages.len().to_string()),
            matches!(pane, Pane::Messages),
        ));
        tabs.push((
            StripAct::Stats,
            t!("rt.stats").into_owned(),
            None,
            matches!(pane, Pane::Stats),
        ));
        tips.resize(tabs.len(), None);
        let n_left = tabs.len() - 3;
        let rpad = if icon_tabs { S4 } else { S5 };
        let right_tabs = tabs.split_off(n_left);
        let right_tips = tips.split_off(n_left);
        let make_tab = |idx: usize,
                        act: StripAct,
                        label: String,
                        count: Option<String>,
                        tip: Option<String>,
                        width: Option<f32>,
                        pad: f32,
                        on: bool,
                        cx: &mut Context<Self>| {
            let icon = (idx >= n_left && icon_tabs).then_some(match act {
                StripAct::Plan => "explain",
                StripAct::Messages => "message",
                _ => "bar-chart",
            });
            let mut el = strip_tab(("rtab", idx), label, count, tip, width, pad, icon, on, cx);
            if matches!(act, StripAct::Messages) && errors {
                el = el.child(div().size(px(S3)).rounded_full().bg(t.red));
            }
            el.on_click(cx.listener(move |this, _, _, cx| this.do_strip_act(act, cx)))
        };
        // Natural width of a tab: padding, label, gap and count.
        let natural: Vec<f32> = tabs
            .iter()
            .map(|(_, label, count, _)| {
                2.0 * S5
                    + locus_core::tabfit::text_w(label)
                    + count
                        .as_ref()
                        .map_or(0.0, |c| S3 + locus_core::tabfit::text_w(c) * 0.85)
            })
            .collect();
        let active_left = tabs.iter().position(|x| x.3);
        // The active tab never gets less than its first dozen characters and the count.
        let min_of = |i: usize| {
            let (_, label, count, _) = &tabs[i];
            let head: String = label.chars().take(12).collect();
            2.0 * S5
                + locus_core::tabfit::text_w(&head)
                + count
                    .as_ref()
                    .map_or(0.0, |c| S3 + locus_core::tabfit::text_w(c) * 0.85)
        };
        let active_min = active_left.map_or(0.0, min_of).min(220.0);
        let fit = locus_core::tabfit::fit(&natural, active_left, active_min, self.ui.tabs_w.get());
        let me = cx.entity().downgrade();
        let list = tabs.clone();
        let mut left = h_flex()
            .id("rtabs")
            .relative()
            .flex_1()
            .min_w(px(active_min
                + if n_left > 1 {
                    locus_core::tabfit::OVERFLOW_W
                } else {
                    0.0
                }))
            .h(px(H_ROW + S2))
            .items_center()
            .overflow_hidden()
            .child({
                let probe = self.ui.tabs_w.clone();
                let me_id = cx.entity_id();
                canvas(
                    move |bounds, _, cx| {
                        let w = f32::from(bounds.size.width);
                        if (probe.get() - w).abs() >= 1.0 {
                            probe.set(w);
                            cx.notify(me_id);
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full()
            });
        for (idx, w) in fit.visible.iter().copied() {
            let (act, label, count, on) = tabs[idx].clone();
            let tip = tips[idx].clone();
            // Names with a common start are cut in the middle so they stay apart.
            let label = if tabs
                .iter()
                .enumerate()
                .any(|(j, o)| j != idx && locus_core::tabfit::shares_prefix(&label, &o.1))
            {
                let room = w
                    - 2.0 * S5
                    - count
                        .as_ref()
                        .map_or(0.0, |c| S3 + locus_core::tabfit::text_w(c) * 0.85);
                locus_core::tabfit::elide_middle(&label, room)
            } else {
                label
            };
            left = left.child(make_tab(idx, act, label, count, tip, Some(w), S5, on, cx));
        }
        if fit.hidden > 0 {
            left = left.child(
                Button::new("tabs-more")
                    .ghost()
                    .xsmall()
                    .flex_none()
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(S2))
                            .child(
                                div()
                                    .relative()
                                    .top(px(0.75))
                                    .child(crate::shell::icons::icon("chevron-down").size(px(12.))),
                            )
                            .child(div().text_size(px(TX_XS)).child(format!("+{}", fit.hidden))),
                    )
                    .tooltip(t!("results.all_results").to_string())
                    .dropdown_menu(move |mut menu, _, _| {
                        for (act, label, count, on) in list.clone() {
                            let me = me.clone();
                            let text = match count {
                                Some(c) => format!("{label}  ·  {c}"),
                                None => label,
                            };
                            menu = menu.item(PopupMenuItem::new(text).checked(on).on_click(
                                move |_, _, cx| {
                                    let _ = me.update(cx, |this, cx| this.do_strip_act(act, cx));
                                },
                            ));
                        }
                        menu
                    }),
            );
        }
        strip = strip.child(left);
        for (i, ((act, label, count, on), tip)) in
            right_tabs.into_iter().zip(right_tips).enumerate()
        {
            strip = strip.child(make_tab(
                n_left + i,
                act,
                label,
                count,
                tip,
                None,
                rpad,
                on,
                cx,
            ));
        }
        strip
    }

    /// Right side of the strip: read-only lock, filter field, relations, structure, copy and export.
    fn strip_tools(
        &self,
        mut strip: Stateful<Div>,
        pane: Pane,
        compact: bool,
        menu: bool,
        query_tables: Vec<(Option<String>, String)>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let has_data = matches!(pane, Pane::Data(_));
        let tool = |id: &'static str, icon: &str, label: String, tip: String| {
            let b = Button::new(id)
                .ghost()
                .flex_none()
                .icon(crate::shell::icons::icon(icon).size(px(14.)))
                .tooltip(tip);
            if compact { b } else { b.label(label) }
        };
        if has_data {
            strip = self.strip_filter(strip, cx);
        }
        let single_table = query_tables.clone();
        if menu {
            return self.strip_tools_menu(strip, has_data, query_tables, cx);
        }
        if let Some(tables) = (!query_tables.is_empty() && has_data).then_some(query_tables)
            && let Some(conn) = self.conn_id.clone()
        {
            strip = strip.child(
                tool(
                    "relations",
                    "schema",
                    t!("diagram.relations_button").to_string(),
                    t!("erv3.relations_query").to_string(),
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(crate::query::tab::TabEvent::ShowQueryRelations {
                        conn_id: conn.clone(),
                        tables: tables.clone(),
                    })
                })),
            );
        }
        if has_data
            && let [(schema, table)] = single_table.as_slice()
            && let Some(conn) = self.conn_id.clone()
        {
            let (schema, table) = (schema.clone().unwrap_or_default(), table.clone());
            strip = strip.child(
                tool(
                    "structure",
                    "table",
                    t!("structure.button").to_string(),
                    t!("structure.button_hint").to_string(),
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(crate::query::tab::TabEvent::ShowStructure {
                        conn_id: conn.clone(),
                        schema: schema.clone(),
                        table: table.clone(),
                    })
                })),
            );
        }
        if has_data {
            let me = cx.entity().downgrade();
            strip = strip.child(
                tool(
                    "copy-as",
                    "copy",
                    t!("copy.button").to_string(),
                    t!("copy.tooltip", cap = fmt::count(copyfmt::COPY_CAP as u64)).to_string(),
                )
                .dropdown_menu(move |mut menu, _, _| {
                    for (f, key) in [
                        (Format::Csv, "copy.csv"),
                        (Format::Json, "copy.json"),
                        (Format::Insert, "copy.insert"),
                        (Format::Markdown, "copy.markdown"),
                        (Format::Tsv, "exp2.copy_tsv"),
                        (Format::JsonLines, "exp2.copy_jsonl"),
                        (Format::Html, "exp2.copy_html"),
                    ] {
                        let me = me.clone();
                        menu = menu.item(PopupMenuItem::new(t!(key).to_string()).on_click(
                            move |_, window, cx| {
                                let _ = me.update(cx, |this, cx| this.copy_as(f, window, cx));
                            },
                        ));
                    }
                    menu
                }),
            );
            strip = strip.child(
                tool(
                    "export",
                    "download",
                    t!("export.button").to_string(),
                    t!("export.tooltip").to_string(),
                )
                .on_click(cx.listener(|this, _, window, cx| this.export_current(window, cx))),
            );
        }
        strip
    }

    /// The narrow strip: relations, structure, copy and export in one "⋯" menu.
    fn strip_tools_menu(
        &self,
        strip: Stateful<Div>,
        has_data: bool,
        query_tables: Vec<(Option<String>, String)>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        if !has_data {
            return strip;
        }
        let me = cx.entity().downgrade();
        let conn = self.conn_id.clone();
        let single = match query_tables.as_slice() {
            [(schema, table)] => Some((schema.clone().unwrap_or_default(), table.clone())),
            _ => None,
        };
        strip.child(
            Button::new("tools-more")
                .ghost()
                .flex_none()
                .icon(crate::shell::icons::icon("more").size(px(14.)))
                .tooltip(t!("results.more_actions").to_string())
                .dropdown_menu(move |mut menu, _, _| {
                    if let Some(conn) = conn.clone() {
                        if !query_tables.is_empty() {
                            let (me, conn, tables) =
                                (me.clone(), conn.clone(), query_tables.clone());
                            menu = menu.item(
                                PopupMenuItem::new(t!("diagram.relations_button").to_string())
                                    .on_click(move |_, _, cx| {
                                        let _ = me.update(cx, |_, cx| {
                                            cx.emit(
                                                crate::query::tab::TabEvent::ShowQueryRelations {
                                                    conn_id: conn.clone(),
                                                    tables: tables.clone(),
                                                },
                                            )
                                        });
                                    }),
                            );
                        }
                        if let Some((schema, table)) = single.clone() {
                            let me = me.clone();
                            menu = menu.item(
                                PopupMenuItem::new(t!("structure.button").to_string()).on_click(
                                    move |_, _, cx| {
                                        let _ = me.update(cx, |_, cx| {
                                            cx.emit(crate::query::tab::TabEvent::ShowStructure {
                                                conn_id: conn.clone(),
                                                schema: schema.clone(),
                                                table: table.clone(),
                                            })
                                        });
                                    },
                                ),
                            );
                        }
                    }
                    menu = menu.separator();
                    for (f, key) in [
                        (Format::Csv, "copy.csv"),
                        (Format::Json, "copy.json"),
                        (Format::Insert, "copy.insert"),
                        (Format::Markdown, "copy.markdown"),
                        (Format::Tsv, "exp2.copy_tsv"),
                        (Format::JsonLines, "exp2.copy_jsonl"),
                        (Format::Html, "exp2.copy_html"),
                    ] {
                        let me = me.clone();
                        menu = menu.item(PopupMenuItem::new(t!(key).to_string()).on_click(
                            move |_, window, cx| {
                                let _ = me.update(cx, |this, cx| this.copy_as(f, window, cx));
                            },
                        ));
                    }
                    let me = me.clone();
                    menu.separator().item(
                        PopupMenuItem::new(t!("export.button").to_string()).on_click(
                            move |_, window, cx| {
                                let _ = me.update(cx, |this, cx| this.export_current(window, cx));
                            },
                        ),
                    )
                }),
        )
    }

    /// Read-only lock (reason in its tooltip) and the row filter field (`.flt`), the item that shrinks.
    fn strip_filter(&self, mut strip: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = crate::shell::theme::tokens(cx);
        // Read-only: a lock icon, the reason in its tooltip.
        if let Some(why) = self
            .current_table()
            .and_then(|t| t.read(cx).delegate().read_only_reason.clone())
        {
            let tip = t!("edit.read_only", why = why).to_string();
            strip = strip.child(
                div()
                    .id("read-only")
                    .flex_none()
                    .mr(px(S2))
                    .child(
                        crate::shell::icons::icon("lock")
                            .size(px(14.))
                            .text_color(t.tx3),
                    )
                    .tooltip(move |w, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)
                    }),
            );
        }
        // Filter field (`.flt`): the item that shrinks.
        strip = strip.child(
            h_flex()
                .id("row-filter")
                .flex_shrink(1.)
                .h(px(H_COMPACT))
                .w(px(230.))
                .min_w(px(FILTER_MIN_W))
                .px(px(S4))
                .mr(px(S4))
                .gap(px(S3))
                .items_center()
                .rounded(px(R_SM))
                .bg(t.on)
                .text_color(t.tx4)
                .child(
                    crate::shell::icons::icon("filter")
                        .size(px(13.))
                        .flex_none(),
                )
                .child(
                    div().flex_1().min_w_0().child(
                        Input::new(&self.ui.filter_input)
                            .xsmall()
                            .appearance(false)
                            .cleanable(true),
                    ),
                ),
        );
        strip
    }

    pub fn render_no_data(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(S3))
            .text_size(px(TX_SM))
            .text_color(t.tx3)
            .child(
                div()
                    .text_size(px(TX_BASE))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.tx2)
                    .child(t!("rt.no_result_title").to_string()),
            )
            .child(
                div()
                    .max_w(px(380.))
                    .text_center()
                    .whitespace_normal()
                    .child(t!("rt.no_result_body").to_string()),
            )
            .child(
                Button::new("go-messages")
                    .label(t!("rt.view_messages").to_string())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.ui.special = None;
                        this.out.active = this.out.results.len();
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    pub fn render_plan(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let centered = |title: String, body: Option<String>, color: Hsla| {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(S3))
                .text_size(px(TX_SM))
                .text_color(t.tx3)
                .child(
                    div()
                        .text_size(px(TX_BASE))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color)
                        .child(title),
                )
                .children(body.map(|b| {
                    div()
                        .max_w(px(560.))
                        .text_center()
                        .whitespace_normal()
                        .child(b)
                }))
        };
        let (body, footer): (AnyElement, String) = match &self.ui.plan {
            PlanState::Idle => (
                centered(
                    t!("plan.idle_title").into_owned(),
                    Some(t!("plan.idle_body").into_owned()),
                    t.tx2,
                )
                .into_any_element(),
                "—".into(),
            ),
            PlanState::Running { analyze } => {
                let k = if *analyze {
                    "plan.running_analyze"
                } else {
                    "plan.running"
                };
                (
                    centered(t!(k).into_owned(), None, t.tx2).into_any_element(),
                    "—".into(),
                )
            }
            PlanState::Failed { msg, .. } => (
                centered(
                    t!("plan.failed").into_owned(),
                    Some(msg.clone()),
                    t.red_text,
                )
                .into_any_element(),
                "—".into(),
            ),
            PlanState::Done { plan, sql, elapsed } => {
                let mut list = v_flex()
                    .id("plan-rows")
                    .size_full()
                    .overflow_y_scroll()
                    .px(px(S6))
                    .py(px(S5));
                list = list.child(
                    div()
                        .pb(px(8.))
                        .text_size(px(TX_MONO))
                        .font_family(mono.clone())
                        .text_color(t.tx4)
                        .truncate()
                        .child(sql.split_whitespace().collect::<Vec<_>>().join(" ")),
                );
                for (i, n) in plan.nodes.iter().enumerate() {
                    list = list.child(plan_node_row(i, n, &mono, t));
                }
                let parts = plan_footer_parts(plan, *elapsed);
                (list.into_any_element(), parts.join(" · "))
            }
        };
        v_flex()
            .size_full()
            .child(div().flex_1().min_h_0().child(body))
            .child(pane_footer(footer, cx))
            .into_any_element()
    }

    pub fn render_stats(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        if !self.has_run {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_size(px(TX_SM))
                .text_color(t.tx3)
                .child(t!("stats.never").to_string())
                .into_any_element();
        }
        let st = self.status.read(cx);
        let phase = st.phase;
        let elapsed = st.elapsed_now();
        let affected: u64 = self
            .out
            .messages
            .iter()
            .filter_map(|m| {
                if let MsgKind::Affected(n) = m.kind {
                    Some(n)
                } else {
                    None
                }
            })
            .sum();
        let kinds = {
            let mut v: Vec<String> = Vec::new();
            for c in db_core::classify::classify_script(&self.last_sql) {
                let k = if c.keyword.is_empty() {
                    "?".to_string()
                } else {
                    c.keyword
                };
                if !v.contains(&k) {
                    v.push(k);
                }
            }
            v.join(", ")
        };
        let mut rows: Vec<(String, String)> = vec![
            (t!("stats.status").into_owned(), phase.label()),
            (t!("stats.kind").into_owned(), kinds),
            (
                t!("stats.statements").into_owned(),
                self.out
                    .messages
                    .iter()
                    .filter(|m| !matches!(m.kind, MsgKind::Error(..)) || true)
                    .count()
                    .to_string(),
            ),
            (
                t!("stats.rows").into_owned(),
                fmt::count(self.out.rows_total),
            ),
            (
                t!("stats.affected").into_owned(),
                if self
                    .out
                    .messages
                    .iter()
                    .any(|m| matches!(m.kind, MsgKind::Affected(_)))
                {
                    fmt::count(affected)
                } else {
                    "—".into()
                },
            ),
            (t!("stats.elapsed").into_owned(), fmt::duration(elapsed)),
        ];
        if let Some(fr) = self.ui.timing.first_row {
            rows.push((t!("stats.first_row").into_owned(), fmt::duration(fr)));
            if let Some(end) = self.ui.timing.end {
                rows.push((
                    t!("stats.fetch").into_owned(),
                    fmt::duration(end.saturating_sub(fr)),
                ));
            }
        }
        rows.push((
            t!("stats.row_limit").into_owned(),
            self.limit
                .map(fmt::count)
                .unwrap_or_else(|| t!("limit.unlimited").into_owned()),
        ));
        if let Some(n) = self.truncated_at() {
            rows.push((t!("stats.truncated").into_owned(), fmt::count(n)));
        }
        let mut grid = h_flex().flex_wrap().gap_x(px(48.)).items_start();
        for (k, v) in rows {
            grid = grid.child(
                h_flex()
                    .w(px(340.))
                    .h(px(28.))
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(t.ln)
                    .child(div().text_color(t.tx3).child(k))
                    .child(
                        div()
                            .text_size(px(TX_SM))
                            .font_family(mono.clone())
                            .child(v),
                    ),
            );
        }
        let footer = t!(
            "stats.footer",
            phase = phase.label(),
            elapsed = fmt::duration(elapsed)
        )
        .into_owned();
        v_flex()
            .size_full()
            .child(
                div()
                    .id("stats")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(S6))
                    .py(px(S4))
                    .text_size(px(TX_BASE))
                    .child(grid),
            )
            .child(pane_footer(footer, cx))
            .into_any_element()
    }

    pub fn messages_footer(&self, cx: &App) -> impl IntoElement {
        pane_footer(
            t!(
                "rt.messages_footer",
                n = self.out.messages.len().to_string()
            )
            .into_owned(),
            cx,
        )
    }
}

/// Footer text of the Data pane: range, filter counts, page state.
pub fn data_range_text(
    a: u64,
    b: u64,
    total: u64,
    fetched: u64,
    filtered: bool,
    scanning: bool,
) -> String {
    let mut s = if filtered {
        t!(
            "filter.range",
            a = fmt::count(a),
            b = fmt::count(b),
            total = fmt::count(total),
            all = fmt::count(fetched)
        )
        .into_owned()
    } else {
        t!(
            "pager.range",
            a = fmt::count(a),
            b = fmt::count(b),
            total = fmt::count(total)
        )
        .into_owned()
    };
    if scanning {
        s.push_str(&t!("filter.scanning"));
    }
    s
}

#[allow(dead_code)]
fn _types(_: &TableState<crate::results::grid::ResultGrid>, _: Phase) {}

/// One operator of the plan: tree glyph, name, detail and the share bar.
fn plan_node_row(
    i: usize,
    n: &plan::PlanNode,
    mono: &SharedString,
    t: &crate::shell::theme::Tokens,
) -> Stateful<Div> {
    let share = n.share;
    h_flex()
        .id(("plan-node", i))
        .h(px(28.))
        .flex_none()
        .pl(px(n.depth as f32 * 18.))
        .items_center()
        .border_b_1()
        .border_color(t.ln)
        .child(
            div()
                .mr(px(8.))
                .font_family(mono.clone())
                .text_color(t.tx4)
                .child("└"),
        )
        .child(
            div()
                .min_w(px(132.))
                .whitespace_nowrap()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.acc_text)
                .child(n.op.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(TX_MONO))
                .font_family(mono.clone())
                .text_color(t.tx3)
                .child(n.detail.clone()),
        )
        .child(
            div()
                .flex_none()
                .w(px(120.))
                .h(px(4.))
                .mx(px(12.))
                .rounded(px(2.))
                .bg(t.on)
                .overflow_hidden()
                .when_some(share, |d, s| {
                    d.child(
                        div()
                            .h(px(4.))
                            .w(relative((s / 100.0) as f32))
                            .bg(t.acc)
                            .opacity(0.8),
                    )
                }),
        )
        .child(
            div()
                .flex_none()
                .w(px(40.))
                .text_right()
                .text_size(px(TX_XS))
                .font_family(mono.clone())
                .text_color(t.tx3)
                .child(
                    share
                        .map(|s| {
                            if s < 1.0 && s > 0.0 {
                                "<1%".to_string()
                            } else {
                                format!("{:.0}%", s)
                            }
                        })
                        .unwrap_or_else(|| "–".into()),
                ),
        )
}

/// Footer pieces of a finished plan: cost, actual time, planning time, basis and the round trip.
fn plan_footer_parts(plan: &Plan, elapsed: std::time::Duration) -> Vec<String> {
    let basis = match plan.basis {
        Basis::Cost => t!("plan.basis_cost"),
        Basis::Time => t!("plan.basis_time"),
        Basis::Rows => t!("plan.basis_rows"),
        Basis::None => t!("plan.basis_none"),
    }
    .into_owned();
    let mut parts = Vec::new();
    if let Some(c) = plan.total_cost {
        parts.push(t!("plan.cost", v = format!("{c:.2}")).into_owned());
    }
    if plan.analyze {
        parts.push(match plan.exec_ms {
            Some(ms) => t!("plan.actual", v = format!("{ms:.2}")).into_owned(),
            None => t!("plan.actual_run").into_owned(),
        });
    }
    if let Some(ms) = plan.plan_ms {
        parts.push(t!("plan.planning", v = format!("{ms:.2}")).into_owned());
    }
    parts.push(basis);
    parts.push(t!("plan.took", v = fmt::duration(elapsed)).into_owned());
    parts
}
