//! The Saved tab: the library of saved queries (search, folders, open / run /
//! edit / delete, import / export) and the snippets (built-in and user).
use super::{SavedChanged, SavedLib};
use crate::query::editor::SqlEditor;
use crate::shell::scale::*;
use crate::shell::seg::{SegItem, seg};
use crate::shell::*;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::input::Editor;
use locus_core::saved::{self, SavedQuery, Snippet};
use std::rc::Rc;

pub(crate) enum SavedPageEvent {
    /// A new tab with the query; `run` also runs it.
    Open {
        conn_id: Option<String>,
        sql: String,
        run: bool,
    },
}

impl EventEmitter<SavedPageEvent> for SavedPage {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Queries,
    Snippets,
}

pub(crate) struct SavedPage {
    lib: Entity<SavedLib>,
    conns: Vec<(String, String)>,
    mode: Mode,
    search: Entity<InputState>,
    focus: FocusHandle,
    /// Selected query id and its edit fields.
    sel: Option<String>,
    f_name: Entity<InputState>,
    f_folder: Entity<InputState>,
    f_desc: Entity<InputState>,
    f_sql: Entity<SqlEditor>,
    f_conn: Option<String>,
    /// Selected snippet (its prefix) and edit fields.
    sel_snip: Option<String>,
    s_prefix: Entity<InputState>,
    s_desc: Entity<InputState>,
    s_body: Entity<SqlEditor>,
    list_scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl SavedPage {
    pub fn new(lib: Entity<SavedLib>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let inp = |ph: Option<String>, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let s = InputState::new(window, cx);
                match ph {
                    Some(p) => s.placeholder(p),
                    None => s,
                }
            })
        };
        let search = inp(Some(t!("sq.search").to_string()), window, cx);
        let f_name = inp(Some(t!("sq.f_name").to_string()), window, cx);
        let f_folder = inp(Some(t!("sq.f_folder").to_string()), window, cx);
        let f_desc = inp(Some(t!("sq.f_desc").to_string()), window, cx);
        let s_prefix = inp(Some(t!("sq.s_prefix").to_string()), window, cx);
        let s_desc = inp(Some(t!("sq.f_desc").to_string()), window, cx);
        let f_sql = cx.new(|cx| SqlEditor::new("", window, cx));
        let s_body = cx.new(|cx| SqlEditor::new("", window, cx));
        let mut subs = Vec::new();
        subs.push(cx.subscribe(&search, |_, _, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                cx.notify();
            }
        }));
        subs.push(cx.subscribe(&lib, |_, _, _: &SavedChanged, cx| cx.notify()));
        Self {
            lib,
            conns: Vec::new(),
            mode: Mode::Queries,
            search,
            focus: cx.focus_handle(),
            sel: None,
            f_name,
            f_folder,
            f_desc,
            f_sql,
            f_conn: None,
            sel_snip: None,
            s_prefix,
            s_desc,
            s_body,
            list_scroll: ScrollHandle::new(),
            _subs: subs,
        }
    }

    pub fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }

    pub fn set_conns(&mut self, conns: Vec<(String, String)>, cx: &mut Context<Self>) {
        if self.conns != conns {
            self.conns = conns;
            cx.notify();
        }
    }

    fn conn_name(&self, id: &Option<String>) -> Option<String> {
        id.as_ref()
            .and_then(|id| self.conns.iter().find(|c| &c.0 == id))
            .map(|c| c.1.clone())
    }

    fn select(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(q) = self
            .lib
            .read(cx)
            .queries
            .iter()
            .find(|q| q.id == id)
            .cloned()
        else {
            return;
        };
        self.f_name
            .update(cx, |s, cx| s.set_value(q.name.clone(), window, cx));
        self.f_folder
            .update(cx, |s, cx| s.set_value(q.folder.clone(), window, cx));
        self.f_desc
            .update(cx, |s, cx| s.set_value(q.description.clone(), window, cx));
        self.f_sql
            .read(cx)
            .state
            .clone()
            .update(cx, |s, cx| s.set_value(q.sql.clone(), window, cx));
        self.f_conn = q.conn_id.clone();
        self.sel = Some(id);
        cx.notify();
    }

    fn edited(&self, cx: &App) -> Option<SavedQuery> {
        let id = self.sel.clone()?;
        Some(SavedQuery {
            id,
            name: self.f_name.read(cx).value().trim().to_string(),
            folder: self.f_folder.read(cx).value().trim().to_string(),
            conn_id: self.f_conn.clone(),
            description: self.f_desc.read(cx).value().to_string(),
            sql: self.f_sql.read(cx).state.read(cx).value().to_string(),
            updated: 0,
        })
    }

    fn save_edit(&mut self, cx: &mut Context<Self>) {
        let Some(q) = self.edited(cx) else { return };
        if q.name.is_empty() {
            crate::shell::toast::show(t!("sq.name_required").to_string(), cx);
            return;
        }
        self.lib.update(cx, |l, cx| l.update(q, cx));
    }

    fn select_snippet(&mut self, prefix: String, window: &mut Window, cx: &mut Context<Self>) {
        let all = saved::all_snippets(&self.lib.read(cx).snippets);
        let Some(s) = all.into_iter().find(|s| s.prefix == prefix) else {
            return;
        };
        self.s_prefix
            .update(cx, |i, cx| i.set_value(s.prefix.clone(), window, cx));
        self.s_desc
            .update(cx, |i, cx| i.set_value(s.description.clone(), window, cx));
        self.s_body
            .read(cx)
            .state
            .clone()
            .update(cx, |i, cx| i.set_value(s.body.clone(), window, cx));
        self.sel_snip = Some(prefix);
        cx.notify();
    }

    fn new_snippet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.s_prefix
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.s_desc.update(cx, |i, cx| i.set_value("", window, cx));
        self.s_body
            .read(cx)
            .state
            .clone()
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.sel_snip = None;
        self.s_prefix.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn save_snippet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prefix = self.s_prefix.read(cx).value().trim().to_string();
        if prefix.is_empty() || prefix.chars().any(|c| !(c.is_alphanumeric() || c == '_')) {
            crate::shell::toast::show(t!("sq.prefix_invalid").to_string(), cx);
            return;
        }
        let s = Snippet {
            prefix: prefix.clone(),
            description: self.s_desc.read(cx).value().to_string(),
            body: self.s_body.read(cx).state.read(cx).value().to_string(),
        };
        let old = self.sel_snip.clone();
        self.lib.update(cx, |l, cx| {
            let mut v = l.snippets.clone();
            // Renaming replaces the old entry; the same prefix overrides it.
            v.retain(|x| {
                Some(&x.prefix) != old.as_ref() && !x.prefix.eq_ignore_ascii_case(&prefix)
            });
            v.push(s);
            l.set_snippets(v, cx);
        });
        self.select_snippet(prefix, window, cx);
    }

    fn delete_snippet(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.sel_snip.take() else {
            return;
        };
        self.lib.update(cx, |l, cx| {
            let mut v = l.snippets.clone();
            v.retain(|x| x.prefix != p);
            l.set_snippets(v, cx);
        });
        cx.notify();
    }

    fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let json = self.lib.read(cx).export();
        let dir = std::env::var_os("USERPROFILE")
            .map(|h| std::path::PathBuf::from(h).join("Documents"))
            .unwrap_or_default();
        let rx = cx.prompt_for_new_path(&dir, Some("saved-queries.json"));
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let r = cx
                .background_executor()
                .spawn(async move { std::fs::write(&path, json) })
                .await;
            let _ = cx.update(|_, cx| match r {
                Ok(()) => crate::shell::toast::show(t!("sq.exported").to_string(), cx),
                Err(e) => crate::shell::toast::show(
                    t!("sq.export_failed", msg = e.to_string()).to_string(),
                    cx,
                ),
            });
        })
        .detach();
    }

    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(p) = paths.into_iter().next() else {
                return;
            };
            let read = cx
                .background_executor()
                .spawn(async move { std::fs::read(&p) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let msg = match read.map_err(|e| e.to_string()).and_then(|b| {
                    let b = b
                        .strip_prefix(&[0xEF, 0xBB, 0xBF][..])
                        .map(<[u8]>::to_vec)
                        .unwrap_or(b);
                    String::from_utf8(b).map_err(|e| e.to_string())
                }) {
                    Ok(text) => match this.lib.update(cx, |l, cx| l.import(&text, cx)) {
                        Ok(n) => t!("sq.imported", n = n.to_string()).to_string(),
                        Err(e) => t!("sq.import_failed", msg = e).to_string(),
                    },
                    Err(e) => t!("sq.import_failed", msg = e).to_string(),
                };
                crate::shell::toast::show(msg, cx);
            });
        })
        .detach();
    }

    fn render_queries(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let left = self.render_query_list(cx);
        let right = self.render_query_detail(cx);
        h_flex()
            .flex_1()
            .min_h_0()
            .child(left)
            .child(right)
            .into_any_element()
    }

    /// Left column: queries grouped by folder, filtered by the search field.
    fn render_query_list(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = crate::shell::theme::tokens(cx);
        let text = self.search.read(cx).value().to_string();
        let lib = self.lib.read(cx);
        let hits = saved::search(&lib.queries, &text);
        // Grouped by folder (folder names sorted, unfiled last).
        let mut folders = saved::folders(&lib.queries);
        folders.push(String::new());
        let mut list = v_flex().gap(px(1.)).p(px(S3));
        for f in folders {
            let rows: Vec<&SavedQuery> = hits
                .iter()
                .map(|i| &lib.queries[*i])
                .filter(|q| q.folder == f)
                .collect();
            if rows.is_empty() {
                continue;
            }
            let title = if f.is_empty() {
                t!("sq.unfiled").to_string()
            } else {
                f.clone()
            };
            list = list.child(
                div()
                    .px(px(S4))
                    .pt(px(S4))
                    .pb(px(2.))
                    .text_size(px(TX_XS))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.tx3)
                    .child(title),
            );
            for q in rows {
                let on = self.sel.as_deref() == Some(q.id.as_str());
                let id = q.id.clone();
                let conn = self.conn_name(&q.conn_id);
                list = list.child(
                    v_flex()
                        .id(SharedString::from(format!("sq-{}", q.id)))
                        .px(px(S4))
                        .py(px(S2))
                        .rounded(px(R_MD))
                        .cursor_pointer()
                        .when(on, |r| r.bg(t.on))
                        .hover(|r| r.bg(t.hv))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(TX_MD))
                                .text_color(t.tx)
                                .child(q.name.clone()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(TX_XS))
                                .text_color(t.tx3)
                                .child(match (&conn, q.description.is_empty()) {
                                    (Some(c), false) => format!("{c} · {}", q.description),
                                    (Some(c), true) => c.clone(),
                                    (None, _) => q.description.clone(),
                                }),
                        )
                        .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                            this.select(id.clone(), window, cx);
                            if ev.click_count() >= 2 {
                                this.open_selected(false, cx);
                            }
                        })),
                );
            }
        }
        let empty = lib.queries.is_empty();
        let none = hits.is_empty();
        div()
            .id("sq-list")
            .w(px(320.))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(t.ln)
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll)
            .child(list)
            .children((empty || none).then(|| {
                div()
                    .p(px(S6))
                    .text_size(px(TX_SM))
                    .text_color(t.tx3)
                    .child(if empty {
                        t!("sq.empty").to_string()
                    } else {
                        t!("sq.no_match").to_string()
                    })
            }))
    }

    /// Right column: the edit form of the selected query, or the hint to pick one.
    fn render_query_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        match &self.sel {
            None => div()
                .flex_1()
                .p(px(S6))
                .text_size(px(TX_SM))
                .text_color(t.tx3)
                .child(t!("sq.pick").to_string())
                .into_any_element(),
            Some(_) => {
                let conn = self
                    .conn_name(&self.f_conn)
                    .unwrap_or_else(|| t!("sq.any_conn").to_string());
                let known_conn = self
                    .f_conn
                    .as_ref()
                    .is_some_and(|c| self.conns.iter().any(|x| &x.0 == c));
                let _ = known_conn;
                let list2: Vec<(String, String)> = self.conns.clone();
                let me = cx.entity().downgrade();
                let cur = self.f_conn.clone();
                let picker = Button::new("sq-conn")
                    .label(conn)
                    .icon(crate::shell::icons::icon("caret-down").size(px(11.)))
                    .dropdown_menu(move |mut menu, _, _| {
                        let m = me.clone();
                        menu = menu.item(
                            PopupMenuItem::new(t!("sq.any_conn").to_string())
                                .checked(cur.is_none())
                                .on_click(move |_, _, cx| {
                                    let _ = m.update(cx, |this, cx| {
                                        this.f_conn = None;
                                        cx.notify();
                                    });
                                }),
                        );
                        for (id, name) in list2.iter().cloned() {
                            let m = me.clone();
                            let checked = cur.as_deref() == Some(id.as_str());
                            menu = menu.item(PopupMenuItem::new(name).checked(checked).on_click(
                                move |_, _, cx| {
                                    let _ = m.update(cx, |this, cx| {
                                        this.f_conn = Some(id.clone());
                                        cx.notify();
                                    });
                                },
                            ));
                        }
                        menu
                    });
                let id = self.sel.clone().unwrap_or_default();
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
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .p(px(S6))
                    .gap(px(S4))
                    .child(field(t!("sq.f_name").to_string(), &self.f_name))
                    .child(field(t!("sq.f_folder").to_string(), &self.f_folder))
                    .child(field(t!("sq.f_desc").to_string(), &self.f_desc))
                    .child(
                        h_flex()
                            .gap(px(S4))
                            .items_center()
                            .child(
                                div()
                                    .w(px(84.))
                                    .flex_none()
                                    .text_size(px(TX_SM))
                                    .text_color(t.tx3)
                                    .child(t!("sq.f_conn").to_string()),
                            )
                            .child(picker),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(120.))
                            .border_1()
                            .border_color(t.ln)
                            .rounded(px(R_MD))
                            .overflow_hidden()
                            .child(
                                Editor::new(&self.f_sql.read(cx).state.clone())
                                    .bordered(false)
                                    .h_full(),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(px(S4))
                            .child(
                                Button::new("sq-save")
                                    .label(t!("sq.save_changes").to_string())
                                    .on_click(cx.listener(|this, _, _, cx| this.save_edit(cx))),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new("sq-delete")
                                    .label(t!("sq.delete").to_string())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let id = id.clone();
                                        this.sel = None;
                                        this.lib.update(cx, |l, cx| l.remove(&id, cx));
                                    })),
                            )
                            .child(
                                Button::new("sq-open")
                                    .label(t!("sq.open").to_string())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.open_selected(false, cx)),
                                    ),
                            )
                            .child(
                                Button::new("sq-run")
                                    .primary()
                                    .label(t!("sq.run").to_string())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.open_selected(true, cx)),
                                    ),
                            ),
                    )
                    .into_any_element()
            }
        }
    }

    fn open_selected(&mut self, run: bool, cx: &mut Context<Self>) {
        let Some(q) = self.edited(cx) else { return };
        cx.emit(SavedPageEvent::Open {
            conn_id: q.conn_id,
            sql: q.sql,
            run,
        });
    }

    fn render_snippets(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let user = self.lib.read(cx).snippets.clone();
        let all = saved::all_snippets(&user);
        let text = self.search.read(cx).value().to_lowercase();
        let mut list = v_flex().gap(px(1.)).p(px(S3));
        for s in all.iter().filter(|s| {
            text.is_empty()
                || s.prefix.to_lowercase().contains(&text)
                || s.description.to_lowercase().contains(&text)
        }) {
            let on = self.sel_snip.as_deref() == Some(s.prefix.as_str());
            let builtin = !user.iter().any(|u| u.prefix == s.prefix);
            let p = s.prefix.clone();
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("sn-{}", s.prefix)))
                    .px(px(S4))
                    .py(px(S3))
                    .gap(px(S4))
                    .rounded(px(R_MD))
                    .cursor_pointer()
                    .when(on, |r| r.bg(t.on))
                    .hover(|r| r.bg(t.hv))
                    .child(
                        div()
                            .w(px(64.))
                            .flex_none()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(TX_SM))
                            .text_color(t.tx)
                            .child(s.prefix.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(TX_SM))
                            .text_color(t.tx3)
                            .child(s.description.clone()),
                    )
                    .children(builtin.then(|| {
                        div()
                            .text_size(px(TX_XS))
                            .text_color(t.tx4)
                            .child(t!("sq.builtin").to_string())
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_snippet(p.clone(), window, cx)
                    })),
            );
        }
        let left = div()
            .id("sn-list")
            .w(px(320.))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(t.ln)
            .overflow_y_scroll()
            .child(list);
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
        let right = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .p(px(S6))
            .gap(px(S4))
            .child(
                div()
                    .text_size(px(TX_SM))
                    .text_color(t.tx3)
                    .child(t!("sq.snip_hint").to_string()),
            )
            .child(field(t!("sq.s_prefix").to_string(), &self.s_prefix))
            .child(field(t!("sq.f_desc").to_string(), &self.s_desc))
            .child(
                div()
                    .flex_1()
                    .min_h(px(120.))
                    .border_1()
                    .border_color(t.ln)
                    .rounded(px(R_MD))
                    .overflow_hidden()
                    .child(
                        Editor::new(&self.s_body.read(cx).state.clone())
                            .bordered(false)
                            .h_full(),
                    ),
            )
            .child(
                h_flex()
                    .gap(px(S4))
                    .child(
                        Button::new("sn-new")
                            .label(t!("sq.new_snippet").to_string())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.new_snippet(window, cx)),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("sn-delete")
                            .label(t!("sq.delete").to_string())
                            .disabled(
                                self.sel_snip
                                    .as_ref()
                                    .is_none_or(|p| !user.iter().any(|u| &u.prefix == p)),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.delete_snippet(cx))),
                    )
                    .child(
                        Button::new("sn-save")
                            .primary()
                            .label(t!("common.save").to_string())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.save_snippet(window, cx)),
                            ),
                    ),
            );
        h_flex()
            .flex_1()
            .min_h_0()
            .child(left)
            .child(right)
            .into_any_element()
    }
}

impl Render for SavedPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        let me = cx.entity().downgrade();
        let mode = self.mode;
        let tab = |id: &'static str, label: String, m: Mode, on: bool| {
            let me = me.clone();
            SegItem::new(
                id,
                label,
                on,
                Rc::new(move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    let _ = me.update(cx, |this, cx| {
                        this.mode = m;
                        cx.notify();
                    });
                }),
            )
        };
        let seg_el = seg(
            vec![
                tab(
                    "sq-m-q",
                    t!("sq.queries").to_string(),
                    Mode::Queries,
                    mode == Mode::Queries,
                ),
                tab(
                    "sq-m-s",
                    t!("sq.snippets").to_string(),
                    Mode::Snippets,
                    mode == Mode::Snippets,
                ),
            ],
            cx,
        );
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
                    Input::new(&self.search).px(px(S5)).cleanable(true).prefix(
                        crate::shell::icons::icon("search")
                            .size(px(14.))
                            .text_color(t.tx3),
                    ),
                ),
            )
            .child(seg_el)
            .child(div().flex_1())
            .children((mode == Mode::Queries).then(|| {
                Button::new("sq-import")
                    .label(t!("sq.import").to_string())
                    .on_click(cx.listener(|this, _, window, cx| this.import(window, cx)))
            }))
            .children((mode == Mode::Queries).then(|| {
                Button::new("sq-export")
                    .label(t!("sq.export").to_string())
                    .on_click(cx.listener(|this, _, window, cx| this.export(window, cx)))
            }));
        let body = match mode {
            Mode::Queries => self.render_queries(cx),
            Mode::Snippets => self.render_snippets(cx),
        };
        v_flex()
            .id("saved-page")
            .track_focus(&self.focus)
            .size_full()
            .child(top)
            .child(body)
    }
}
