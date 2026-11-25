//! Query flow: run / cancel a tab, connection pick, close while busy, autocomplete target.
use crate::shell::*;

impl Locus {
    // ---- query flow -------------------------------------------------------

    #[cfg(feature = "dev")]
    pub fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab(cx);
        self.run_tab(tab, RunMode::Current, None, window, cx);
    }

    /// Runs `sql` in the active tab (the editor is not touched).
    #[cfg(feature = "dev")]
    pub fn run_text(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab(cx);
        self.run_tab(tab, RunMode::Script, Some(sql), window, cx);
    }

    #[cfg(feature = "dev")]
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.active_tab(cx).update(cx, |t, cx| t.cancel(cx));
    }

    pub(crate) fn run_tab(
        &mut self,
        tab: Entity<QueryTab>,
        mode: RunMode,
        sql: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if tab.read(cx).structure.is_some() {
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
        let (sql, stmts) = match sql {
            Some(s) => (s, Vec::new()),
            None => tab.read(cx).sql_for(mode, cx),
        };
        let sql = match stmts.len() <= 1 {
            true => tab
                .update(cx, |t, _| t.saved_view_sql(&sql, &conn))
                .unwrap_or(sql),
            false => sql,
        };
        if tab.read(cx).busy(cx) || tab.read(cx).ap.applying || sql.trim().is_empty() {
            return;
        }
        self.run_prepared(tab, conn, sql, stmts, None, window, cx);
    }

    /// The run after the script is picked: client commands (`@set`, `@unset`,
    /// `@echo`) and `${var}` text substitution first, then `:name` placeholders
    /// (bound, never pasted). What the Production gate, the audit and the driver
    /// see is the statement after the substitution.
    ///
    /// `given` is the Run dialog's answer: parameter values by name, and no
    /// dialog again. Without it, an undefined `${var}` or a placeholder opens the dialog.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_prepared(
        &mut self,
        tab: Entity<QueryTab>,
        conn: SavedConnection,
        raw: String,
        raw_stmts: Vec<String>,
        given: Option<std::collections::HashMap<String, locus_core::params::ParamValue>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use locus_core::{params, vars};
        let prep = vars::prepare(&raw, &tab.read(cx).bind.vars);
        // `@set` / `@unset` stay for the tab, even when the run is not sent.
        tab.update(cx, |t, _| {
            t.bind.vars = prep.vars.clone();
            t.completion.lock().vars = prep.vars.keys().cloned().collect();
            t.bind.echoes.clear();
        });
        if !prep.errors.is_empty() {
            crate::shell::toast::show(
                t!("vars.bad_command", line = prep.errors[0].as_str()).to_string(),
                cx,
            );
            return;
        }
        let sql = prep.sql.clone();
        if sql.trim().is_empty() {
            // Only commands: nothing to send; `@echo` shows in Messages.
            if prep.had_commands {
                tab.update(cx, |t, cx| {
                    t.out.echoes = prep.echoes.clone();
                    t.out.active = t.out.results.len();
                    cx.notify();
                });
                crate::shell::toast::show(t!("vars.done", n = prep.vars.len()).to_string(), cx);
            }
            return;
        }
        // The statements of the picked text without the command lines.
        let stmts = if raw_stmts.is_empty() {
            Vec::new()
        } else if prep.had_commands || prep.sql != raw {
            db_core::sql::split_statements(&sql)
                .into_iter()
                .map(|r| sql[r].to_string())
                .collect()
        } else {
            raw_stmts.clone()
        };
        let opts = crate::settings::get(cx).param_opts();
        let phs = params::detect_with(&sql, conn.kind, &opts);
        if given.is_none() && (!phs.is_empty() || !prep.undefined.is_empty()) {
            if !phs.is_empty() && stmts.len() > 1 {
                crate::shell::toast::show(t!("qp.one_statement").to_string(), cx);
                return;
            }
            self.open_params(tab, conn, raw, raw_stmts, phs, prep.undefined, window, cx);
            return;
        }
        let echoes = prep.echoes;
        match given {
            Some(values) if !phs.is_empty() => {
                if stmts.len() > 1 {
                    crate::shell::toast::show(t!("qp.one_statement").to_string(), cx);
                    return;
                }
                let (send, bound) = params::bind(&sql, &phs, &values);
                let written = params::names(&phs)
                    .iter()
                    .filter_map(|n| {
                        values
                            .get(n)
                            .map(|v| locus_core::history::HistParam::from_value(n, v))
                    })
                    .collect();
                self.gate_run(
                    tab,
                    conn,
                    sql,
                    send,
                    Vec::new(),
                    bound,
                    written,
                    echoes,
                    window,
                    cx,
                );
            }
            _ => self.gate_run(
                tab,
                conn,
                sql.clone(),
                sql,
                stmts,
                Vec::new(),
                Vec::new(),
                echoes,
                window,
                cx,
            ),
        }
    }

    /// Every run goes through the Production gate (lock, confirmation, audit).
    /// `shown` is the statement as written (what the classifier, the dialogs
    /// and the audit see); `sql` is what is sent, with `bound` as bound values.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn gate_run(
        &mut self,
        tab: Entity<QueryTab>,
        conn: SavedConnection,
        shown: String,
        sql: String,
        stmts: Vec<String>,
        bound: Vec<db_core::Param>,
        written: Vec<locus_core::history::HistParam>,
        echoes: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let classified = Self::classify_run(&shown);
        let go: crate::prod::Cont = {
            let (tab, shown) = (tab.clone(), shown.clone());
            std::rc::Rc::new(move |wb, window, cx| {
                tab.update(cx, |t, _| {
                    t.bind.bound = bound.clone();
                    t.bind.echoes = echoes.clone();
                    t.bind.hist_written =
                        (!written.is_empty()).then(|| (shown.clone(), written.clone()));
                });
                wb.send_run(tab.clone(), sql.clone(), stmts.clone(), window, cx)
            })
        };
        self.gate(
            crate::prod::Gated {
                tab: Some(tab),
                conn,
                classified,
                preview: shown,
                go,
            },
            window,
            cx,
        );
    }

    /// Sends `sql` on the tab's connection. Only the gate calls this.
    fn send_run(
        &mut self,
        tab: Entity<QueryTab>,
        sql: String,
        stmts: Vec<String>,
        window: &mut Window,
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
            t.run_sql(sql, stmts, conn, password, slot, ro, rt, window, cx)
        });
        cx.notify();
    }

    pub(crate) fn set_tab_connection(
        &mut self,
        tab: Entity<QueryTab>,
        id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if tab.read(cx).conn_id == id {
            return;
        }
        // An open transaction is ended (Commit / Rollback) before the switch.
        if tab.read(cx).tx.needs_prompt() {
            let (t2, id2) = (tab.clone(), id.clone());
            let then: crate::prod::Cont = std::rc::Rc::new(move |wb, _, cx| {
                wb.set_tab_connection(t2.clone(), id2.clone(), cx)
            });
            self.confirm_end_tx_later(vec![tab], then, cx);
            return;
        }
        let mode = self.default_tx_mode(id.as_deref(), tab.read(cx).tx.mode, cx);
        // A running query on the old connection is dropped (its handle cancels it).
        tab.update(cx, |t, cx| {
            t.abandon(cx);
            t.reset_tx(mode);
            t.conn_id = id;
            cx.notify();
        });
        self.point_completion(&tab, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(crate) fn any_busy(&self, cx: &App) -> bool {
        self.spaces
            .iter()
            .flat_map(|s| s.tabs.iter())
            .any(|t| t.read(cx).busy(cx))
    }

    pub(crate) fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.save_now(cx);
        // Open Manual transactions: Commit / Rollback / Cancel first.
        let open = self.open_tx_tabs(None, cx);
        if !open.is_empty() {
            let then: crate::prod::Cont = std::rc::Rc::new(|_, window, _| window.remove_window());
            self.confirm_end_tx(open, then, window, cx);
            return false;
        }
        if !self.any_busy(cx) {
            return true;
        }
        // Cancel first and close when every Finished arrived, so each KILL
        // reaches the server before the process exits; give up after 3 s.
        self.close_requested = true;
        for t in self
            .spaces
            .iter()
            .flat_map(|s| s.tabs.iter())
            .cloned()
            .collect::<Vec<_>>()
        {
            t.update(cx, |t, cx| t.cancel(cx));
        }
        if !self.any_busy(cx) {
            return true;
        }
        cx.spawn_in(window, async move |this, cx| {
            for _ in 0..60 {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let busy = this
                    .update(cx, |this, cx| this.any_busy(cx))
                    .unwrap_or(false);
                if !busy {
                    break;
                }
            }
            let _ = cx.update(|window, _| window.remove_window());
        })
        .detach();
        false
    }
}

impl Locus {
    /// Autocomplete of `tab` follows its connection (and that connection's slot).
    pub(crate) fn point_completion(&mut self, tab: &Entity<QueryTab>, cx: &mut App) {
        let conn = tab.read(cx).conn_id.clone();
        let slot = conn.as_ref().map(|c| self.pool.slot(c));
        *tab.read(cx).completion.lock() = crate::query::completion::Target {
            conn_id: conn,
            vars: tab.read(cx).bind.vars.keys().cloned().collect(),
            slot,
        };
    }
}
