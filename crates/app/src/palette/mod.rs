//! Command palette ("Strata v3"): scopes `>` commands, `@` open tabs (all
//! Spaces), `#` tables / views / schemas of the Space's open connections
//! (from the completion code's catalog cache, never a new connection), `!`
//! history, `$` connections. A chip row shows the scope; Tab / Shift+Tab cycle
//! it. Fuzzy match with highlighted characters; an empty query shows recent
//! tabs, a few commands and recent history.
use crate::shell::scale::*;
use crate::shell::*;
use db_core::ObjectKind;
use locus_core::palette_scope::{Match, Scope, fuzzy, parse_query};
use locus_core::store::{DbKind, Env};
use std::ops::Range;

// ---- command palette and settings -------------------------------------------

/// Every command the palette lists: (command, label key). The key text comes
/// from the live keymap (`keymap::command_keys`).
pub(crate) const ACTIONS: &[(Shortcut, &str)] = &[
    (Shortcut::Run, "actions.run"),
    (Shortcut::RunScript, "actions.run_script"),
    (Shortcut::TxCommit, "txn.act_commit"),
    (Shortcut::TxRollback, "txn.act_rollback"),
    (Shortcut::TxMode, "txn.act_mode"),
    (Shortcut::Format, "tools.format"),
    (Shortcut::Find, "tools.find"),
    (Shortcut::Replace, "tools.replace"),
    (Shortcut::Explain, "tools.explain"),
    (Shortcut::ExplainAnalyze, "tools.explain_analyze"),
    (Shortcut::Cancel, "actions.cancel"),
    (Shortcut::NewTab, "actions.new_tab"),
    (Shortcut::CloseTab, "actions.close_tab"),
    (Shortcut::NextTab, "actions.next_tab"),
    (Shortcut::PrevTab, "actions.prev_tab"),
    (Shortcut::OpenFile, "actions.open"),
    (Shortcut::Save, "actions.save"),
    (Shortcut::SaveAs, "actions.save_as"),
    (Shortcut::Export, "actions.export"),
    (Shortcut::ImportData, "imp.palette"),
    (Shortcut::NextSpace, "actions.next_space"),
    (Shortcut::PrevSpace, "actions.prev_space"),
    (Shortcut::SplitRight, "actions.split_right"),
    (Shortcut::SplitDown, "actions.split_down"),
    (Shortcut::SplitLeft, "actions.split_left"),
    (Shortcut::SplitUp, "actions.split_up"),
    (Shortcut::ClosePane, "actions.close_pane"),
    (Shortcut::ToggleZoom, "split2.act_zoom"),
    (Shortcut::NewSplit, "split2.act_new"),
    (Shortcut::Unsplit, "split2.act_unsplit"),
    (Shortcut::NextPane, "actions.next_pane"),
    (Shortcut::PrevPane, "actions.prev_pane"),
    (Shortcut::ToggleSidebar, "actions.toggle_sidebar"),
    (Shortcut::IconSidebar, "actions.icon_sidebar"),
    (Shortcut::ToggleSchema, "shell.act_schema"),
    (Shortcut::History, "pal.act_history"),
    (Shortcut::OpenDiagram, "erv3.act_open"),
    (Shortcut::BindFolder, "space.bind_folder"),
    (Shortcut::UnbindFolder, "space.unbind_folder"),
    (Shortcut::LockAllProd, "prod.lock_all"),
    (Shortcut::Settings, "actions.settings"),
    (Shortcut::SaveQuery, "sq.act_save"),
    (Shortcut::OpenSaved, "sq.act_open"),
    (Shortcut::ToggleGit, "git.act_toggle"),
    (Shortcut::GitCommit, "git.act_commit"),
    (Shortcut::GitPull, "git.act_pull"),
    (Shortcut::GitPush, "git.act_push"),
    (Shortcut::GitFetch, "git.act_fetch"),
    (Shortcut::GitSwitchBranch, "git.act_branch"),
    (Shortcut::GitRefresh, "git.act_refresh"),
];

/// What Enter does with an entry.
#[derive(Clone, Debug)]
pub(crate) enum Target {
    Command(Shortcut),
    Tab {
        space: usize,
        tab: String,
    },
    /// `SELECT * FROM` a table or view.
    Table {
        conn: String,
        schema: String,
        name: String,
    },
    /// The relationship diagram of a schema.
    Schema {
        conn: String,
        schema: String,
    },
    /// A diagram centred on one table.
    DiagramCentre {
        conn: String,
        schema: String,
        table: String,
    },
    /// The Structure tab of a table or view.
    Structure {
        conn: String,
        schema: String,
        table: String,
    },
    /// The History tab with this entry selected.
    History(u32),
    /// A saved query (id) into a new tab.
    Saved(String),
    /// A new query on the connection.
    Conn(String),
    /// The Server activity tab of a connection.
    Activity(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
enum Group {
    Recent,
    Commands,
    Tabs,
    Tables,
    Views,
    Schemas,
    History,
    Connections,
    Saved,
}

impl Group {
    fn label(self) -> String {
        t!(match self {
            Group::Recent => "pal.g_recent",
            Group::Commands => "pal.g_commands",
            Group::Tabs => "pal.g_tabs",
            Group::Tables => "pal.g_tables",
            Group::Views => "pal.g_views",
            Group::Schemas => "pal.g_schemas",
            Group::History => "pal.g_history",
            Group::Connections => "pal.g_connections",
            Group::Saved => "sq.g_saved",
        })
        .into_owned()
    }

    fn scope(self) -> Scope {
        match self {
            Group::Recent => Scope::All,
            Group::Commands => Scope::Commands,
            Group::Tabs => Scope::Tabs,
            Group::Tables | Group::Views | Group::Schemas => Scope::Objects,
            Group::History => Scope::History,
            Group::Connections => Scope::Connections,
            Group::Saved => Scope::Saved,
        }
    }
}

/// One selectable entry of the palette (a snapshot taken when it opens).
#[derive(Clone)]
pub(crate) struct PalItem {
    group: Group,
    label: String,
    detail: String,
    keys: String,
    icon: String,
    badge: Option<(DbKind, bool)>,
    env: Option<Env>,
    /// Space colour (open tabs).
    hue: Option<locus_core::space_theme::SpaceColor>,
    /// Space icon (open tabs), shown instead of the hue dot when set.
    space_icon: Option<IconName>,
    target: Target,
}

impl PalItem {
    fn new(group: Group, label: String, icon: &str, target: Target) -> Self {
        Self {
            group,
            label,
            detail: String::new(),
            keys: String::new(),
            icon: icon.to_string(),
            badge: None,
            env: None,
            hue: None,
            space_icon: None,
            target,
        }
    }
}

pub(crate) enum PaletteEvent {
    Pick(Target),
}

impl EventEmitter<PaletteEvent> for Palette {}

enum Res {
    Head(String),
    Item {
        item: PalItem,
        label_hits: Vec<usize>,
    },
}

pub(crate) struct Palette {
    items: Vec<PalItem>,
    /// Entries shown for an empty query in the "all" scope.
    recent: Vec<PalItem>,
    input: Entity<InputState>,
    chip: Scope,
    results: Vec<Res>,
    sel: usize,
    scroll: ScrollHandle,
    /// Catalog cache has no entries yet (hint in the `#` scope).
    no_catalog: bool,
    _sub: Subscription,
}

pub(crate) fn palette_commands(cx: &App) -> Vec<PalItem> {
    ACTIONS
        .iter()
        .map(|(s, k)| {
            let mut it = PalItem::new(
                Group::Commands,
                t!(*k).into_owned(),
                "query",
                Target::Command(*s),
            );
            it.keys = crate::shell::keymap::command_keys(*s, cx);
            it
        })
        .collect()
}

const PER_GROUP_ALL: usize = 5;
const PER_GROUP_SCOPED: usize = 60;

impl Palette {
    fn new(
        items: Vec<PalItem>,
        recent: Vec<PalItem>,
        no_catalog: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("t3.pal_placeholder").to_string())
        });
        let sub = cx.subscribe(&input, |this: &mut Self, _, ev: &InputEvent, cx| match ev {
            InputEvent::Change => {
                this.sel = 0;
                this.recompute(cx);
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.pick(cx),
            _ => {}
        });
        let mut p = Self {
            items,
            recent,
            input,
            chip: Scope::All,
            results: Vec::new(),
            sel: 0,
            scroll: ScrollHandle::new(),
            no_catalog,
            _sub: sub,
        };
        p.recompute_with("", Scope::All);
        p
    }

    fn effective(&self, cx: &App) -> (Scope, String) {
        let q = self.input.read(cx).value().to_string();
        let (s, rest) = parse_query(&q, self.chip);
        (s, rest.to_string())
    }

    fn recompute(&mut self, cx: &App) {
        let (scope, q) = self.effective(cx);
        self.recompute_with(&q, scope);
    }

    fn recompute_with(&mut self, q: &str, scope: Scope) {
        let mut res: Vec<Res> = Vec::new();
        if q.is_empty() && scope == Scope::All {
            // Recent tabs, a few commands, recent history.
            let mut groups: Vec<(Group, Vec<&PalItem>)> = Vec::new();
            for it in &self.recent {
                match groups.iter_mut().find(|(g, _)| *g == it.group) {
                    Some((_, v)) => v.push(it),
                    None => groups.push((it.group, vec![it])),
                }
            }
            for (g, its) in groups {
                res.push(Res::Head(format!("{} · {}", g.label(), its.len())));
                for it in its {
                    res.push(Res::Item {
                        item: it.clone(),
                        label_hits: Vec::new(),
                    });
                }
            }
            self.results = res;
            self.sel = self.first_item();
            return;
        }
        let limit = if scope == Scope::All {
            PER_GROUP_ALL
        } else {
            PER_GROUP_SCOPED
        };
        let mut scored: Vec<(Group, i32, usize, Vec<usize>)> = Vec::new();
        for (i, it) in self.items.iter().enumerate() {
            if scope != Scope::All && it.group.scope() != scope {
                continue;
            }
            if q.is_empty() {
                scored.push((it.group, 0, i, Vec::new()));
                continue;
            }
            let m: Option<Match> = fuzzy(q, &it.label);
            match m {
                Some(m) => scored.push((it.group, m.score, i, m.positions)),
                None => {
                    if let Some(m) = fuzzy(q, &it.detail) {
                        scored.push((it.group, m.score / 2, i, Vec::new()));
                    }
                }
            }
        }
        // Stable: equal scores keep their natural order.
        scored.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
        let mut last: Option<Group> = None;
        let mut shown = 0;
        let totals: Vec<(Group, usize)> = {
            let mut v: Vec<(Group, usize)> = Vec::new();
            for s in &scored {
                match v.iter_mut().find(|(g, _)| *g == s.0) {
                    Some((_, n)) => *n += 1,
                    None => v.push((s.0, 1)),
                }
            }
            v
        };
        for (g, _, i, hits) in scored {
            if last != Some(g) {
                let n = totals
                    .iter()
                    .find(|(x, _)| *x == g)
                    .map(|(_, n)| *n)
                    .unwrap_or(0);
                res.push(Res::Head(format!("{} · {n}", g.label())));
                last = Some(g);
                shown = 0;
            }
            if shown >= limit {
                continue;
            }
            shown += 1;
            res.push(Res::Item {
                item: self.items[i].clone(),
                label_hits: hits,
            });
        }
        self.results = res;
        self.sel = self.first_item();
    }

    fn first_item(&self) -> usize {
        self.results
            .iter()
            .position(|r| matches!(r, Res::Item { .. }))
            .unwrap_or(0)
    }

    fn step(&mut self, d: isize, cx: &mut Context<Self>) {
        let ix: Vec<usize> = self
            .results
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r, Res::Item { .. }))
            .map(|(i, _)| i)
            .collect();
        if ix.is_empty() {
            return;
        }
        let cur = ix.iter().position(|i| *i == self.sel).unwrap_or(0) as isize;
        let n = ix.len() as isize;
        self.sel = ix[((cur + d) % n + n) as usize % n as usize];
        self.scroll.scroll_to_item(self.sel);
        cx.notify();
    }

    fn cycle(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (cur, rest) = self.effective(cx);
        self.chip = cur.cycle(back);
        // The prefix leaves the text: the chip carries the scope now.
        self.input
            .update(cx, |s, cx| s.set_value(rest.clone(), window, cx));
        self.sel = 0;
        self.recompute_with(&rest, self.chip);
        cx.notify();
    }

    fn pick(&mut self, cx: &mut Context<Self>) {
        if let Some(Res::Item { item, .. }) = self.results.get(self.sel) {
            let target = item.target.clone();
            cx.emit(PaletteEvent::Pick(target));
        }
    }

    fn set_chip(&mut self, s: Scope, window: &mut Window, cx: &mut Context<Self>) {
        // Clicking a chip drops a typed prefix, as Tab does.
        let (_, rest) = self.effective(cx);
        self.chip = s;
        self.input
            .update(cx, |st, cx| st.set_value(rest.clone(), window, cx));
        self.recompute_with(&rest, s);
        cx.notify();
    }
}

/// `text` with the matched characters coloured.
fn marked(text: &str, hits: &[usize], t: &crate::shell::theme::Tokens) -> AnyElement {
    if hits.is_empty() {
        return div().child(text.to_string()).into_any_element();
    }
    let mut ranges: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let hl = HighlightStyle {
        color: Some(t.acc_hit()),
        font_weight: Some(FontWeight::SEMIBOLD),
        ..Default::default()
    };
    for (ci, (bi, ch)) in text.char_indices().enumerate() {
        if hits.contains(&ci) {
            ranges.push((bi..bi + ch.len_utf8(), hl));
        }
    }
    StyledText::new(text.to_string())
        .with_highlights(ranges)
        .into_any_element()
}

impl crate::shell::theme::Tokens {
    /// Colour of matched characters in the palette (`mark` in the mockup).
    fn acc_hit(&self) -> Hsla {
        if self.dark {
            Hsla::from(rgb(0xf0a36c))
        } else {
            Hsla::from(rgb(0xb4561a))
        }
    }
}

impl Render for Palette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        let (scope, q) = self.effective(cx);
        let mono = cx.theme().mono_font_family.clone();
        let chips = self.palette_chips(scope, cx);
        let list = self.palette_list(scope, &q, cx);
        let hint = |keys: &'static str, what: String| {
            h_flex()
                .gap(px(S2))
                .child(div().font_family(mono.clone()).child(keys))
                .child(what)
        };
        v_flex()
            .key_context("Palette")
            .id("palette")
            .w_full()
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let k = &ev.keystroke;
                if k.modifiers.control || k.modifiers.alt || k.modifiers.platform {
                    return;
                }
                match k.key.as_str() {
                    "down" => this.step(1, cx),
                    "up" => this.step(-1, cx),
                    "tab" => this.cycle(k.modifiers.shift, window, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(S4))
                    .px(px(S6))
                    .h(px(50.))
                    .items_center()
                    .border_b_1()
                    .border_color(t.ln)
                    .child(
                        crate::shell::icons::icon("search")
                            .size(px(16.))
                            .text_color(t.tx3),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.input).appearance(false).bordered(false)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(mono.clone())
                            .text_size(px(TX_XS))
                            .text_color(t.tx3)
                            .child("Esc"),
                    ),
            )
            .child(chips)
            .child(list)
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(16.))
                    .px(px(S6))
                    .py(px(S4))
                    .border_t_1()
                    .border_color(t.ln)
                    .text_size(px(TX_XS))
                    .text_color(t.tx3)
                    .child(hint("↑ ↓", t!("pal.h_select").into_owned()))
                    .child(hint("Enter", t!("pal.h_open").into_owned()))
                    .child(hint("Tab", t!("pal.h_scope").into_owned()))
                    .child(div().flex_1())
                    .child(t!("pal.h_all_spaces").into_owned()),
            )
    }
}

impl Palette {
    /// Scope chips under the input.
    fn palette_chips(&self, scope: Scope, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let mut chips = h_flex()
            .flex_none()
            .gap(px(4.))
            .px(px(S5))
            .py(px(S4))
            .border_b_1()
            .border_color(t.ln);
        for (i, s) in Scope::ALL.iter().copied().enumerate() {
            let on = s == scope;
            chips = chips.child(
                h_flex()
                    .id(("pal-chip", i))
                    .h(px(H_COMPACT))
                    .px(px(S4))
                    .gap(px(S2))
                    .items_center()
                    .rounded_full()
                    .cursor_pointer()
                    .text_size(px(TX_SM))
                    .text_color(if on { t.acc } else { t.tx3 })
                    .bg(if on { t.acc.opacity(0.16) } else { t.hv })
                    .child(t!(s.label_key()).into_owned())
                    .children(s.prefix().map(|c| {
                        div()
                            .font_family(mono.clone())
                            .opacity(0.6)
                            .child(c.to_string())
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| this.set_chip(s, window, cx))),
            );
        }
        chips
    }

    /// Result list: headings, items or the empty message.
    fn palette_list(&self, scope: Scope, q: &str, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = crate::shell::theme::tokens(cx);
        let mut list = v_flex()
            .id("pal-list")
            .max_h(px(420.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px(px(6.))
            .py(px(4.));
        let mut any = false;
        for (ri, r) in self.results.iter().enumerate() {
            match r {
                Res::Head(h) => {
                    list = list.child(
                        div()
                            .px(px(S4))
                            .pt(px(9.))
                            .pb(px(4.))
                            .text_size(px(TX_XS))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.tx3)
                            .child(h.to_uppercase()),
                    );
                }
                Res::Item { item, label_hits } => {
                    any = true;
                    list = list.child(self.palette_item_row(ri, item, label_hits, cx));
                }
            }
        }
        if !any {
            let msg = if scope == Scope::Objects && self.no_catalog && q.is_empty() {
                t!("pal.no_catalog").into_owned()
            } else if q.is_empty() {
                t!("pal.empty_scope").into_owned()
            } else {
                t!("pal.no_match", q = q.to_string()).into_owned()
            };
            list = list.child(
                div()
                    .min_h(px(100.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(S6))
                    .text_size(px(TX_BASE))
                    .text_color(t.tx3)
                    .text_center()
                    .child(msg),
            );
        }
        list
    }

    /// One result row: icon or engine badge, label, badges, detail and keys.
    fn palette_item_row(
        &self,
        ri: usize,
        item: &PalItem,
        label_hits: &[usize],
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = crate::shell::theme::tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let it = item;
        let on = ri == self.sel;
        let icon: AnyElement = match it.badge {
            Some((kind, maria)) => {
                let (label, color) = crate::shell::theme::db_badge(kind, maria, cx);
                let (bg, fg) = crate::shell::theme::badge_colors(color, cx);
                div()
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
                    .font_family(mono.clone())
                    .child(label)
                    .into_any_element()
            }
            None => crate::shell::icons::icon(&it.icon)
                .size(px(14.))
                .text_color(t.tx3)
                .into_any_element(),
        };
        h_flex()
            .id(("pal-item", ri))
            .h(px(H_ROW + S1))
            .px(px(S4))
            .gap(px(S4))
            .items_center()
            .rounded(px(R_MD))
            .cursor_pointer()
            .text_size(px(TX_BASE))
            .text_color(t.tx)
            .when(on, |r| r.bg(t.acc.opacity(0.15)))
            .child(
                div()
                    .w(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(marked(&it.label, label_hits, t)),
            )
            .children(it.env.filter(|e| *e == Env::Prod).map(|e| {
                let (bg, fg) = crate::shell::theme::env_colors(e, cx);
                div()
                    .flex_none()
                    .px(px(5.))
                    .h(px(15.))
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .bg(bg)
                    .text_color(fg)
                    .text_size(px(9.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(e.badge())
            }))
            .children(it.hue.map(|h| {
                match it.space_icon.clone() {
                    Some(i) => Icon::new(i)
                        .size(px(12.))
                        .text_color(crate::shell::style::tint_color(h, cx))
                        .into_any_element(),
                    None => div()
                        .flex_none()
                        .size(px(7.))
                        .rounded_full()
                        .bg(crate::shell::style::tint_color(h, cx))
                        .into_any_element(),
                }
            }))
            .child(
                div()
                    .flex_none()
                    .max_w(px(220.))
                    .truncate()
                    .text_size(px(TX_XS))
                    .text_color(t.tx3)
                    .child(it.detail.clone()),
            )
            .children((!it.keys.is_empty()).then(|| {
                div()
                    .flex_none()
                    .font_family(mono.clone())
                    .text_size(px(TX_XS))
                    .text_color(t.tx3)
                    .child(it.keys.clone())
            }))
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.sel != ri {
                    this.sel = ri;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.sel = ri;
                this.pick(cx);
            }))
    }
}

// ---- opening and running ---------------------------------------------------------

impl Locus {
    /// Entries of every scope, taken now: tabs of all Spaces, the objects the
    /// completion cache holds for this Space's connections, recent history,
    /// connections. Nothing here opens a connection.
    fn palette_items(&self, cx: &mut Context<Self>) -> (Vec<PalItem>, Vec<PalItem>, bool) {
        let mut items = palette_commands(cx);
        let cur_tab = self.active_tab(cx).read(cx).id.clone();
        let tabs = self.tab_items(cx);
        let (objects, no_catalog) = self.object_items();
        let history = self.history_items(cx);
        let conns = self.conn_items();
        // Empty query: recent tabs (most recently shown first), a few commands, recent history.
        let mut recent: Vec<PalItem> = Vec::new();
        let mut seen: Vec<&String> = Vec::new();
        let mut order: Vec<&String> = self.recent.iter().collect();
        for t in tabs.iter() {
            if let Target::Tab { tab, .. } = &t.target
                && !order.contains(&tab)
            {
                order.push(tab);
            }
        }
        for id in order {
            if recent.len() >= 4 || *id == cur_tab || seen.contains(&id) {
                continue;
            }
            seen.push(id);
            if let Some(t) = tabs
                .iter()
                .find(|t| matches!(&t.target, Target::Tab { tab, .. } if tab == id))
            {
                let mut t = t.clone();
                t.group = Group::Recent;
                recent.push(t);
            }
        }
        recent.extend(
            items
                .iter()
                .filter(|i| {
                    matches!(
                        i.target,
                        Target::Command(
                            Shortcut::NewTab
                                | Shortcut::History
                                | Shortcut::Settings
                                | Shortcut::ToggleSchema
                        )
                    )
                })
                .cloned(),
        );
        recent.extend(history.iter().take(3).cloned());
        items.extend(tabs);
        items.extend(objects);
        items.extend(history);
        items.extend(conns);
        items.extend(self.saved_items(cx));
        (items, recent, no_catalog)
    }

    /// Tabs of all Spaces.
    fn tab_items(&self, cx: &mut Context<Self>) -> Vec<PalItem> {
        let mut tabs: Vec<PalItem> = Vec::new();
        for (si, sp) in self.spaces.iter().enumerate() {
            for tab in sp.ordered(cx) {
                let tr = tab.read(cx);
                let conn = tr
                    .conn_id
                    .as_ref()
                    .and_then(|c| self.conns.iter().find(|x| &x.id == c));
                let mut it = PalItem::new(
                    Group::Tabs,
                    tr.display_title(cx),
                    tab_kind(tr)
                        .icon()
                        .trim_start_matches("app/")
                        .trim_end_matches(".svg"),
                    Target::Tab {
                        space: si,
                        tab: tr.id.clone(),
                    },
                );
                it.detail = match conn {
                    Some(c) => format!("{} · {}", sp.name, c.name),
                    None => sp.name.clone(),
                };
                it.hue = Some(sp.theme.primary());
                it.space_icon = crate::shell::space_popover::space_icon(sp.icon.as_deref());
                tabs.push(it);
            }
        }
        tabs
    }

    /// Objects: only what the completion cache already holds. Also whether no connection has a catalog.
    fn object_items(&self) -> (Vec<PalItem>, bool) {
        let mut objects = Vec::new();
        let mut no_catalog = true;
        {
            let cache = self.completions.lock();
            for cid in &self.space().connections {
                let Some(conn) = self.conns.iter().find(|c| &c.id == cid) else {
                    continue;
                };
                let Some(cc) = cache.get(cid) else { continue };
                no_catalog = false;
                let mut schemas: Vec<&String> = cc.objects.keys().collect();
                schemas.sort();
                for schema in schemas {
                    let mut s = PalItem::new(
                        Group::Schemas,
                        schema.clone(),
                        "layers",
                        Target::Schema {
                            conn: cid.clone(),
                            schema: schema.clone(),
                        },
                    );
                    s.detail = conn.name.clone();
                    objects.push(s);
                    for o in &cc.objects[schema] {
                        let (g, icon) = if o.kind == ObjectKind::View {
                            (Group::Views, "eye")
                        } else {
                            (Group::Tables, "table")
                        };
                        let mut it = PalItem::new(
                            g,
                            o.name.clone(),
                            icon,
                            Target::Table {
                                conn: cid.clone(),
                                schema: schema.clone(),
                                name: o.name.clone(),
                            },
                        );
                        it.detail = format!("{} · {}", conn.name, schema);
                        objects.push(it);
                        {
                            let label = t!(
                                "structure.palette",
                                table = o.name.clone(),
                                conn = conn.name.clone()
                            )
                            .into_owned();
                            let mut d = PalItem::new(
                                Group::Schemas,
                                label,
                                "table",
                                Target::Structure {
                                    conn: cid.clone(),
                                    schema: schema.clone(),
                                    table: o.name.clone(),
                                },
                            );
                            d.detail = format!("{} · {}", conn.name, schema);
                            objects.push(d);
                        }
                        if o.kind != ObjectKind::View {
                            let label = t!(
                                "erv3.palette_centre",
                                table = o.name.clone(),
                                conn = conn.name.clone()
                            )
                            .into_owned();
                            let mut d = PalItem::new(
                                Group::Schemas,
                                label,
                                "schema",
                                Target::DiagramCentre {
                                    conn: cid.clone(),
                                    schema: schema.clone(),
                                    table: o.name.clone(),
                                },
                            );
                            d.detail = format!("{} · {}", conn.name, schema);
                            objects.push(d);
                        }
                    }
                }
            }
        }
        (objects, no_catalog)
    }

    /// History: the newest 300 entries of all connections.
    fn history_items(&self, cx: &App) -> Vec<PalItem> {
        let hist = self.history.read(cx);
        let mut history: Vec<PalItem> = Vec::new();
        for (i, e) in hist.entries.iter().enumerate().rev().take(300) {
            let one: String = e.sql.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut it = PalItem::new(
                Group::History,
                one.chars().take(90).collect(),
                "history",
                Target::History(i as u32),
            );
            let cn = self
                .conns
                .iter()
                .find(|c| c.id == e.conn_id)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            it.detail = format!("{cn} · {}", locus_core::history::format_clock(e.at));
            history.push(it);
        }
        history
    }

    /// Connections, and their activity monitors.
    fn conn_items(&self) -> Vec<PalItem> {
        let mut conns: Vec<PalItem> = Vec::new();
        for c in &self.conns {
            let mut it = PalItem::new(
                Group::Connections,
                c.name.clone(),
                "db",
                Target::Conn(c.id.clone()),
            );
            it.detail = format!("{} · {}", c.host, c.database.clone().unwrap_or_default());
            it.badge = Some((
                c.kind,
                self.conn_version(&c.id)
                    .is_some_and(|v| v.contains("MariaDB")),
            ));
            it.env = Some(c.env);
            conns.push(it);
            if c.kind != locus_core::store::DbKind::Sqlite {
                let mut a = PalItem::new(
                    Group::Connections,
                    t!("act.palette", name = c.name.clone()).into_owned(),
                    "analysis",
                    Target::Activity(c.id.clone()),
                );
                a.detail = format!("{} · {}", c.host, c.database.clone().unwrap_or_default());
                a.env = Some(c.env);
                conns.push(a);
            }
        }
        conns
    }

    /// Saved queries (`*`).
    fn saved_items(&self, cx: &App) -> Vec<PalItem> {
        let mut items = Vec::new();
        for q in &self.saved.read(cx).queries {
            let mut it = PalItem::new(
                Group::Saved,
                q.name.clone(),
                "query",
                Target::Saved(q.id.clone()),
            );
            let cn = q
                .conn_id
                .as_ref()
                .and_then(|id| self.conns.iter().find(|c| &c.id == id))
                .map(|c| c.name.clone())
                .unwrap_or_default();
            it.detail = [q.folder.clone(), cn]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            items.push(it);
        }
        items
    }

    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (items, recent, no_catalog) = self.palette_items(cx);
        let pal = cx.new(|cx| Palette::new(items, recent, no_catalog, window, cx));
        let sub = cx.subscribe_in(
            &pal,
            window,
            |this, _, ev: &PaletteEvent, window, cx| match ev {
                PaletteEvent::Pick(target) => {
                    window.close_dialog(cx);
                    // Already inside the shell's update: call `this`, not the weak handle (that panics).
                    this.run_target(target.clone(), window, cx);
                }
            },
        );
        self._subs.push(sub);
        let first = std::cell::Cell::new(true);
        window.open_pop_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                pal.read(cx)
                    .input
                    .read(cx)
                    .focus_handle(cx)
                    .focus(window, cx);
            }
            dialog
                .w(px(660.))
                .margin_top(px(84.))
                .close_button(false)
                .p_0()
                .child(pal.clone())
        });
    }

    fn quote(kind: DbKind, name: &str) -> String {
        match kind {
            DbKind::Mysql => format!("`{}`", name.replace('`', "``")),
            _ => format!("\"{}\"", name.replace('"', "\"\"")),
        }
    }

    /// Enter on a palette entry.
    pub(crate) fn run_target(
        &mut self,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            Target::Command(s) => {
                self.perform(s, false, window, cx);
            }
            Target::Tab { space, tab } => {
                if space < self.spaces.len() && space != self.active_space {
                    self.switch_space(space, window, cx);
                }
                self.show_tab(&tab, window, cx);
            }
            Target::Table { conn, schema, name } => {
                let Some(kind) = self.conns.iter().find(|c| c.id == conn).map(|c| c.kind) else {
                    return;
                };
                let sql = format!(
                    "SELECT * FROM {}.{}",
                    Self::quote(kind, &schema),
                    Self::quote(kind, &name)
                );
                self.ensure_conn_in_space(&conn, cx);
                self.open_query(conn, sql, window, cx);
            }
            Target::Schema { conn, schema } => {
                self.ensure_conn_in_space(&conn, cx);
                self.open_diagram(conn, schema, window, cx);
            }
            Target::DiagramCentre {
                conn,
                schema,
                table,
            } => {
                self.ensure_conn_in_space(&conn, cx);
                self.open_relations(conn, schema, table, window, cx);
            }
            Target::Structure {
                conn,
                schema,
                table,
            } => {
                self.ensure_conn_in_space(&conn, cx);
                self.open_structure(conn, schema, table, window, cx);
            }
            Target::History(i) => self.open_history_entry(i, window, cx),
            Target::Activity(id) => self.open_activity(id, window, cx),
            Target::Saved(id) => self.open_saved_query(&id, window, cx),
            Target::Conn(id) => {
                self.ensure_conn_in_space(&id, cx);
                self.new_tab_on(Some(id), window, cx);
            }
        }
        cx.notify();
    }
}
