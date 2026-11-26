//! Source tabs: the stored source of a package, procedure, function,
//! trigger, type or event, edited in the SQL editor and saved back with
//! "Compile" (Oracle) / "Save to database". The SQL and parsing live in
//! `db_core::routine`; this file runs it through the normal query path and
//! the Production gate.
use crate::shell::scale::*;
use crate::shell::*;
use db_core::dictionary::{Dialect, Row, Stmt};
use db_core::routine::{self, CompileError, Part};
use db_core::{Cell, DbObjectKind, Driver as _, Outcome, QueryEvent, QueryOptions};
use gpui_kit::component::Disableable as _;
use locus_core::driver::AnyDriver;
use locus_core::store::DbKind;
use std::collections::HashMap;
use std::rc::Rc;

/// Which object a Source tab shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRef {
    pub conn_id: String,
    pub schema: String,
    pub kind: DbObjectKind,
    pub name: String,
    pub dialect: Dialect,
    pub mariadb: bool,
}

/// State of a Source tab (kept on its `QueryTab`).
pub struct Source {
    pub r: SourceRef,
    pub part: Part,
    /// Editor text of the part that is not shown.
    pub hidden: HashMap<Part, String>,
    /// Source as the server had it when opened or last saved: the base of
    /// the "changed by someone else" check.
    pub opened: HashMap<Part, String>,
    /// Oracle `ALL_OBJECTS.STATUS` per part.
    pub status: HashMap<Part, String>,
    pub errors: Vec<CompileError>,
    /// Editor line of the unit keyword when the errors were read.
    pub err_offset: u32,
    pub busy: bool,
    /// Result of the last compile / save: (ok, text).
    pub note: Option<(bool, String)>,
}

/// `None` for engines without stored routines (SQLite: its triggers are shown as DDL).
pub fn dialect_of(kind: DbKind) -> Option<Dialect> {
    match kind {
        DbKind::Mysql => Some(Dialect::Mysql),
        DbKind::Postgres => Some(Dialect::Postgres),
        DbKind::Oracle => Some(Dialect::Oracle),
        DbKind::Sqlite | DbKind::SqlServer | DbKind::Redis => None,
    }
}

/// Runs one statement with bound parameters and returns its rows as text.
async fn fetch(driver: &AnyDriver, s: Stmt) -> Result<Vec<Row>, DbError> {
    let mut h = driver.execute(s.sql, s.params, QueryOptions::default());
    let mut cols = 0;
    let mut rows = Vec::new();
    while let Some(ev) = h.next().await {
        match ev {
            QueryEvent::Columns(c) => cols = c.len(),
            QueryEvent::Batch(b) => {
                for r in 0..b.rows() {
                    rows.push(
                        (0..cols)
                            .map(|c| match b.cell(r, c) {
                                Cell::Null => None,
                                Cell::Text(s) => Some(s.to_string()),
                                Cell::Bytes(x) => Some(String::from_utf8_lossy(x).into_owned()),
                            })
                            .collect(),
                    );
                }
            }
            QueryEvent::Finished(Outcome::Failed(e)) => return Err(e),
            QueryEvent::Finished(_) => break,
            _ => {}
        }
    }
    Ok(rows)
}

/// Sends one statement as text (no parameters: a body may hold `?`).
async fn run_text(driver: &AnyDriver, sql: String) -> Result<(), DbError> {
    let mut h = driver.query(sql, QueryOptions::default());
    while let Some(ev) = h.next().await {
        match ev {
            QueryEvent::Finished(Outcome::Failed(e)) => return Err(e),
            QueryEvent::Finished(Outcome::Cancelled) => {
                return Err(DbError::new(ErrorKind::Internal, "cancelled"));
            }
            QueryEvent::Finished(_) => break,
            _ => {}
        }
    }
    Ok(())
}

/// The current source of one part, as an editable CREATE statement.
pub async fn read_source(
    driver: Arc<AnyDriver>,
    r: SourceRef,
    part: Part,
) -> Result<String, DbError> {
    match r.dialect {
        Dialect::Oracle => {
            let s = routine::oracle_source_stmt(&r.schema, r.kind, &r.name, part)
                .ok_or_else(|| DbError::new(ErrorKind::Internal, "not a stored program unit"))?;
            Ok(routine::oracle_source_text(
                &fetch(&driver, s).await?,
                &r.schema,
            ))
        }
        Dialect::Mysql | Dialect::Postgres => {
            let fut = driver.catalog().map(|c| c.ddl(&r.schema, r.kind, &r.name));
            let ddl = match fut {
                Some(f) => f.await?,
                None => return Err(DbError::new(ErrorKind::Internal, "no catalog")),
            };
            Ok(if r.dialect == Dialect::Mysql {
                routine::mysql_qualify(&ddl, &r.schema, r.kind)
            } else {
                routine::postgres_source(&ddl, r.kind)
            })
        }
    }
}

async fn read_status(driver: &AnyDriver, r: &SourceRef) -> HashMap<Part, String> {
    let mut out = HashMap::new();
    let Some(s) = (r.dialect == Dialect::Oracle)
        .then(|| routine::oracle_status_stmt(&r.schema, r.kind, &r.name))
        .flatten()
    else {
        return out;
    };
    if let Ok(rows) = fetch(driver, s).await {
        for row in rows {
            let ty = row.first().cloned().flatten().unwrap_or_default();
            let st = row.get(1).cloned().flatten().unwrap_or_default();
            let part = if ty.ends_with("BODY") {
                Part::Body
            } else {
                Part::Spec
            };
            out.insert(part, st);
        }
    }
    out
}

struct Opened {
    spec: String,
    body: Option<String>,
    status: HashMap<Part, String>,
}

async fn read_all(driver: Arc<AnyDriver>, r: SourceRef) -> Result<Opened, DbError> {
    let spec = read_source(driver.clone(), r.clone(), Part::Spec).await?;
    let body = if routine::has_body(r.dialect, r.kind) {
        Some(read_source(driver.clone(), r.clone(), Part::Body).await?)
    } else {
        None
    };
    let status = read_status(&driver, &r).await;
    Ok(Opened { spec, body, status })
}

struct Saved {
    /// The CREATE was stored but does not compile (Oracle).
    with_errors: bool,
    errors: Vec<CompileError>,
    status: HashMap<Part, String>,
    fresh: HashMap<Part, String>,
}

async fn save(
    driver: Arc<AnyDriver>,
    r: SourceRef,
    part: Part,
    statements: Vec<String>,
) -> Result<Saved, DbError> {
    let mut with_errors = false;
    for sql in statements {
        match run_text(&driver, sql).await {
            Ok(()) => {}
            Err(e)
                if r.dialect == Dialect::Oracle && routine::is_compiled_with_errors(&e.message) =>
            {
                with_errors = true
            }
            Err(e) => return Err(e),
        }
    }
    let mut errors = Vec::new();
    if r.dialect == Dialect::Oracle
        && let Some(s) = routine::oracle_errors_stmt(&r.schema, r.kind, &r.name, part)
    {
        errors = routine::parse_errors(&fetch(&driver, s).await?);
    }
    let status = read_status(&driver, &r).await;
    // Both parts: compiling one can change the other's status and source.
    let mut fresh = HashMap::new();
    let parts: &[Part] = if routine::has_body(r.dialect, r.kind) {
        &[Part::Spec, Part::Body]
    } else {
        &[Part::Spec]
    };
    for p in parts {
        if let Ok(f) = read_source(driver.clone(), r.clone(), *p).await {
            fresh.insert(*p, f);
        }
    }
    Ok(Saved {
        with_errors: with_errors || errors.iter().any(|e| !e.warning),
        errors,
        status,
        fresh,
    })
}

// ---- the tab ---------------------------------------------------------------

impl QueryTab {
    fn editor_text(&self, cx: &App) -> String {
        self.editor.read(cx).state.read(cx).value().to_string()
    }

    fn set_editor_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.doc.baseline = text.clone();
        let state = self.editor.read(cx).state.clone();
        state.update(cx, |s, cx| s.set_value(text, window, cx));
    }

    /// Spec / Body switch: the hidden part keeps its unsaved text.
    pub(crate) fn switch_part(&mut self, part: Part, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.editor_text(cx);
        let Some(src) = self.routine.as_mut() else {
            return;
        };
        if src.part == part {
            return;
        }
        src.hidden.insert(src.part, current);
        let text = src.hidden.remove(&part).unwrap_or_default();
        src.part = part;
        src.errors.clear();
        src.note = None;
        self.set_editor_text(text, window, cx);
        cx.notify();
    }

    fn goto_error(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(src) = &self.routine else { return };
        let Some(e) = src.errors.get(ix) else { return };
        let line = e.line.saturating_sub(1) + src.err_offset;
        let col = e.col.saturating_sub(1);
        let state = self.editor.read(cx).state.clone();
        state.update(cx, |s, cx| {
            s.set_cursor_position(
                gpui_kit::component::input::Position::new(line, col),
                window,
                cx,
            )
        });
        state.read(cx).focus_handle(cx).focus(window, cx);
    }

    /// Toolbar above the editor of a Source tab.
    pub(crate) fn render_routine_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let src = self.routine.as_ref()?;
        let t = crate::shell::theme::tokens(cx);
        let mut bar = h_flex()
            .flex_none()
            .gap(px(S4))
            .px(px(S5))
            .py(px(S2))
            .items_center()
            .text_size(px(TX_SM))
            .border_b_1()
            .border_color(t.ln)
            .child(
                crate::shell::icons::icon("script")
                    .xsmall()
                    .text_color(t.tx3),
            )
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("{}.{}", src.r.schema, src.r.name)),
            )
            .child(
                div()
                    .text_color(t.tx3)
                    .child(crate::connections::objects::kind_label(src.r.kind)),
            );
        if let Some(st) = src.status.get(&src.part) {
            let bad = st != "VALID";
            bar = bar.child(
                div()
                    .px(px(S3))
                    .rounded(px(4.))
                    .text_size(px(TX_XS))
                    .font_weight(FontWeight::SEMIBOLD)
                    .bg(if bad { t.red } else { t.grn })
                    .text_color(if bad { t.red_text } else { t.grn_text })
                    .child(
                        if bad {
                            t!("routines.invalid")
                        } else {
                            t!("routines.valid")
                        }
                        .into_owned(),
                    ),
            );
        }
        if routine::has_body(src.r.dialect, src.r.kind) {
            let me = cx.entity().downgrade();
            let item = |part: Part, label: String, id: &'static str| {
                let me = me.clone();
                crate::shell::seg::SegItem::new(
                    id,
                    label,
                    src.part == part,
                    Rc::new(move |_, window, cx| {
                        let _ = me.update(cx, |t, cx| t.switch_part(part, window, cx));
                    }),
                )
            };
            bar = bar.child(crate::shell::seg::seg(
                vec![
                    item(Part::Spec, t!("routines.spec").into_owned(), "src-spec"),
                    item(Part::Body, t!("routines.body").into_owned(), "src-body"),
                ],
                cx,
            ));
        }
        bar = bar
            .child(div().flex_1())
            .children(src.note.as_ref().map(|(ok, text)| {
                div()
                    .max_w(px(360.))
                    .truncate()
                    .text_color(if *ok { t.grn_text } else { t.red_text })
                    .child(text.clone())
            }));
        let label = if src.r.dialect == Dialect::Oracle {
            t!("routines.compile")
        } else {
            t!("routines.save")
        };
        let busy = src.busy;
        bar = bar
            .child(
                Button::new("src-reload")
                    .h(px(H_ROW))
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .tooltip(t!("routines.reload").to_string())
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if !busy {
                            cx.emit(TabEvent::ReloadSource);
                        }
                    })),
            )
            .child(
                Button::new("src-compile")
                    .h(px(H_ROW))
                    .primary()
                    .loading(busy)
                    .label(label.into_owned())
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if !busy {
                            cx.emit(TabEvent::CompileSource);
                        }
                    })),
            );
        Some(bar.into_any_element())
    }

    /// Error list under the editor of a Source tab (replaces the results).
    pub(crate) fn render_routine_errors(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let src = self.routine.as_ref()?;
        let t = crate::shell::theme::tokens(cx);
        if src.errors.is_empty() {
            let text = if src.dialect_is_oracle() {
                t!("routines.no_errors")
            } else {
                t!("routines.save_hint")
            };
            return Some(crate::query::tab::empty(text.into_owned(), cx));
        }
        let mut list = v_flex()
            .id("src-errors")
            .size_full()
            .overflow_y_scroll()
            .text_size(px(TX_SM));
        list =
            list.child(
                div().px(px(S5)).py(px(S2)).text_color(t.tx3).child(
                    t!("routines.errors_title", n = src.errors.len().to_string()).into_owned(),
                ),
            );
        for (ix, e) in src.errors.iter().enumerate() {
            list = list.child(
                h_flex()
                    .id(("src-err", ix))
                    .gap(px(S4))
                    .px(px(S5))
                    .py(px(S1))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.hv))
                    .child(
                        Icon::new(if e.warning {
                            IconName::TriangleAlert
                        } else {
                            IconName::CircleX
                        })
                        .xsmall()
                        .flex_none()
                        .text_color(if e.warning {
                            t.yel_text
                        } else {
                            t.red_text
                        }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(72.))
                            .font_family("monospace")
                            .text_color(t.tx3)
                            .child(format!("{}:{}", e.line, e.col)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_normal()
                            .child(e.text.clone()),
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.goto_error(ix, window, cx)),
                    ),
            );
        }
        Some(list.into_any_element())
    }
}

impl Source {
    fn dialect_is_oracle(&self) -> bool {
        self.r.dialect == Dialect::Oracle
    }
}

// ---- the shell side ----------------------------------------------------

impl Locus {
    /// Opens (or shows) the Source tab of an object from the object tree.
    pub(crate) fn open_source(
        &mut self,
        conn_id: String,
        schema: String,
        kind: DbObjectKind,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.conns.iter().find(|c| c.id == conn_id).cloned() else {
            return;
        };
        let Some(dialect) = dialect_of(conn.kind).filter(|d| routine::editable(*d, kind)) else {
            return self.show_ddl(conn_id, schema, kind, name, window, cx);
        };
        let mariadb = self
            .conn_version(&conn_id)
            .is_some_and(|v| v.contains("MariaDB"));
        let r = SourceRef {
            conn_id: conn_id.clone(),
            schema,
            kind,
            name,
            dialect,
            mariadb,
        };
        let same = |s: &Source| {
            s.r.conn_id == r.conn_id
                && s.r.schema == r.schema
                && s.r.kind == r.kind
                && s.r.name == r.name
        };
        let found = self
            .space()
            .tabs
            .iter()
            .find(|t| t.read(cx).routine.as_ref().is_some_and(same))
            .cloned();
        if let Some(t) = found {
            let id = t.read(cx).id.clone();
            return self.show_tab(&id, window, cx);
        }
        let Some(driver) = self.pool.open_driver(&conn_id) else {
            crate::shell::toast::show(t!("error.no_connection").to_string(), cx);
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let res = read_all(driver, r.clone()).await;
            let _ = this.update_in(cx, |this, window, cx| match res {
                Err(e) => crate::shell::toast::show(
                    t!("routines.open_error", msg = e.message).to_string(),
                    cx,
                ),
                Ok(o) => {
                    let mut data = this.blank_tab(Some(r.conn_id.clone()), cx);
                    data.sql = o.spec.clone();
                    data.title = Some(r.name.clone());
                    let tab = this.make_tab(&data, window, cx);
                    let mut opened = HashMap::from([(Part::Spec, o.spec)]);
                    let mut hidden = HashMap::new();
                    if let Some(b) = o.body {
                        opened.insert(Part::Body, b.clone());
                        hidden.insert(Part::Body, b);
                    }
                    tab.update(cx, |t, _| {
                        t.routine = Some(Source {
                            r,
                            part: Part::Spec,
                            hidden,
                            opened,
                            status: o.status,
                            errors: Vec::new(),
                            err_offset: 0,
                            busy: false,
                            note: None,
                        });
                    });
                    let s = this.space_mut();
                    s.tabs.push(tab);
                    s.show_alone(&data.id, cx);
                    this.sync_space_conns(this.active_space, cx);
                    this.schedule_save(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Reload: the server's source replaces both parts (unsaved edits are lost).
    pub(crate) fn reload_source(
        &mut self,
        tab: Entity<QueryTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(r) = tab.read(cx).routine.as_ref().map(|s| s.r.clone()) else {
            return;
        };
        let Some(driver) = self.pool.open_driver(&r.conn_id) else {
            crate::shell::toast::show(t!("error.no_connection").to_string(), cx);
            return;
        };
        cx.spawn_in(window, async move |_, cx| {
            let res = read_all(driver, r).await;
            let _ = tab.update_in(cx, |t, window, cx| match res {
                Err(e) => crate::shell::toast::show(
                    t!("routines.open_error", msg = e.message).to_string(),
                    cx,
                ),
                Ok(o) => {
                    let Some(src) = t.routine.as_mut() else {
                        return;
                    };
                    let part = src.part;
                    src.opened = HashMap::from([(Part::Spec, o.spec.clone())]);
                    src.hidden.clear();
                    if let Some(b) = o.body {
                        src.opened.insert(Part::Body, b);
                    }
                    for (p, text) in src.opened.clone() {
                        if p != part {
                            src.hidden.insert(p, text);
                        }
                    }
                    src.status = o.status;
                    src.errors.clear();
                    src.note = None;
                    let text = src.opened.get(&part).cloned().unwrap_or_default();
                    t.set_editor_text(text, window, cx);
                    t.doc.modified = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Compile / Save: conflict check, MySQL DROP + CREATE confirmation, the
    /// Production gate, then the save itself.
    pub(crate) fn compile_source(
        &mut self,
        tab: Entity<QueryTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A restored tab did not know the server flavour yet.
        if let Some(id) = tab.read(cx).routine.as_ref().map(|s| s.r.conn_id.clone())
            && let Some(v) = self.conn_version(&id)
        {
            tab.update(cx, |t, _| {
                if let Some(s) = t.routine.as_mut() {
                    s.r.mariadb = v.contains("MariaDB");
                }
            });
        }
        let (r, part, opened, text) = {
            let t = tab.read(cx);
            let Some(src) = t.routine.as_ref() else {
                return;
            };
            if src.busy {
                return;
            }
            (
                src.r.clone(),
                src.part,
                src.opened.get(&src.part).cloned().unwrap_or_default(),
                t.editor.read(cx).state.read(cx).value().to_string(),
            )
        };
        if text.trim().is_empty() {
            crate::shell::toast::show(t!("routines.empty").to_string(), cx);
            return;
        }
        let Some(driver) = self.pool.open_driver(&r.conn_id) else {
            crate::shell::toast::show(t!("error.no_connection").to_string(), cx);
            return;
        };
        if let Some(want) = routine::oracle_unit_part(r.kind, &text)
            && want != part
        {
            let name = |p: Part| {
                if p == Part::Body {
                    t!("wavea.part_body")
                } else {
                    t!("wavea.part_spec")
                }
                .into_owned()
            };
            let msg = t!("wavea.wrong_part", want = name(want), active = name(part)).into_owned();
            tab.update(cx, |t, cx| {
                if let Some(s) = t.routine.as_mut() {
                    s.note = Some((false, msg.clone()));
                }
                cx.notify();
            });
            crate::shell::toast::show(msg, cx);
            return;
        }
        set_busy(&tab, true, cx);
        cx.spawn_in(window, async move |this, cx| {
            // Changed by someone else since it was opened? (A read error is no conflict.)
            let fresh = read_source(driver, r, part).await.ok();
            let _ = this.update_in(cx, |this, window, cx| {
                set_busy(&tab, false, cx);
                match fresh {
                    Some(f) if !opened.trim().is_empty() && !routine::same_source(&f, &opened) => {
                        this.open_conflict(tab, f, window, cx)
                    }
                    _ => this.confirm_save(tab, window, cx),
                }
            });
        })
        .detach();
    }

    fn open_conflict(
        &mut self,
        tab: Entity<QueryTab>,
        fresh: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let me = cx.entity().downgrade();
        window.open_pop_dialog(cx, move |dialog, _, cx| {
            let t = crate::shell::theme::tokens(cx);
            let (me1, me2, tab1, tab2, fresh1) = (
                me.clone(),
                me.clone(),
                tab.clone(),
                tab.clone(),
                fresh.clone(),
            );
            dialog
                .title(t!("routines.conflict_title").to_string())
                .w(px(W_DLG_M))
                .child(
                    v_flex()
                        .gap(px(S5))
                        .child(
                            div()
                                .text_size(px(TX_SM))
                                .text_color(t.tx2)
                                .whitespace_normal()
                                .child(t!("routines.conflict_body").into_owned()),
                        )
                        .child(
                            h_flex()
                                .gap(px(S4))
                                .child(div().flex_1())
                                .child(
                                    Button::new("cf-cancel")
                                        .h(px(H_ROW))
                                        .label(t!("common.cancel").to_string())
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                )
                                .child(
                                    Button::new("cf-reload")
                                        .h(px(H_ROW))
                                        .label(t!("routines.reload").to_string())
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let tab = tab1.clone();
                                            let _ = me1.update(cx, |this, cx| {
                                                this.reload_source(tab, window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("cf-overwrite")
                                        .h(px(H_ROW))
                                        .primary()
                                        .label(t!("routines.overwrite").to_string())
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let tab = tab2.clone();
                                            // Overwrite: the server's text becomes the new base.
                                            tab.update(cx, |t, _| {
                                                if let Some(s) = t.routine.as_mut() {
                                                    s.opened.insert(s.part, fresh1.clone());
                                                }
                                            });
                                            let _ = me2.update(cx, |this, cx| {
                                                this.confirm_save(tab, window, cx)
                                            });
                                        }),
                                ),
                        ),
                )
        });
    }

    /// MySQL (not MariaDB) has no CREATE OR REPLACE for routines: confirm the
    /// non-atomic DROP + CREATE first.
    /// Oracle: an object the session user does not own, or any object while
    /// connected as APPS, is shared: name it and ask for its name first.
    fn confirm_save(&mut self, tab: Entity<QueryTab>, window: &mut Window, cx: &mut Context<Self>) {
        let Some((r, conn)) = tab
            .read(cx)
            .routine
            .as_ref()
            .map(|s| s.r.clone())
            .and_then(|r| {
                self.conns
                    .iter()
                    .find(|c| c.id == r.conn_id)
                    .cloned()
                    .map(|c| (r, c))
            })
        else {
            return;
        };
        let why = (r.dialect == Dialect::Oracle)
            .then(|| routine::shared_object(&conn.user, &r.schema))
            .flatten();
        let Some(why) = why else {
            return self.confirm_drop_create(tab, window, cx);
        };
        let me = cx.entity().downgrade();
        let typed = cx.new(|cx| InputState::new(window, cx));
        let first = std::cell::Cell::new(true);
        window.open_pop_dialog(cx, move |dialog, window, cx| {
            let t = crate::shell::theme::tokens(cx);
            if first.replace(false) {
                typed.read(cx).focus_handle(cx).focus(window, cx);
            }
            let ok = typed.read(cx).value().trim().eq_ignore_ascii_case(&r.name);
            let text = match why {
                routine::Shared::NotOwner => t!(
                    "wavea.shared_not_owner",
                    user = conn.user.clone(),
                    owner = r.schema.clone(),
                    name = r.name.clone()
                ),
                routine::Shared::Privileged => t!(
                    "wavea.shared_apps",
                    owner = r.schema.clone(),
                    name = r.name.clone()
                ),
            }
            .into_owned();
            let go: Rc<dyn Fn(&mut Window, &mut App)> = {
                let (me, tab) = (me.clone(), tab.clone());
                Rc::new(move |window, cx| {
                    window.close_dialog(cx);
                    let tab = tab.clone();
                    let _ = me.update(cx, |this, cx| this.confirm_drop_create(tab, window, cx));
                })
            };
            let (go_click, go_enter) = (go.clone(), go);
            dialog
                .title(t!("wavea.shared_title").to_string())
                .w(px(W_DLG_M))
                .child(
                    v_flex()
                        .gap(px(S5))
                        .child(
                            div()
                                .text_size(px(TX_SM))
                                .text_color(t.red_text)
                                .whitespace_normal()
                                .child(text),
                        )
                        .child(
                            div()
                                .text_size(px(TX_SM))
                                .text_color(t.tx3)
                                .child(t!("wavea.shared_type", name = r.name.clone()).into_owned()),
                        )
                        .child(Input::new(&typed).px(px(S5)))
                        .child(
                            h_flex()
                                .gap(px(S4))
                                .child(div().flex_1())
                                .child(
                                    Button::new("sh-cancel")
                                        .h(px(H_ROW))
                                        .label(t!("common.cancel").to_string())
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                )
                                .child(
                                    Button::new("sh-go")
                                        .h(px(H_ROW))
                                        .primary()
                                        .disabled(!ok)
                                        .label(t!("routines.compile").to_string())
                                        .on_click(move |_, window, cx| go_click(window, cx)),
                                ),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    if ok {
                        go_enter(window, cx);
                    }
                    false
                })
        });
    }

    fn confirm_drop_create(
        &mut self,
        tab: Entity<QueryTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(plan) = save_plan_of(&tab, cx) else {
            return;
        };
        if plan.atomic {
            return self.gate_save(tab, window, cx);
        }
        let me = cx.entity().downgrade();
        window.open_pop_dialog(cx, move |dialog, _, cx| {
            let t = crate::shell::theme::tokens(cx);
            let (me, tab) = (me.clone(), tab.clone());
            dialog
                .title(t!("routines.drop_create_title").to_string())
                .w(px(W_DLG_M))
                .child(
                    v_flex()
                        .gap(px(S5))
                        .child(
                            div()
                                .text_size(px(TX_SM))
                                .text_color(t.tx2)
                                .whitespace_normal()
                                .child(t!("routines.drop_create_body").into_owned()),
                        )
                        .child(
                            h_flex()
                                .gap(px(S4))
                                .child(div().flex_1())
                                .child(
                                    Button::new("dc-cancel")
                                        .h(px(H_ROW))
                                        .label(t!("common.cancel").to_string())
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                )
                                .child(
                                    Button::new("dc-go")
                                        .h(px(H_ROW))
                                        .primary()
                                        .label(t!("routines.save").to_string())
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let tab = tab.clone();
                                            let _ = me.update(cx, |this, cx| {
                                                this.gate_save(tab, window, cx)
                                            });
                                        }),
                                ),
                        ),
                )
        });
    }

    fn gate_save(&mut self, tab: Entity<QueryTab>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = save_plan_of(&tab, cx) else {
            return;
        };
        let Some(conn_id) = tab.read(cx).routine.as_ref().map(|s| s.r.conn_id.clone()) else {
            return;
        };
        let Some(conn) = self.conns.iter().find(|c| c.id == conn_id).cloned() else {
            return;
        };
        // Each statement is classified whole: a body is never split on its `;`.
        let classified = plan
            .statements
            .iter()
            .map(|s| db_core::classify::classify(s))
            .collect();
        let preview = plan.statements.join("\n;\n");
        let go: crate::prod::Cont = {
            let tab = tab.clone();
            Rc::new(move |wb, window, cx| wb.run_save(tab.clone(), window, cx))
        };
        self.gate(
            crate::prod::Gated {
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

    /// Sends the save. Only the gate calls this.
    fn run_save(&mut self, tab: Entity<QueryTab>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = save_plan_of(&tab, cx) else {
            return;
        };
        let (r, part, text) = {
            let t = tab.read(cx);
            let Some(s) = t.routine.as_ref() else { return };
            (
                s.r.clone(),
                s.part,
                t.editor.read(cx).state.read(cx).value().to_string(),
            )
        };
        let Some(driver) = self.pool.open_driver(&r.conn_id) else {
            crate::shell::toast::show(t!("error.no_connection").to_string(), cx);
            return;
        };
        let offset = routine::error_line_offset(&text);
        set_busy(&tab, true, cx);
        let conn_id = r.conn_id.clone();
        let oracle = r.dialect == Dialect::Oracle;
        cx.spawn_in(window, async move |this, cx| {
            let res = save(driver, r, part, plan.statements).await;
            tab.update(cx, |t, cx| {
                let Some(src) = t.routine.as_mut() else {
                    return;
                };
                src.busy = false;
                match res {
                    Err(e) => {
                        src.errors.clear();
                        src.note = Some((
                            false,
                            t!("routines.save_failed", msg = e.message).into_owned(),
                        ));
                    }
                    Ok(s) => {
                        src.err_offset = offset;
                        src.errors = s.errors;
                        if !s.status.is_empty() {
                            src.status = s.status;
                        }
                        for (p, f) in s.fresh {
                            // The hidden part's unsaved text stays; only its baseline moves.
                            src.opened.insert(p, f);
                        }
                        src.note = Some(if s.with_errors {
                            (false, t!("routines.compiled_errors").into_owned())
                        } else if oracle {
                            (true, t!("routines.compiled_ok").into_owned())
                        } else {
                            (true, t!("routines.saved_ok").into_owned())
                        });
                        t.doc.baseline = text;
                        t.doc.modified = false;
                    }
                }
                cx.notify();
            });
            let _ = this.update(cx, |this, cx| {
                this.after_run(&conn_id, cx);
                this.objects.update(cx, |o, cx| o.refresh(cx));
            });
        })
        .detach();
    }
}

fn set_busy(tab: &Entity<QueryTab>, busy: bool, cx: &mut App) {
    tab.update(cx, |t, cx| {
        if let Some(s) = t.routine.as_mut() {
            s.busy = busy;
            cx.notify();
        }
    });
}

fn save_plan_of(tab: &Entity<QueryTab>, cx: &App) -> Option<routine::SavePlan> {
    let t = tab.read(cx);
    let s = t.routine.as_ref()?;
    let text = t.editor.read(cx).state.read(cx).value().to_string();
    Some(routine::save_plan(
        s.r.dialect,
        s.r.mariadb,
        &s.r.schema,
        s.r.kind,
        &s.r.name,
        &text,
    ))
}

// ---- restoring Source / Structure tabs after a restart ------------------------

/// `DbObjectKind` from the debug name stored in `spaces.json`.
fn kind_from_name(s: &str) -> Option<DbObjectKind> {
    use DbObjectKind::*;
    [
        Table,
        View,
        MaterializedView,
        Index,
        Sequence,
        Procedure,
        Function,
        Package,
        Trigger,
        Event,
        Synonym,
        Type,
        Extension,
    ]
    .into_iter()
    .find(|k| format!("{k:?}") == s)
}

impl QueryTab {
    /// The object a Source / Structure tab shows, for `spaces.json`.
    pub fn obj_ref(&self) -> Option<locus_core::spaces::ObjRef> {
        use locus_core::spaces::{ObjRef, ObjTab};
        if let Some(s) = &self.routine {
            return Some(ObjRef {
                tab: ObjTab::Source,
                schema: s.r.schema.clone(),
                kind: format!("{:?}", s.r.kind),
                name: s.r.name.clone(),
            });
        }
        self.structure.as_ref().map(|s| ObjRef {
            tab: ObjTab::Structure,
            schema: s.r.schema.clone(),
            kind: "Table".into(),
            name: s.r.table.clone(),
        })
    }
}

impl Locus {
    /// Gives a tab restored from `spaces.json` its Source / Structure state
    /// back (called by `make_tab`). The editor text is the one saved with the
    /// tab, so unsaved edits survive; the server baseline and status are read
    /// again when the connection is open.
    pub(crate) fn attach_object(
        &mut self,
        tab: &Entity<QueryTab>,
        data: &TabData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use locus_core::spaces::ObjTab;
        let (Some(o), Some(conn_id)) = (data.obj.as_ref(), data.conn_id.clone()) else {
            return;
        };
        match o.tab {
            ObjTab::Structure => {
                let r = crate::structure::StructureRef {
                    conn_id,
                    schema: o.schema.clone(),
                    table: o.name.clone(),
                };
                let state = tab.read(cx).editor.read(cx).state.clone();
                state.update(cx, |s, cx| s.set_readonly(true, cx));
                tab.update(cx, |t, _| {
                    t.structure = Some(crate::structure::Structure {
                        section: crate::structure::last_section(&r),
                        r,
                        data: crate::structure::Load::Loading,
                        ddl: crate::structure::Load::Loading,
                        hscroll: Default::default(),
                    })
                });
                self.load_structure(tab.clone(), false, window, cx);
            }
            ObjTab::Source => {
                let Some(kind) = kind_from_name(&o.kind) else {
                    return;
                };
                let Some(conn) = self.conns.iter().find(|c| c.id == conn_id).cloned() else {
                    return;
                };
                let Some(dialect) = dialect_of(conn.kind).filter(|d| routine::editable(*d, kind))
                else {
                    return;
                };
                let mariadb = self
                    .conn_version(&conn_id)
                    .is_some_and(|v| v.contains("MariaDB"));
                let r = SourceRef {
                    conn_id: conn_id.clone(),
                    schema: o.schema.clone(),
                    kind,
                    name: o.name.clone(),
                    dialect,
                    mariadb,
                };
                tab.update(cx, |t, _| {
                    t.routine = Some(Source {
                        r: r.clone(),
                        part: Part::Spec,
                        hidden: HashMap::new(),
                        opened: HashMap::new(),
                        status: HashMap::new(),
                        errors: Vec::new(),
                        err_offset: 0,
                        busy: false,
                        note: None,
                    });
                });
                // Baseline and status from the server, when it is reachable already.
                let Some(driver) = self.pool.open_driver(&conn_id) else {
                    return;
                };
                let tab = tab.clone();
                cx.spawn_in(window, async move |_, cx| {
                    let Ok(o) = read_all(driver, r).await else {
                        return;
                    };
                    tab.update(cx, |t, cx| {
                        let Some(src) = t.routine.as_mut() else {
                            return;
                        };
                        src.opened = HashMap::from([(Part::Spec, o.spec)]);
                        if let Some(b) = o.body {
                            src.opened.insert(Part::Body, b.clone());
                            src.hidden.entry(Part::Body).or_insert(b);
                        }
                        src.status = o.status;
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }
}
