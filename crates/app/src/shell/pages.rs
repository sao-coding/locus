//! History and Settings as tabs: opening (one History per Space, one Settings
//! per window), creating their views, and what their buttons do. A page tab
//! never joins a split: showing one collapses the Space's layout to it.
use super::*;
use crate::history::page::{ConnInfo, HistoryPage, HistoryPageEvent};
use crate::settings::page::{SettingsPage, SettingsPageEvent};
use locus_core::spaces::TabPage;

impl Locus {
    /// The page tab of this kind: Settings in any Space, History in this one.
    fn find_page(&self, page: TabPage, cx: &App) -> Option<(usize, String)> {
        let spaces: Vec<usize> = match page {
            TabPage::Settings | TabPage::Saved => (0..self.spaces.len()).collect(),
            TabPage::History => vec![self.active_space],
            TabPage::Redis | TabPage::Activity => Vec::new(),
        };
        for si in spaces {
            for t in &self.spaces[si].tabs {
                let t = t.read(cx);
                if t.pageslot.kind == Some(page) {
                    return Some((si, t.id.clone()));
                }
            }
        }
        None
    }

    pub(crate) fn open_page(&mut self, page: TabPage, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((si, id)) = self.find_page(page, cx) {
            if si != self.active_space {
                self.switch_space(si, window, cx);
            }
            self.show_tab(&id, window, cx);
            return;
        }
        let mut data = self.blank_tab(None, cx);
        data.page = Some(page);
        let tab = self.make_tab(&data, window, cx);
        self.space_mut().tabs.push(tab);
        self.sync_space_conns(self.active_space, cx);
        self.show_tab(&data.id, window, cx);
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_page(TabPage::Settings, window, cx);
    }

    /// Settings on its About category (the update state lives there).
    pub(crate) fn open_settings_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(window, cx);
        let view = self.space().tabs.iter().find_map(|t| {
            let t = t.read(cx);
            (t.pageslot.kind == Some(TabPage::Settings))
                .then(|| t.pageslot.view.clone())
                .flatten()
                .and_then(|v| v.downcast::<SettingsPage>().ok())
        });
        if let Some(v) = view {
            v.update(cx, |p, cx| {
                p.show_cat(crate::settings::page::Cat::About, window, cx)
            });
        }
    }

    pub(crate) fn open_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_page(TabPage::History, window, cx);
    }

    /// The History tab with one entry selected (palette `!`).
    pub(crate) fn open_history_entry(
        &mut self,
        entry: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_history(window, cx);
        if let Some(v) = self.history_view(cx) {
            v.update(cx, |p, cx| p.reveal(entry, window, cx));
        }
    }

    fn history_view(&self, cx: &App) -> Option<Entity<HistoryPage>> {
        self.space().tabs.iter().find_map(|t| {
            let t = t.read(cx);
            (t.pageslot.kind == Some(TabPage::History))
                .then(|| t.pageslot.view.clone())
                .flatten()
                .and_then(|v| v.downcast::<HistoryPage>().ok())
        })
    }

    /// Creates the view of a page tab (called by `make_tab`).
    pub(crate) fn attach_page(
        &mut self,
        tab: &Entity<QueryTab>,
        data: &TabData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match data.page {
            Some(TabPage::History) => {
                let store = self.history.clone();
                let view = cx.new(|cx| HistoryPage::new(store, window, cx));
                let sub = cx.subscribe_in(
                    &view,
                    window,
                    |this, _, ev: &HistoryPageEvent, window, cx| match ev {
                        HistoryPageEvent::Open {
                            conn_id,
                            sql,
                            params,
                        } => this.open_sql_tab_params(
                            conn_id.clone(),
                            sql.clone(),
                            params.clone(),
                            false,
                            window,
                            cx,
                        ),
                        HistoryPageEvent::Rerun {
                            conn_id,
                            sql,
                            params,
                        } => this.open_sql_tab_params(
                            conn_id.clone(),
                            sql.clone(),
                            params.clone(),
                            true,
                            window,
                            cx,
                        ),
                    },
                );
                self.tab_subs.push((data.id.clone(), sub));
                let focus = view.read(cx).focus_handle(cx);
                tab.update(cx, |t, _| {
                    t.pageslot.view = Some(view.into());
                    t.pageslot.focus = Some(focus);
                });
            }
            Some(TabPage::Settings) => {
                let view = cx.new(|cx| SettingsPage::new(window, cx));
                let sub = cx.subscribe_in(
                    &view,
                    window,
                    |this, _, ev: &SettingsPageEvent, window, cx| match ev {
                        SettingsPageEvent::Language(l) => this.set_language(l, window, cx),
                        SettingsPageEvent::Changed => this.apply_settings(window, cx),
                    },
                );
                self.tab_subs.push((data.id.clone(), sub));
                let focus = view.read(cx).focus_handle(cx);
                tab.update(cx, |t, _| {
                    t.pageslot.view = Some(view.into());
                    t.pageslot.focus = Some(focus);
                });
            }
            Some(TabPage::Saved) => self.attach_saved(tab, data, window, cx),
            Some(TabPage::Redis) => self.attach_redis(tab, data, window, cx),
            Some(TabPage::Activity) => self.attach_activity(tab, data, window, cx),
            None => {}
        }
    }

    /// Connections of every Space for the History tab's filter and badges.
    pub(crate) fn sync_pages(&mut self, cx: &mut Context<Self>) {
        self.sync_saved_page(cx);
        let Some(v) = self.history_view(cx) else {
            return;
        };
        let infos: Vec<ConnInfo> = self
            .conns
            .iter()
            .map(|c| ConnInfo {
                id: c.id.clone(),
                name: c.name.clone(),
                kind: c.kind,
                maria: self
                    .conn_version(&c.id)
                    .is_some_and(|v| v.contains("MariaDB")),
                env: c.env,
            })
            .collect();
        v.update(cx, |p, cx| p.set_conns(infos, cx));
    }

    /// A connection used from History / palette joins the active Space.
    pub(crate) fn ensure_conn_in_space(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.conns.iter().any(|c| c.id == id)
            || self.space().connections.iter().any(|c| c == id)
        {
            return;
        }
        self.space_mut().connections.push(id.to_string());
        self.sync_space_conns(self.active_space, cx);
        self.schedule_save(cx);
    }

    /// A new tab on `conn_id` with `sql`, in the focused pane; with `run` it
    /// runs at once (through the Production guard, like any run).
    pub(crate) fn open_sql_tab(
        &mut self,
        conn_id: String,
        sql: String,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_sql_tab_params(conn_id, sql, Vec::new(), run, window, cx);
    }

    /// `open_sql_tab` that also refills the Run dialog values (History entries with parameters).
    pub(crate) fn open_sql_tab_params(
        &mut self,
        conn_id: String,
        sql: String,
        params: Vec<locus_core::history::HistParam>,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_conn_in_space(&conn_id, cx);
        if run {
            self.open_query_with(
                conn_id,
                sql,
                locus_core::history::params_map(&params),
                window,
                cx,
            );
            return;
        }
        let mut data = self.blank_tab(Some(conn_id), cx);
        data.sql = sql;
        let tab = self.make_tab(&data, window, cx);
        let s = self.space_mut();
        s.tabs.push(tab);
        s.show_alone(&data.id, cx);
        self.sync_space_conns(self.active_space, cx);
        self.focus_editor(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Whether the tab is a History / Settings page.
    pub(crate) fn is_page(&self, id: &str, cx: &App) -> bool {
        self.space()
            .tab(id, cx)
            .is_some_and(|t| t.read(cx).pageslot.kind.is_some())
    }
}

impl Locus {
    /// Card header of a page tab: icon, title and a one-line hint.
    pub(crate) fn render_page_header(&self, page: TabPage, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let (kind, title, hint) = match page {
            TabPage::History => (
                crate::shell::theme::TabKind::History,
                t!("hp.tab_title"),
                t!("hp.subtitle"),
            ),
            TabPage::Settings => (
                crate::shell::theme::TabKind::Settings,
                t!("settings.title"),
                t!("sp.subtitle"),
            ),
            TabPage::Redis => (
                crate::shell::theme::TabKind::Table,
                t!("redis.menu_browse"),
                t!("redis.menu_browse"),
            ),
            TabPage::Activity => (
                crate::shell::theme::TabKind::Table,
                t!("act.menu"),
                t!("act.menu"),
            ),
            TabPage::Saved => (
                crate::shell::theme::TabKind::Script,
                t!("sq.tab_title"),
                t!("sq.subtitle"),
            ),
        };
        h_flex()
            .id("card-header")
            .flex_none()
            .h(px(scale::H_TAB + scale::S2))
            .pl(px(scale::S6))
            .pr(px(scale::S5))
            .gap(px(scale::S5))
            .items_center()
            .border_b_1()
            .border_color(t.ln)
            .child(self.sidebar_toggle(cx))
            .child(kind_icon(kind).size(px(14.)).text_color(t.tx2))
            .child(
                div()
                    .flex_none()
                    .text_size(px(scale::TX_MD))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.tx1)
                    .child(title.into_owned()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(scale::TX_SM))
                    .text_color(t.tx3)
                    .child(hint.into_owned()),
            )
            .into_any_element()
    }
}

// ---- bench mode (`locus.exe history-type`) -------------------------------------

impl Locus {
    #[cfg(feature = "dev")]
    pub fn bench_open_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_history(window, cx);
    }

    /// One measured frame: the History search box gets the next text of a
    /// typing sequence (a word letter by letter, then the next word).
    #[cfg(feature = "dev")]
    pub fn bench_type_history(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        const WORDS: [&str; 6] = [
            "select", "from wb_", "where id", "order by", "limit 10", "update",
        ];
        let w = WORDS[(ix / 12) % WORDS.len()];
        let n = (ix % 12 + 1).min(w.len());
        if let Some(v) = self.history_view(cx) {
            v.update(cx, |p, cx| p.set_search(&w[..n], window, cx));
        }
    }
}
