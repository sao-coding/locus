//! Relationship diagram tabs (open / reuse, JOIN insertion, entry points) and
//! read-only DDL tabs.
use crate::shell::*;
use er_diagram::{Depth, Scope};

/// Same diagram: same scope kind and, for a centred one, the same table (the
/// depth is the user's choice); lists compare as sets of tables.
fn same_scope(a: &Scope, b: &Scope) -> bool {
    match (a, b) {
        (Scope::All, Scope::All) => true,
        (Scope::Centre { table: x, .. }, Scope::Centre { table: y, .. }) => {
            x.eq_ignore_ascii_case(y)
        }
        (Scope::Tables(x), Scope::Tables(y)) => {
            let set = |v: &Vec<String>| {
                v.iter()
                    .map(|s| s.to_lowercase())
                    .collect::<std::collections::BTreeSet<_>>()
            };
            set(x) == set(y)
        }
        _ => false,
    }
}

impl Locus {
    /// "Diagram" on a schema in the object tree: a tab holding its ER diagram.
    pub(crate) fn open_diagram(
        &mut self,
        conn_id: String,
        schema: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_diagram_scope(conn_id, schema, Scope::All, window, cx);
    }

    /// Shows the diagram of `(connection, schema)` in `scope`: an open tab
    /// with the same scope is reused, else a new tab opens.
    pub(crate) fn open_diagram_scope(
        &mut self,
        conn_id: String,
        schema: String,
        scope: Scope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let want = Some((conn_id.clone(), schema.clone()));
        let found = self
            .space()
            .tabs
            .iter()
            .find(|t| {
                let t = t.read(cx);
                t.diagram == want && same_scope(&t.diagram_scope(cx), &scope)
            })
            .cloned();
        if let Some(t) = found {
            let id = t.read(cx).id.clone();
            self.show_tab(&id, window, cx);
            return;
        }
        let mut data = self.blank_tab(Some(conn_id), cx);
        data.diagram = want;
        data.diagram_state = serde_json::to_value(crate::diagram::tab::DiagramState {
            scope,
            hidden: Vec::new(),
        })
        .ok();
        let tab = self.make_tab(&data, window, cx);
        let s = self.space_mut();
        s.tabs.push(tab);
        s.show_alone(&data.id, cx);
        self.sync_space_conns(self.active_space, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// "Show relations" on a table: a diagram centred on it, 1 hop.
    pub(crate) fn open_relations(
        &mut self,
        conn_id: String,
        schema: String,
        table: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scope = Scope::Centre {
            table: format!("{schema}.{table}"),
            depth: Depth::Hops(1),
        };
        self.open_diagram_scope(conn_id, schema, scope, window, cx);
    }

    /// The result strip's Relations button: only the tables of the query.
    pub(crate) fn open_query_relations(
        &mut self,
        conn_id: String,
        tables: Vec<(Option<String>, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(schema) = tables
            .iter()
            .find_map(|(s, _)| s.clone())
            .or_else(|| self.default_schema(&conn_id))
        else {
            crate::shell::toast::show(t!("erv3.no_schema").to_string(), cx);
            return;
        };
        let keys = tables
            .into_iter()
            .map(|(s, t)| format!("{}.{t}", s.unwrap_or_else(|| schema.clone())))
            .collect();
        self.open_diagram_scope(conn_id, schema, Scope::Tables(keys), window, cx);
    }

    /// Connection header / palette: the whole-schema diagram of the schema the connection is on.
    pub(crate) fn open_conn_diagram(
        &mut self,
        conn_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.default_schema(&conn_id) {
            Some(schema) => self.open_diagram(conn_id, schema, window, cx),
            None => crate::shell::toast::show(t!("erv3.no_schema").to_string(), cx),
        }
    }

    /// Ctrl+Shift+R: the diagram of the current tab's connection and schema.
    pub(crate) fn open_current_diagram(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab(cx);
        let (diagram, conn) = {
            let t = tab.read(cx);
            (t.diagram.clone(), t.conn_id.clone())
        };
        if let Some((conn, schema)) = diagram {
            self.open_diagram(conn, schema, window, cx);
        } else if let Some(conn) = conn.or_else(|| self.space().connections.first().cloned()) {
            self.open_conn_diagram(conn, window, cx);
        } else {
            crate::shell::toast::show(t!("error.no_connection").to_string(), cx);
        }
    }

    /// The schema a connection is working in: the one the catalog cache knows
    /// as current, the connection's database (MySQL), else the first schema known.
    pub(crate) fn default_schema(&self, conn_id: &str) -> Option<String> {
        let c = self.conns.iter().find(|c| c.id == conn_id)?;
        let g = self.completions.lock();
        let cc = g.get(conn_id);
        if let Some(cur) = cc.and_then(|cc| cc.current.clone().flatten()) {
            return Some(cur);
        }
        if c.kind == locus_core::store::DbKind::Mysql
            && let Some(d) = c.database.clone().filter(|d| !d.is_empty())
        {
            return Some(d);
        }
        cc.and_then(|cc| cc.schemas.as_ref().and_then(|s| s.first().cloned()))
    }

    /// A line was clicked in a diagram: append the JOIN to a query pane of
    /// this Space's split (one undo step) and focus it; with no query pane,
    /// copy it and say so.
    pub(crate) fn insert_join(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        let target = self
            .space()
            .panes()
            .iter()
            .enumerate()
            .find_map(|(ix, id)| {
                let t = self.space().tab(id, cx)?;
                t.read(cx).diagram.is_none().then_some((ix, t))
            });
        let Some((ix, tab)) = target else {
            cx.write_to_clipboard(ClipboardItem::new_string(sql.clone()));
            crate::shell::toast::show(t!("erv3.join_copied", sql = sql).to_string(), cx);
            return;
        };
        let state = tab.read(cx).editor.read(cx).state.clone();
        let title = tab.read(cx).display_title(cx);
        let text = state.read(cx).value().to_string();
        let line = text.matches('\n').count() as u32;
        let col = text
            .rsplit('\n')
            .next()
            .unwrap_or("")
            .encode_utf16()
            .count() as u32;
        let insert = format!(
            "{}{}",
            if text.is_empty() || text.ends_with('\n') {
                ""
            } else {
                "\n"
            },
            sql
        );
        state.update(cx, |s, cx| {
            s.set_cursor_position(
                gpui_kit::component::input::Position::new(line, col),
                window,
                cx,
            );
            s.insert(insert, window, cx);
        });
        self.space_mut().focused = ix;
        self.focus_editor(window, cx);
        crate::shell::toast::show(t!("erv3.join_inserted", tab = title).to_string(), cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Diagram context menu "Show in Schema panel": open the panel filtered to the table.
    pub(crate) fn show_in_schema(
        &mut self,
        table: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_right_panel(Some(RightPanel::Schema), cx);
        self.schema_search
            .update(cx, |s, cx| s.set_value(table, window, cx));
    }

    /// Opens the diagram tab's connection if needed, then loads the diagram.
    pub(crate) fn start_diagram(&mut self, tab: Entity<QueryTab>, cx: &mut Context<Self>) {
        let Some((conn_id, _)) = tab.read(cx).diagram.clone() else {
            return;
        };
        let Some(conn) = self.conns.iter().find(|c| c.id == conn_id).cloned() else {
            tab.update(cx, |t, cx| {
                t.dgm.error = Some(t!("error.no_connection").into_owned());
                cx.notify();
            });
            return;
        };
        let password = self
            .store
            .password(&conn_id)
            .ok()
            .flatten()
            .unwrap_or_default();
        let slot = self.pool.slot(&conn_id);
        let ro = self.pool.ro_flag(&conn_id);
        let rt = self.pool.rt().clone();
        let persist = self.store.persists();
        cx.spawn(async move |this, cx| {
            let res = {
                let mut g = slot.lock().await;
                match g.as_ref() {
                    Some(d) => Ok(d.clone()),
                    None => {
                        match locus_core::driver::AnyDriver::connect(&rt, &conn, password, ro).await
                        {
                            Ok(d) => {
                                let d = Arc::new(d);
                                *g = Some(d.clone());
                                Ok(d)
                            }
                            Err(e) => Err(e),
                        }
                    }
                }
            };
            tab.update(cx, |t, cx| match res {
                Ok(d) => t.load_diagram(d, persist, cx),
                Err(e) => {
                    t.dgm.error = Some(format!(
                        "{}: {}",
                        locus_core::status::error_title(e.kind),
                        e.message
                    ));
                    cx.notify();
                }
            });
            let _ = this.update(cx, |this, cx| this.sync_objects(cx));
        })
        .detach();
    }
}

impl Locus {
    /// "View DDL": reads the CREATE text and opens it in a read-only tab.
    pub(crate) fn show_ddl(
        &mut self,
        conn_id: String,
        schema: String,
        kind: db_core::DbObjectKind,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(d) = self.pool.open_driver(&conn_id) else {
            return;
        };
        let Some(fut) = db_core::Driver::catalog(&*d).map(|c| c.ddl(&schema, kind, &name)) else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let res = fut.await;
            let _ = this.update_in(cx, |this, window, cx| match res {
                Err(e) => crate::shell::toast::show(
                    t!("objects.ddl_error", msg = e.message).to_string(),
                    cx,
                ),
                Ok(ddl) => {
                    let mut data = this.blank_tab(Some(conn_id.clone()), cx);
                    data.sql = ddl;
                    data.title = Some(t!("objects.ddl_title", name = name.clone()).into_owned());
                    let tab = this.make_tab(&data, window, cx);
                    let state = tab.read(cx).editor.read(cx).state.clone();
                    state.update(cx, |s, cx| s.set_readonly(true, cx));
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
}
