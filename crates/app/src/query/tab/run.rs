use super::*;

impl QueryTab {
    /// SQL for `mode` and the statement texts (for the Messages tab).
    pub fn sql_for(&self, mode: RunMode, cx: &App) -> (String, Vec<String>) {
        let s = self.editor.read(cx).state.read(cx);
        let text = s.value().to_string();
        let pick = match mode {
            RunMode::Script => text.clone(),
            RunMode::Current => {
                let sel = s.selected_value().to_string();
                if !sel.trim().is_empty() {
                    sel
                } else {
                    match statement_at(&text, s.cursor()) {
                        Some(r) => text[r].to_string(),
                        None => text.clone(),
                    }
                }
            }
        };
        let stmts = split_statements(&pick)
            .into_iter()
            .map(|r| pick[r].to_string())
            .collect();
        (pick, stmts)
    }

    // ---- query flow -------------------------------------------------------

    /// Runs `sql` on `conn` through `slot` (connecting first when the slot is
    /// empty). `password` is read by the caller from Credential Manager.
    pub fn run_sql(
        &mut self,
        sql: String,
        stmts: Vec<String>,
        conn: SavedConnection,
        password: Result<String, DbError>,
        slot: Slot,
        ro: Arc<std::sync::atomic::AtomicBool>,
        rt: tokio::runtime::Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bound = std::mem::take(&mut self.bind.bound);
        self.bind.run_written = self.bind.hist_written.take();
        if self.busy(cx) || sql.trim().is_empty() || self.ap.applying {
            return;
        }
        self.begin_run(&sql, stmts, &conn, &slot, window, cx);
        let password = match password {
            Ok(p) => p,
            Err(e) => {
                self.fail_before_run(e, cx);
                return;
            }
        };
        self.status.update(cx, |s, cx| {
            s.session_id = None;
            s.set_phase(Phase::Connecting, cx)
        });
        let limit = self.limit;
        let tx_classes = locus_core::txn::classify(&sql);
        let tx_manual = self.tx.manual();
        let tx_begin = self.tx.needs_begin(&tx_classes);
        let tx_sess = self.txn.session.clone();
        let tx_conn = conn.id.clone();
        self.txn.run = None;
        let parts = if has_delimiter(&sql) && !self.trace.stmts.is_empty() && bound.is_empty() {
            self.trace.stmts.clone()
        } else {
            vec![sql]
        };
        let task = cx.spawn_in(window, async move |this, cx| {
            // Connect (or reuse the shared driver) while holding the slot, so
            // two tabs starting together open one connection.
            let driver = {
                let mut g = slot.lock().await;
                match g.as_ref() {
                    Some(d) => d.clone(),
                    None => match locus_core::driver::AnyDriver::connect(&rt, &conn, password, ro).await {
                        Ok(d) => {
                            let d = Arc::new(d);
                            *g = Some(d.clone());
                            d
                        }
                        Err(e) => {
                            drop(g);
                            let _ = this.update_in(cx, |this, window, cx| this.finish(Outcome::Failed(e), window, cx));
                            return;
                        }
                    },
                }
            };
            let _ = limit;
            // Manual mode: every statement runs on the tab's pinned session.
            let runner = if tx_manual {
                match manual_runner(&this, cx, &driver, tx_sess, &tx_conn, tx_begin, tx_classes).await {
                    Some(runner) => runner,
                    None => return,
                }
            } else {
                driver.clone()
            };
            // The handle lives in this task: dropping the task (tab closed,
            // window gone) drops the handle, which cancels the query.
            let n = parts.len();
            for (pi, part) in parts.into_iter().enumerate() {
                let mut handle = if bound.is_empty() { runner.query(part, QueryOptions::default()) } else { runner.execute(part, bound.clone(), QueryOptions::default()) };
                let canceller = handle.canceller();
                let d2 = driver.clone();
                if this.update(cx, |this, cx| this.query_started(d2, canceller, cx)).is_err() {
                    return;
                }
                let mut finished = false;
                let more = pi + 1 < n;
                while let Some(ev) = handle.next().await {
                    let last = matches!(ev, QueryEvent::Finished(_));
                    let lost = matches!(&ev, QueryEvent::Finished(Outcome::Failed(e)) if e.kind == ErrorKind::Connect);
                    if lost {
                        // The connection is gone (server restart): the next Run reconnects.
                        pool::invalidate(&slot, &driver).await;
                    }
                    // A DELIMITER script runs statement by statement: only the
                    // last statement's Done (or any failure) ends the run.
                    let step_done = more && matches!(ev, QueryEvent::Finished(Outcome::Done { .. }));
                    if step_done {
                        let stop = this
                            .update(cx, |this, cx| {
                                this.close_open(MsgKind::Rows);
                                this.out.truncated.is_some() || this.status.read(cx).phase == Phase::Cancelling
                            })
                            .unwrap_or(true);
                        if stop {
                            let _ = this.update_in(cx, |this, window, cx| this.finish(Outcome::Cancelled, window, cx));
                            return;
                        }
                        finished = true;
                        break;
                    }
                    if this.update_in(cx, |this, window, cx| this.on_event(ev, window, cx)).is_err() {
                        return;
                    }
                    if last {
                        return;
                    }
                    // Paging: at the end of a page, stop reading. Back-pressure
                    // holds the server cursor until Next, Load all, cancel or
                    // the idle timeout.
                    let wait = this.update(cx, |t, cx| t.maybe_pause(cx)).ok().flatten();
                    if let Some(mut rx) = wait {
                        match rx.recv().await {
                            Some(true) => {}
                            _ => {
                                let _ = this.update_in(cx, |t, window, cx| t.expire(window, cx));
                                return;
                            }
                        }
                    }
                }
                if !finished {
                    pool::invalidate(&slot, &driver).await;
                    let e = DbError::new(ErrorKind::Internal, t!("error.driver_gone").to_string());
                    let _ = this.update_in(cx, |this, window, cx| this.finish(Outcome::Failed(e), window, cx));
                    return;
                }
            }
        });
        self.job = Some(Job {
            _task: task,
            canceller: None,
        });
        cx.notify();
    }

    /// Reset the tab for a new run: results, messages, counters, paging and the row cap.
    fn begin_run(
        &mut self,
        sql: &str,
        stmts: Vec<String>,
        conn: &SavedConnection,
        slot: &Slot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.out.results.clear();
        self.ui_on_run(window, cx);
        self.out.messages.clear();
        self.out.active = 0;
        self.trace.stmts = stmts;
        self.trace.open_stmt = None;
        self.out.rows_total = 0;
        self.out.truncated = None;
        self.last_sql = sql.to_string();
        self.cv_begin(sql, conn);
        self.trace.count_ctx = Some((
            match conn.kind {
                locus_core::store::DbKind::Postgres => db_core::sql::SqlDialect::Postgres,
                locus_core::store::DbKind::Oracle => db_core::sql::SqlDialect::Oracle,
                locus_core::store::DbKind::Sqlite | locus_core::store::DbKind::SqlServer => {
                    db_core::sql::SqlDialect::Sqlite
                }
                _ => db_core::sql::SqlDialect::Mysql,
            },
            conn.env == locus_core::store::Env::Prod,
        ));
        self.ap.error = None;
        self.has_run = true;
        self.pg.page = 0;
        self.pg.fetch_target = self.limit.unwrap_or(u64::MAX);
        self.pg.paused = false;
        self.pg.expired = false;
        self.pg.show_all = false;
        self.pg.total_count = None;
        self.pg.count_task = None;
        self.pg.pager_tx = None;
        self.pg.idle_task = None;
        self.pg.rows_cap = crate::settings::get(cx).rows_cap();
        self.pg.row_cap = locus_core::rowcap::RowCap::new(self.pg.rows_cap);
        self.sync_status(cx);
        self.run_at = locus_core::history::now_secs();
        self.slot = Some(slot.clone());
    }

    /// The password could not be read: the run ends before connecting.
    fn fail_before_run(&mut self, e: DbError, cx: &mut Context<Self>) {
        self.status.update(cx, |s, cx| {
            s.set_phase(Phase::Connecting, cx);
            s.error = Some(e.clone());
            s.set_phase(Phase::Failed, cx);
        });
        self.out.messages.push(Message {
            index: 0,
            sql: None,
            kind: MsgKind::Error(e, 0),
            elapsed: Duration::ZERO,
        });
        cx.notify();
        cx.notify();
    }

    pub(super) fn query_started(
        &mut self,
        driver: Arc<locus_core::driver::AnyDriver>,
        canceller: Canceller,
        cx: &mut Context<Self>,
    ) {
        self.driver = Some(driver);
        let cancelling = self.status.read(cx).phase == Phase::Cancelling;
        if cancelling {
            canceller.cancel();
        } else {
            self.status
                .update(cx, |s, cx| s.set_phase(Phase::Running, cx));
        }
        if let Some(job) = &mut self.job {
            job.canceller = Some(canceller);
        }
        self.trace.stmt_mark = Instant::now();
        self.ui.timing.exec_start.get_or_insert_with(Instant::now);
        cx.notify();
    }

    pub(super) fn stmt_text(&self, i: usize) -> Option<String> {
        self.trace
            .stmts
            .get(i)
            .cloned()
            .or_else(|| (i == 0 && self.trace.stmts.is_empty()).then(|| self.last_sql.clone()))
    }

    pub(super) fn next_index(&self) -> usize {
        self.out.messages.len()
    }

    /// Closes the statement whose rows were streaming.
    pub(super) fn close_open(&mut self, kind: impl FnOnce(u64) -> MsgKind) {
        if let Some(ri) = self.trace.open_stmt.take() {
            let rows = self.out.results.get(ri).map(|r| r.rows).unwrap_or(0);
            let i = self.next_index();
            let elapsed = self.trace.stmt_mark.elapsed();
            self.out.messages.push(Message {
                index: i,
                sql: self.stmt_text(i),
                kind: kind(rows),
                elapsed,
            });
            self.trace.stmt_mark = Instant::now();
        }
    }

    pub(super) fn on_event(&mut self, ev: QueryEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            QueryEvent::Started { session_id } => {
                self.status.update(cx, |s, cx| {
                    s.session_id = Some(session_id);
                    cx.notify();
                });
            }
            QueryEvent::Columns(cols) => self.on_columns(cols, window, cx),
            QueryEvent::Batch(b) => self.on_batch(b, cx),
            QueryEvent::Affected { rows } => {
                self.close_open(MsgKind::Rows);
                let i = self.next_index();
                self.out.messages.push(Message {
                    index: i,
                    sql: self.stmt_text(i),
                    kind: MsgKind::Affected(rows),
                    elapsed: self.trace.stmt_mark.elapsed(),
                });
                self.trace.stmt_mark = Instant::now();
                cx.notify();
            }
            QueryEvent::Finished(outcome) => self.finish(outcome, window, cx),
        }
    }

    /// A statement returned columns: a new result grid with its right-click, width and double-click handling.
    fn on_columns(
        &mut self,
        cols: Vec<db_core::ColumnMeta>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_open(MsgKind::Rows);
        let sql = self.stmt_text(self.next_index()).unwrap_or_default();
        let name = db_core::tabname::result_name(&sql, &cols);
        let tab_weak = cx.entity().downgrade();
        let table = cx.new(|cx| {
            let mut g = ResultGrid::new();
            g.reset(cols);
            self.cv_grid(&mut g);
            g.on_up = Some(std::rc::Rc::new(move |m, cx| {
                let _ = tab_weak.update(cx, |t, cx| t.grid_up(m, cx));
            }));
            TableState::new(g, window, cx)
                .cell_selectable(true)
                .row_selectable(true)
        });
        let sub = cx.subscribe_in(
            &table,
            window,
            |this, table, ev: &TableEvent, window, cx| match ev {
                TableEvent::RightClickedCell(r, c) => {
                    let (r, c) = (*r, *c);
                    let weak = table.downgrade();
                    table.update(cx, |t, _| t.delegate_mut().right_col = c);
                    let editable = table.read(cx).delegate().editable();
                    let nav = table.read(cx).delegate().nav_items(r, c);
                    let menu = PopupMenu::build(window, cx, move |m, _, _| {
                        crate::results::grid::grid_menu(weak, r, c, editable, nav, m)
                    });
                    let pos = window.mouse_position();
                    menu.read(cx).focus_handle(cx).focus(window, cx);
                    this.show_grid_menu(menu, pos, cx);
                }
                TableEvent::ColumnWidthsChanged(w) => {
                    table.update(cx, |t, _| t.delegate_mut().set_widths(w))
                }
                TableEvent::DoubleClickedCell(r, c) => {
                    let (r, c) = (*r, *c);
                    // A panic here (unexpected value shape) shows an error instead
                    // of closing the window; the panic hook still logs it to crash.log.
                    let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        table.update(cx, |t, cx| {
                            if t.delegate().editable() {
                                t.delegate_mut().start_edit(r, c, window, cx);
                            } else if t.delegate().wants_viewer(r, c) {
                                open_viewer(t.delegate(), r, c, window, cx);
                            }
                        })
                    }))
                    .is_ok();
                    if !ok {
                        crate::shell::toast::show(t!("grid.edit_failed").to_string(), cx);
                    }
                }
                _ => {}
            },
        );
        if self.ui.timing.first_row.is_none() {
            self.ui.timing.first_row = self.ui.timing.exec_start.map(|t| t.elapsed());
        }
        self.out.results.push(ResultTab {
            table,
            note: None,
            rows: 0,
            name,
            tip: db_core::tabname::elide(&sql),
            _sub: sub,
        });
        self.trace.open_stmt = Some(self.out.results.len() - 1);
        if self.out.results.len() == 1 {
            self.out.active = 0;
        }
        cx.notify();
    }

    /// A batch of rows: kept up to the memory cap, appended to the open result.
    fn on_batch(&mut self, b: db_core::Batch, cx: &mut Context<Self>) {
        if self.out.truncated.is_some() {
            return;
        }
        let Some(ri) = self.trace.open_stmt else {
            return;
        };
        let mut b = b;
        let mut hit = false;
        // The memory cap (settings) still stops the query; the row
        // limit only pages (see `maybe_pause`). A result of exactly
        // the cap is complete: only a further row marks it truncated.
        let total = b.rows() as u64;
        let admit = self.pg.row_cap.admit(total);
        if admit.keep < total {
            hit = admit.truncated_now;
            if admit.keep == 0 {
                if hit {
                    self.out.truncated = Some(self.pg.rows_cap);
                    if let Some(c) = self.job.as_ref().and_then(|j| j.canceller.clone()) {
                        c.cancel();
                    }
                }
                return;
            }
            b = truncate_batch(&b, admit.keep as usize);
        }
        let n = b.rows() as u64;
        self.out.rows_total += n;
        let window = self.page_window();
        let r = &mut self.out.results[ri];
        r.rows += n;
        // notify() is coalesced by GPUI: however many batches arrive,
        // the grid and status bar repaint at most once per frame.
        r.table.update(cx, |t, cx| {
            if t.delegate_mut().push(b) {
                t.refresh(cx);
            }
            t.delegate_mut().page = window;
            cx.notify();
        });
        self.status.update(cx, |s, cx| {
            s.rows += n;
            cx.notify();
        });
        self.kick_filter(cx);
        if hit {
            // At the memory cap: stop the query on the server and say so.
            self.out.truncated = Some(self.pg.rows_cap);
            if let Some(c) = self.job.as_ref().and_then(|j| j.canceller.clone()) {
                c.cancel();
            }
        }
    }

    pub(super) fn finish(
        &mut self,
        outcome: Outcome,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tx_after_run(&outcome, cx);
        if let Some(conn_id) = self.conn_id.clone() {
            let error = match &outcome {
                Outcome::Failed(e) => Some(match e.code {
                    Some(c) => format!("[{c}] {}", e.message),
                    None => e.message.clone(),
                }),
                Outcome::Cancelled if self.out.truncated.is_none() => {
                    Some(t!("state.cancelled").into_owned())
                }
                _ => None,
            };
            let ms = self
                .status
                .read(cx)
                .started
                .map(|s| s.elapsed().as_millis() as u64)
                .unwrap_or(0);
            let rows = error.is_none().then_some(self.out.rows_total);
            let (sql, params) = self
                .bind
                .run_written
                .take()
                .unwrap_or_else(|| (self.last_sql.clone(), Vec::new()));
            cx.emit(TabEvent::Finished(locus_core::history::Entry {
                conn_id,
                sql,
                at: self.run_at,
                ms,
                rows,
                error,
                params,
                mcp_client: None,
            }));
        }
        let truncated = self.out.truncated;
        let mut phase = match &outcome {
            Outcome::Done { .. } => Phase::Done,
            Outcome::Cancelled => Phase::Cancelled,
            Outcome::Failed(_) => Phase::Failed,
        };
        if let (Some(n), Phase::Cancelled | Phase::Done) = (truncated, phase) {
            phase = Phase::Done;
            let open = self.trace.open_stmt;
            self.close_open(|_| MsgKind::Truncated(n));
            if let Some(ri) = open.or(self.out.results.len().checked_sub(1)) {
                self.out.results[ri].note = Some(Note::Truncated(n));
            }
        }
        match &outcome {
            Outcome::Done { .. } => self.close_open(MsgKind::Rows),
            Outcome::Cancelled => {
                if let Some(ri) = self.trace.open_stmt {
                    self.out.results[ri].note = Some(Note::Cancelled);
                }
                if truncated.is_none() {
                    let had_open = self.trace.open_stmt.is_some();
                    self.close_open(MsgKind::Cancelled);
                    if !had_open {
                        let i = self.next_index();
                        self.out.messages.push(Message {
                            index: i,
                            sql: self.stmt_text(i),
                            kind: MsgKind::Cancelled(0),
                            elapsed: self.trace.stmt_mark.elapsed(),
                        });
                    }
                }
            }
            Outcome::Failed(e) => {
                if let Some(ri) = self.trace.open_stmt {
                    self.out.results[ri].note = Some(Note::Failed);
                }
                let had_open = self.trace.open_stmt.is_some();
                let e2 = e.clone();
                self.close_open(|rows| MsgKind::Error(e2, rows));
                if !had_open {
                    let i = self.next_index();
                    self.out.messages.push(Message {
                        index: i,
                        sql: self.stmt_text(i),
                        kind: MsgKind::Error(e.clone(), 0),
                        elapsed: self.trace.stmt_mark.elapsed(),
                    });
                }
            }
        }
        self.job = None;
        self.sync_status(cx);
        self.ui.timing.end = self.ui.timing.exec_start.map(|t| t.elapsed());
        self.status.update(cx, |s, cx| {
            if let Outcome::Failed(e) = &outcome {
                s.error = Some(e.clone());
            }
            s.set_phase(phase, cx);
        });
        for r in &self.out.results {
            r.table.update(cx, |t, cx| {
                t.delegate_mut().sortable = true;
                t.refresh(cx);
                cx.notify();
            });
        }
        // An error with nothing to show goes straight to Messages.
        if self.out.results.is_empty()
            || (matches!(outcome, Outcome::Failed(_))
                && self.out.results.iter().all(|r| r.rows == 0))
        {
            self.out.active = self.out.results.len();
        }
        self.check_editable(cx);
        cx.notify();
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        let phase = self.status.read(cx).phase;
        match phase {
            Phase::Paused => {
                // The stream loop gives the query up and keeps the rows.
                if let Some(tx) = self.pg.pager_tx.take() {
                    let _ = tx.send(false);
                }
            }
            Phase::Connecting => {
                if let Some(c) = self.job.as_ref().and_then(|j| j.canceller.clone()) {
                    c.cancel();
                    self.status.update(cx, |s, cx| {
                        s.phase = Phase::Cancelling;
                        cx.notify();
                    });
                } else {
                    // Nothing runs on the server yet: drop the connect attempt.
                    self.job = None;
                    self.status
                        .update(cx, |s, cx| s.set_phase(Phase::Cancelled, cx));
                    let i = self.next_index();
                    self.out.messages.push(Message {
                        index: i,
                        sql: None,
                        kind: MsgKind::Cancelled(0),
                        elapsed: Duration::ZERO,
                    });
                    self.out.active = self.out.results.len();
                }
            }
            Phase::Running => {
                if let Some(c) = self.job.as_ref().and_then(|j| j.canceller.clone()) {
                    c.cancel();
                }
                // Stays `cancelling` until the driver reports Finished.
                self.status.update(cx, |s, cx| {
                    s.phase = Phase::Cancelling;
                    cx.notify();
                });
            }
            _ => {}
        }
        cx.notify();
    }

    /// Drops the job without waiting (its handle cancels on drop): the
    /// connection was retired or deleted.
    pub fn abandon(&mut self, cx: &mut Context<Self>) {
        if self.job.take().is_some() {
            self.status
                .update(cx, |s, cx| s.set_phase(Phase::Cancelled, cx));
        }
        self.driver = None;
        self.slot = None;
    }
}

/// Manual mode: opens (or reuses) the tab's pinned session and begins the transaction when the
/// statements need it; every statement then runs on that session. `None` when the run already ended.
async fn manual_runner(
    this: &WeakEntity<QueryTab>,
    cx: &mut AsyncWindowContext,
    driver: &Arc<locus_core::driver::AnyDriver>,
    tx_sess: Option<locus_core::txn::TxSession>,
    tx_conn: &str,
    tx_begin: bool,
    tx_classes: Vec<db_core::classify::Classified>,
) -> Option<Arc<dyn db_core::Driver>> {
    let session = match tx_sess {
        Some(s) => s,
        None => match locus_core::txn::TxSession::open(driver, tx_conn).await {
            Ok(s) => s,
            Err(e) => {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.finish(Outcome::Failed(e), window, cx)
                });
                return None;
            }
        },
    };
    let begun = if tx_begin {
        session.begin().await
    } else {
        Ok(())
    };
    let keep = session.clone();
    let ok = this.update(cx, |t, cx| {
        match &begun {
            Ok(()) => {
                t.txn.session = Some(keep);
                if tx_begin {
                    t.tx.began(Instant::now());
                    t.txn.note = None;
                    t.tx_watch(cx);
                }
                t.txn.run = Some((session.engine, tx_classes));
            }
            // A session that cannot begin is not kept.
            Err(_) => t.txn.session = None,
        }
    });
    if ok.is_err() {
        return None;
    }
    if let Err(e) = begun {
        let _ = this.update_in(cx, |this, window, cx| {
            this.finish(Outcome::Failed(e), window, cx)
        });
        return None;
    }
    Some(session.driver.clone())
}
