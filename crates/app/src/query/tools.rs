//! Editor tools: Format SQL, Find / Replace, Explain / Explain Analyze.
use crate::shell::*;
use db_core::classify::{StatementKind, classify, classify_script};
use locus_core::plan::Engine;
use locus_core::store::DbKind;

impl Locus {
    /// Formats the selection, else the whole editor text, as one undo step.
    pub(crate) fn format_sql(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab(cx);
        if tab.read(cx).diagram.is_some() || tab.read(cx).structure.is_some() {
            return;
        }
        let opts = crate::settings::get(cx).format_options();
        let state = tab.read(cx).editor.read(cx).state.clone();
        let (sel, text) = {
            let s = state.read(cx);
            (s.selected_value().to_string(), s.value().to_string())
        };
        let whole = sel.trim().is_empty();
        let src = if whole { text } else { sel };
        if src.trim().is_empty() {
            return;
        }
        let out = db_core::format::format_sql(&src, &opts);
        if out == src {
            // The formatter hands the text back untouched when it is already
            // formatted or when it cannot read it with confidence.
            crate::shell::toast::show(t!("format.unchanged").to_string(), cx);
            return;
        }
        state.update(cx, |s, cx| {
            if whole {
                s.replace_all(out, window, cx)
            } else {
                s.replace(out, window, cx)
            }
        });
        tab.update(cx, |t, cx| {
            t.doc.modified = true;
            t.doc.dirty = true;
            cx.emit(TabEvent::Changed);
            cx.notify();
        });
        crate::shell::toast::show(
            t!(if whole {
                "format.done"
            } else {
                "format.done_selection"
            })
            .to_string(),
            cx,
        );
    }

    /// The editor's own find / replace panel (Ctrl+F / Ctrl+H when focused).
    pub(crate) fn find_replace(
        &mut self,
        replace: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.active_tab(cx);
        if tab.read(cx).diagram.is_some() {
            // Ctrl+F in a diagram: find a table
            tab.update(cx, |t, cx| t.diagram_search(window, cx));
            return;
        }
        if tab.read(cx).structure.is_some() {
            return;
        }
        tab.update(cx, |t, cx| t.open_find(replace, window, cx));
    }

    /// Engine of the active tab's connection when it can explain.
    pub(crate) fn explain_engine(&self, cx: &App) -> Option<Engine> {
        let tab = self.active_tab(cx);
        if tab.read(cx).diagram.is_some() || tab.read(cx).structure.is_some() {
            return None;
        }
        match self.active_conn(cx)?.kind {
            DbKind::Mysql => Some(Engine::Mysql),
            DbKind::Postgres => Some(Engine::Postgres),
            DbKind::Sqlite => Some(Engine::Sqlite),
            DbKind::Oracle | DbKind::SqlServer | DbKind::Redis => None,
        }
    }

    /// Explain (read only) or Explain Analyze (runs the statement: goes through the gate).
    pub(crate) fn explain(&mut self, analyze: bool, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab(cx);
        if tab.read(cx).diagram.is_some() || tab.read(cx).structure.is_some() {
            return;
        }
        let Some(conn) = tab
            .read(cx)
            .conn_id
            .as_ref()
            .and_then(|id| self.conns.iter().find(|c| &c.id == id))
            .cloned()
        else {
            crate::shell::toast::show(t!("error.no_connection").to_string(), cx);
            return;
        };
        let engine = match conn.kind {
            DbKind::Mysql => Engine::Mysql,
            DbKind::Postgres => Engine::Postgres,
            DbKind::Sqlite if analyze => {
                crate::shell::toast::show(t!("sqlite.explain_no_analyze").to_string(), cx);
                return;
            }
            DbKind::Sqlite => Engine::Sqlite,
            DbKind::Oracle | DbKind::SqlServer | DbKind::Redis => {
                crate::shell::toast::show(t!("explain.unsupported_engine").to_string(), cx);
                return;
            }
        };
        if tab.read(cx).busy(cx) || tab.read(cx).ap.applying {
            return;
        }
        let (pick, stmts) = tab.read(cx).sql_for(RunMode::Current, cx);
        let stmt = stmts.first().cloned().unwrap_or(pick);
        let stmt = stmt.trim().trim_end_matches(';').trim().to_string();
        if stmt.is_empty() {
            crate::shell::toast::show(t!("explain.no_statement").to_string(), cx);
            return;
        }
        if !matches!(
            classify(&stmt).kind,
            StatementKind::Select
                | StatementKind::Insert
                | StatementKind::Update
                | StatementKind::Delete
        ) {
            crate::shell::toast::show(t!("explain.unsupported_statement").to_string(), cx);
            return;
        }
        if !analyze {
            // EXPLAIN only reads the plan: no gate, no audit entry.
            self.send_explain(tab, stmt, false, engine, window, cx);
            return;
        }
        // EXPLAIN ANALYZE executes the statement: same lock / confirmation as a run.
        let classified = classify_script(&stmt);
        let go: crate::prod::Cont = {
            let (tab, stmt) = (tab.clone(), stmt.clone());
            std::rc::Rc::new(move |wb, window, cx| {
                wb.send_explain(tab.clone(), stmt.clone(), true, engine, window, cx)
            })
        };
        let preview = format!("EXPLAIN ANALYZE {stmt}");
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

    /// After a successful single SELECT: fills the Plan pane in the background
    /// (plain EXPLAIN, never ANALYZE; the pane is not switched).
    pub(crate) fn auto_plan(&mut self, entry: &locus_core::history::Entry, cx: &mut Context<Self>) {
        if entry.error.is_some() || crate::settings::get(cx).no_auto_plan {
            return;
        }
        let Some(tab) = self
            .spaces
            .iter()
            .flat_map(|s| s.tabs.iter())
            .find(|t| {
                t.read(cx).conn_id.as_deref() == Some(entry.conn_id.as_str())
                    && t.read(cx).last_sql == entry.sql
            })
            .cloned()
        else {
            return;
        };
        let Some(conn) = self.conns.iter().find(|c| c.id == entry.conn_id).cloned() else {
            return;
        };
        let engine = match conn.kind {
            DbKind::Mysql => Engine::Mysql,
            DbKind::Postgres => Engine::Postgres,
            DbKind::Sqlite => Engine::Sqlite,
            DbKind::Oracle | DbKind::SqlServer | DbKind::Redis => return,
        };
        let stmt = entry.sql.trim().trim_end_matches(';').trim().to_string();
        if stmt.is_empty()
            || db_core::sql::split_statements(&stmt).len() != 1
            || classify(&stmt).kind != StatementKind::Select
        {
            return;
        }
        let Ok(password) = self.store.password(&conn.id).map(|p| p.unwrap_or_default()) else {
            return;
        };
        let slot = self.pool.slot(&conn.id);
        let ro = self.pool.ro_flag(&conn.id);
        let rt = self.pool.rt().clone();
        tab.update(cx, |t, cx| {
            t.run_explain(
                stmt,
                false,
                false,
                engine,
                conn,
                Ok(password),
                slot,
                ro,
                rt,
                cx,
            )
        });
    }

    /// Sends the plan statement. Only `explain` and the gate call this.
    fn send_explain(
        &mut self,
        tab: Entity<QueryTab>,
        stmt: String,
        analyze: bool,
        engine: Engine,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = tab
            .read(cx)
            .conn_id
            .as_ref()
            .and_then(|id| self.conns.iter().find(|c| &c.id == id))
            .cloned()
        else {
            crate::shell::toast::show(t!("error.no_connection").to_string(), cx);
            return;
        };
        let password = self
            .store
            .password(&conn.id)
            .map(|p| p.unwrap_or_default())
            .map_err(|e| {
                DbError::new(
                    ErrorKind::Internal,
                    t!("error.credential", msg = e.to_string()).to_string(),
                )
            });
        let slot = self.pool.slot(&conn.id);
        let ro = self.pool.ro_flag(&conn.id);
        let rt = self.pool.rt().clone();
        tab.update(cx, |t, cx| {
            t.run_explain(
                stmt, analyze, true, engine, conn, password, slot, ro, rt, cx,
            )
        });
        cx.notify();
    }
}
