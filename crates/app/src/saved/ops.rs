//! Saved queries on the shell: the Saved tab, saving the editor text,
//! opening a saved query into a tab (also from the palette).
use super::page::{SavedPage, SavedPageEvent};
use crate::shell::scale::*;
use crate::shell::*;
use locus_core::saved::SavedQuery;
use locus_core::spaces::{TabData, TabPage};

impl Locus {
    pub(crate) fn open_saved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_page(TabPage::Saved, window, cx);
    }

    pub(crate) fn sync_saved_page(&mut self, cx: &mut Context<Self>) {
        let view = self
            .spaces
            .iter()
            .flat_map(|s| s.tabs.iter())
            .find_map(|t| {
                let t = t.read(cx);
                (t.pageslot.kind == Some(TabPage::Saved))
                    .then(|| t.pageslot.view.clone())
                    .flatten()
                    .and_then(|v| v.downcast::<SavedPage>().ok())
            });
        let Some(v) = view else { return };
        let conns: Vec<(String, String)> = self
            .conns
            .iter()
            .map(|c| (c.id.clone(), c.name.clone()))
            .collect();
        v.update(cx, |p, cx| p.set_conns(conns, cx));
    }

    pub(crate) fn attach_saved(
        &mut self,
        tab: &Entity<QueryTab>,
        data: &TabData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lib = self.saved.clone();
        let view = cx.new(|cx| SavedPage::new(lib, window, cx));
        let sub = cx.subscribe_in(
            &view,
            window,
            |this, _, ev: &SavedPageEvent, window, cx| match ev {
                SavedPageEvent::Open { conn_id, sql, run } => {
                    this.open_saved_sql(conn_id.clone(), sql.clone(), *run, window, cx)
                }
            },
        );
        self.tab_subs.push((data.id.clone(), sub));
        let focus = view.read(cx).focus_handle(cx);
        tab.update(cx, |t, _| {
            t.pageslot.view = Some(view.into());
            t.pageslot.focus = Some(focus);
        });
        self.sync_saved_page(cx);
    }

    /// A saved query in a new tab: on its own connection when that still
    /// exists, else on a connection of a tab of this Space, else the first one.
    pub(crate) fn open_saved_sql(
        &mut self,
        conn_id: Option<String>,
        sql: String,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let known: Vec<String> = self.conns.iter().map(|c| c.id.clone()).collect();
        let conn = conn_id
            .filter(|id| known.contains(id))
            .or_else(|| {
                self.space()
                    .tabs
                    .iter()
                    .rev()
                    .find_map(|t| t.read(cx).conn_id.clone().filter(|id| known.contains(id)))
            })
            .or_else(|| self.space().connections.first().cloned());
        let Some(conn) = conn else {
            crate::shell::toast::show(t!("error.no_connection").to_string(), cx);
            return;
        };
        self.open_sql_tab(conn, sql, run, window, cx);
    }

    /// Palette `*` entry: opens the query into a tab.
    pub(crate) fn open_saved_query(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(q) = self
            .saved
            .read(cx)
            .queries
            .iter()
            .find(|q| q.id == id)
            .cloned()
        else {
            return;
        };
        self.open_saved_sql(q.conn_id, q.sql, false, window, cx);
    }

    /// "Save query...": the selection, or the whole editor text, with a name.
    pub(crate) fn save_query_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab(cx);
        if tab.read(cx).pageslot.kind.is_some()
            || tab.read(cx).structure.is_some()
            || tab.read(cx).diagram.is_some()
        {
            return;
        }
        let (sql, conn_id) = {
            let t = tab.read(cx);
            let s = t.editor.read(cx).state.read(cx);
            let sel = s.selected_value().to_string();
            (
                if sel.trim().is_empty() {
                    s.value().to_string()
                } else {
                    sel
                },
                t.conn_id.clone(),
            )
        };
        if sql.trim().is_empty() {
            crate::shell::toast::show(t!("sq.nothing_to_save").to_string(), cx);
            return;
        }
        let guess: String = sql
            .split_whitespace()
            .take(6)
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(48)
            .collect();
        let name = cx.new(|cx| {
            let mut s = InputState::new(window, cx);
            s.set_value(guess, window, cx);
            s
        });
        let folder =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("sq.f_folder").to_string()));
        let desc =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("sq.f_desc").to_string()));
        name.read(cx).focus_handle(cx).focus(window, cx);
        let lib = self.saved.clone();
        let save: std::rc::Rc<dyn Fn(&mut Window, &mut App)> = {
            let (name, folder, desc, sql, conn_id) = (
                name.clone(),
                folder.clone(),
                desc.clone(),
                sql.clone(),
                conn_id.clone(),
            );
            std::rc::Rc::new(move |window: &mut Window, cx: &mut App| {
                let n = name.read(cx).value().trim().to_string();
                if n.is_empty() {
                    return;
                }
                let q = SavedQuery {
                    name: n,
                    folder: folder.read(cx).value().trim().to_string(),
                    description: desc.read(cx).value().to_string(),
                    conn_id: conn_id.clone(),
                    sql: sql.clone(),
                    ..Default::default()
                };
                lib.update(cx, |l, cx| l.add(q, cx));
                window.close_dialog(cx);
                crate::shell::toast::show(t!("sq.saved_toast").to_string(), cx);
            })
        };
        for i in [&name, &folder, &desc] {
            let save = save.clone();
            cx.subscribe_in(i, window, move |_, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    save(window, cx);
                }
            })
            .detach();
        }
        let (save_ok, save_btn) = (save.clone(), save.clone());
        window.open_pop_dialog(cx, move |dialog, _, cx| {
            let t = crate::shell::theme::tokens(cx);
            let save_btn = save_btn.clone();
            let field = |label: String, input: &Entity<InputState>| {
                h_flex()
                    .gap(px(S4))
                    .items_center()
                    .child(
                        div()
                            .w(px(84.))
                            .flex_none()
                            .text_size(px(TX_SM))
                            .text_color(t.tx3)
                            .child(label),
                    )
                    .child(div().flex_1().min_w_0().child(Input::new(input)))
            };
            let body = v_flex()
                .gap(px(S5))
                .child(field(t!("sq.f_name").to_string(), &name))
                .child(field(t!("sq.f_folder").to_string(), &folder))
                .child(field(t!("sq.f_desc").to_string(), &desc))
                .child(
                    h_flex()
                        .gap(px(S4))
                        .child(div().flex_1())
                        .child(
                            Button::new("sqd-cancel")
                                .h(px(H_ROW))
                                .label(t!("common.cancel").to_string())
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("sqd-save")
                                .h(px(H_ROW))
                                .primary()
                                .label(t!("common.save").to_string())
                                .on_click(move |_, window, cx| save_btn(window, cx)),
                        ),
                );
            let save_ok = save_ok.clone();
            dialog
                .title(t!("sq.save_title").to_string())
                .w(px(W_DLG_M))
                .child(body)
                .on_ok(move |_, window, cx| {
                    save_ok(window, cx);
                    false
                })
        });
    }
}
