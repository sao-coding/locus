use super::*;

impl QueryTab {
    // ---- editability -----------------------------------------------------

    pub(super) fn check_editable(&mut self, cx: &mut Context<Self>) {
        let Some(driver) = self.driver.clone() else {
            return;
        };
        let single_result = self.out.results.len() == 1;
        let default_db = self
            .conn_id
            .as_ref()
            .and_then(|id| self.space_conns.iter().find(|c| &c.id == id))
            .and_then(|c| c.database.clone());
        for r in &self.out.results {
            self.check_result_editable(r.table.clone(), &driver, &default_db, single_result, cx);
        }
    }

    /// Decides whether one result grid is editable (single table, keys known) and wires its
    /// foreign-key navigation; the column list from the catalog arrives asynchronously.
    fn check_result_editable(
        &self,
        table: Entity<TableState<ResultGrid>>,
        driver: &Arc<locus_core::driver::AnyDriver>,
        default_db: &Option<String>,
        single_result: bool,
        cx: &mut Context<Self>,
    ) {
        let cols = table.read(cx).delegate().rs.columns().to_vec();
        if cols.is_empty() {
            return;
        }
        let reason = |key: &str| Some(t!(key).into_owned());
        if driver.is_oracle() {
            table.update(cx, |t, cx| {
                t.delegate_mut().read_only_reason = reason("edit.ro_oracle");
                cx.notify();
            });
            return;
        }
        let mut origins: Vec<_> = cols.iter().map(|c| c.origin.clone()).collect();
        // Drivers without column origins (sidecar): a plain single-table
        // SELECT names its table; columns are then taken by name.
        if single_result
            && origins.iter().all(Option::is_none)
            && let Some(tr) = db_core::sql::single_table(&self.last_sql)
            && let Some(schema) = tr.schema.clone().or_else(|| default_db.clone())
        {
            origins = cols
                .iter()
                .map(|c| {
                    Some(db_core::ColumnOrigin {
                        schema: schema.clone(),
                        table: tr.table.clone(),
                        column: c.name.clone(),
                    })
                })
                .collect();
        }
        // An empty schema is a column of a derived table: not editable.
        let pre = if origins
            .iter()
            .any(|o| o.as_ref().is_none_or(|o| o.schema.is_empty()))
        {
            reason("edit.ro_expression")
        } else {
            let first = origins[0].as_ref().unwrap();
            if origins.iter().any(|o| {
                let o = o.as_ref().unwrap();
                o.schema != first.schema || o.table != first.table
            }) {
                reason("edit.ro_joined")
            } else {
                None
            }
        };
        if let Some(why) = pre {
            table.update(cx, |t, cx| {
                t.delegate_mut().read_only_reason = Some(why);
                cx.notify();
            });
            return;
        }
        let o = origins[0].clone().unwrap();
        let col_names: Vec<String> = origins
            .iter()
            .map(|x| x.as_ref().unwrap().column.clone())
            .collect();
        // Foreign-key navigation: same single-table check, keys of that schema.
        if let Some(fk_fut) = driver.catalog().map(|c| c.foreign_keys(&o.schema)) {
            let ctx = crate::results::grid::FkCtx {
                schema: o.schema.clone(),
                table: o.table.clone(),
                cols: col_names.clone(),
                fks: Vec::new(),
                out_cols: vec![false; col_names.len()],
                driver: driver.clone(),
                dialect: crate::results::ui::dialect_of(Some(driver)),
            };
            table.update(cx, |t, _| t.delegate_mut().fk = Some(ctx));
            let table = table.clone();
            cx.spawn(async move |_, cx| {
                if let Ok(fks) = fk_fut.await {
                    table.update(cx, |t, cx| {
                        t.delegate_mut().set_fks(fks);
                        cx.notify();
                    });
                }
            })
            .detach();
        }
        let Some(catalog_fut) = driver.catalog().map(|c| c.columns(&o.schema, &o.table)) else {
            table.update(cx, |t, cx| {
                t.delegate_mut().read_only_reason = Some(t!("edit.ro_no_catalog").into_owned());
                cx.notify();
            });
            return;
        };
        let d = driver.clone();
        cx.spawn(async move |_, cx| {
            let res = catalog_fut.await;
            table.update(cx, |t, cx| {
                let g = t.delegate_mut();
                match res {
                    Err(e) => {
                        g.read_only_reason =
                            Some(t!("edit.ro_catalog_error", msg = e.message).into_owned())
                    }
                    Ok(info) => set_edit_target(g, &info, &col_names, &d, &o),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A message from a result grid: open a foreign-key lookup in a new tab, or redraw the
    /// footer (selection numbers).
    pub(super) fn grid_up(&mut self, m: crate::results::grid::GridUp, cx: &mut Context<Self>) {
        use crate::results::grid::GridUp;
        match m {
            GridUp::Open(sql) => {
                if let Some(conn_id) = self.conn_id.clone() {
                    cx.emit(TabEvent::OpenTable { conn_id, sql });
                }
            }
            GridUp::Stats => cx.notify(),
        }
    }

    // ---- apply staged edits ---------------------------------------------

    /// The staged statements Apply would send (empty when nothing is staged).
    pub fn staged_statements(&self, cx: &App) -> Vec<Statement> {
        self.current_table()
            .map(|t| t.read(cx).delegate().statements())
            .unwrap_or_default()
    }

    /// Sends the staged edits. Only the shell calls this, after the
    /// Production gate (`TabEvent::ApplyRequest`).
    pub fn apply_edits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(table) = self.current_table().cloned() else {
            return;
        };
        let Some(driver) = self.driver.clone() else {
            return;
        };
        let stmts = table.read(cx).delegate().statements();
        if stmts.is_empty() || self.ap.applying || self.busy(cx) {
            return;
        }
        self.ap.applying = true;
        self.ap.error = None;
        let tx = self.tx.manual().then(|| {
            (
                self.txn.session.clone(),
                !self.tx.open,
                self.conn_id.clone().unwrap_or_default(),
            )
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            let res = match tx {
                // Manual mode: the edits join the tab's transaction; nothing commits.
                Some((session, begin, conn_id)) => {
                    let n = stmts.len() as u32;
                    let session = match session {
                        Some(s) => Ok(s),
                        None => locus_core::txn::TxSession::open(&driver, &conn_id)
                            .await
                            .map_err(|e| e.message),
                    };
                    match session {
                        Ok(s) => {
                            let res =
                                locus_core::apply::apply_in_tx(&*s.driver, begin, stmts).await;
                            let _ = this.update(cx, |t, cx| {
                                t.txn.session = Some(s);
                                if begin {
                                    t.tx.began(Instant::now());
                                    t.txn.note = None;
                                    t.tx_watch(cx);
                                }
                                if res.is_ok() {
                                    t.tx.applied(n, Instant::now());
                                }
                                cx.notify();
                            });
                            res
                        }
                        Err(e) => Err(e),
                    }
                }
                None => locus_core::apply::apply(driver, stmts).await,
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.ap.applying = false;
                this.ap.task = None;
                cx.emit(TabEvent::ApplyDone);
                match res {
                    Ok(()) => {
                        // Show what is stored now (generated keys, defaults, triggers).
                        table.update(cx, |t, _| t.delegate_mut().discard());
                        let _ = window;
                        cx.emit(TabEvent::Rerun);
                    }
                    Err(e) => this.ap.error = Some(e),
                }
                cx.notify();
            });
        });
        self.ap.task = Some(task);
        cx.notify();
    }

    pub fn preview_edits(&self, window: &mut Window, cx: &mut App) {
        let Some(table) = self.current_table() else {
            return;
        };
        let stmts = table.read(cx).delegate().statements();
        let text = preview_text(&stmts, self.tx.manual());
        let state = cx.new(|cx| {
            gpui_kit::component::input::TextareaState::new(window, cx)
                .rows(14)
                .default_value(text)
        });
        window.open_pop_dialog(cx, move |dialog, _, cx| {
            dialog
                .title(t!("edit.preview_title").to_string())
                .w(px(760.))
                .child(
                    v_flex()
                        .gap(px(sc::S4))
                        .child(
                            div()
                                .text_size(px(sc::TX_BASE))
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("edit.preview_note").to_string()),
                        )
                        .child(
                            gpui_kit::component::input::Textarea::new(&state)
                                .readonly(true)
                                .h(px(320.)),
                        ),
                )
        });
    }

    // ---- grid keys --------------------------------------------------------

    /// The grid in front, when it (or its cell editor) has focus.
    pub(super) fn focused_table(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<Entity<TableState<ResultGrid>>> {
        let table = self.current_table()?.clone();
        table
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
            .then_some(table)
    }

    /// A cell of the focused grid is being edited.
    pub fn grid_editing(&self, window: &Window, cx: &App) -> bool {
        self.focused_table(window, cx)
            .is_some_and(|t| t.read(cx).delegate().is_editing())
    }

    /// Esc in a cell edit: drop the edit, keep the grid focused.
    pub fn grid_cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(table) = self.focused_table(window, cx) else {
            return;
        };
        table.update(cx, |t, cx| t.delegate_mut().cancel_edit(cx));
        table.read(cx).focus_handle(cx).focus(window, cx);
    }

    /// The focused grid, when it is not editing a cell (the cell editor keeps its keys).
    pub(super) fn grid_for_keys(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<Entity<TableState<ResultGrid>>> {
        self.focused_table(window, cx)
            .filter(|t| !t.read(cx).delegate().is_editing())
    }

    /// Ctrl+Shift+C: the column name(s) of the selection.
    pub(super) fn grid_copy_columns(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(table) = self.grid_for_keys(window, cx) else {
            return false;
        };
        let sel = table.read(cx).selection();
        let g = table.read(cx).delegate();
        let cols = g.rs.columns();
        let names: Vec<String> = match (g.range_bounds(), sel) {
            (Some((_, cs)), _) => cs
                .filter_map(|c| cols.get(c).map(|x| x.name.clone()))
                .collect(),
            (None, TableSelection::Cell(_, c) | TableSelection::Column(c)) => cols
                .get(c)
                .map(|x| vec![x.name.clone()])
                .unwrap_or_default(),
            _ => cols.iter().map(|x| x.name.clone()).collect(),
        };
        cx.write_to_clipboard(ClipboardItem::new_string(names.join(", ")));
        true
    }

    /// Ctrl+C: the range, else the cell / row / column, as TSV.
    pub(super) fn grid_copy(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(table) = self.grid_for_keys(window, cx) else {
            return false;
        };
        if let Some(text) = table.read(cx).delegate().copy_range() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            return true;
        }
        let sel = table.read(cx).selection();
        let g = table.read(cx).delegate();
        let n = g.rs.columns().len();
        let text = match sel {
            TableSelection::Cell(r, c) => Some(g.copy_rows_tsv(&[r], c..c + 1)),
            TableSelection::Row(r) => Some(g.copy_rows_tsv(&[r], 0..n)),
            TableSelection::Column(c) => {
                Some(g.copy_rows_tsv(&(0..g.rows_count_all()).collect::<Vec<_>>(), c..c + 1))
            }
            TableSelection::None => None,
        };
        let Some(text) = text else {
            return false;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    /// Shift+arrow: grow the range from the selected cell.
    pub(super) fn grid_extend(
        &mut self,
        dr: isize,
        dc: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(table) = self.grid_for_keys(window, cx) else {
            return false;
        };
        let from = match table.read(cx).selection() {
            TableSelection::Cell(r, c) => (r, c),
            TableSelection::Row(r) => (r, 0),
            _ => (0, 0),
        };
        table.update(cx, |t, cx| {
            let end = t.delegate_mut().extend_range(from, dr, dc);
            t.delegate_mut().range_changed(cx);
            t.scroll_to_row(end.0, cx);
            cx.notify();
        });
        true
    }

    /// F2 / Enter on a cell: edit it, or open the viewer when read-only.
    pub(super) fn grid_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(table) = self.grid_for_keys(window, cx) else {
            return false;
        };
        let TableSelection::Cell(r, c) = table.read(cx).selection() else {
            return false;
        };
        table.update(cx, |t, cx| {
            if t.delegate().editable() {
                t.delegate_mut().start_edit(r, c, window, cx);
            } else {
                open_viewer(t.delegate(), r, c, window, cx);
            }
        });
        true
    }

    /// The grid's key handlers (context `Grid`, see `shell::keymap`). A key that
    /// does not apply goes on to the table's own bindings.
    pub(super) fn grid_actions(el: Div, cx: &mut Context<Self>) -> Div {
        use crate::shell::keymap::act;
        fn done(handled: bool, cx: &mut App) {
            if !handled {
                cx.propagate();
            }
        }
        el.key_context("Grid")
            .on_action(cx.listener(|this, _: &act::GridCopy, w, cx| {
                let h = this.grid_copy(w, cx);
                done(h, cx)
            }))
            .on_action(cx.listener(|this, _: &act::GridCopyColumns, w, cx| {
                let h = this.grid_copy_columns(w, cx);
                done(h, cx)
            }))
            .on_action(cx.listener(|this, _: &act::GridExtendUp, w, cx| {
                let h = this.grid_extend(-1, 0, w, cx);
                done(h, cx)
            }))
            .on_action(cx.listener(|this, _: &act::GridExtendDown, w, cx| {
                let h = this.grid_extend(1, 0, w, cx);
                done(h, cx)
            }))
            .on_action(cx.listener(|this, _: &act::GridExtendLeft, w, cx| {
                let h = this.grid_extend(0, -1, w, cx);
                done(h, cx)
            }))
            .on_action(cx.listener(|this, _: &act::GridExtendRight, w, cx| {
                let h = this.grid_extend(0, 1, w, cx);
                done(h, cx)
            }))
            .on_action(cx.listener(|this, _: &act::GridEdit, w, cx| {
                let h = this.grid_edit(w, cx);
                done(h, cx)
            }))
    }
}

/// Keys, quoting and the reason (if any) a result cannot be edited, from the table's columns.
fn set_edit_target(
    g: &mut ResultGrid,
    info: &[db_core::ColumnInfo],
    names: &[String],
    d: &Arc<locus_core::driver::AnyDriver>,
    o: &db_core::ColumnOrigin,
) {
    let mut pk: Vec<_> = info
        .iter()
        .filter(|c| c.primary_key)
        .map(|c| c.name.clone())
        .collect();
    // SQLite: a table without a primary key is keyed by its
    // rowid when the result carries it (`SELECT rowid, * ...`).
    let sqlite = d.is_sqlite();
    if pk.is_empty()
        && sqlite
        && !info.is_empty()
        && let Some(r) = names.iter().find(|n| {
            ["rowid", "_rowid_", "oid"]
                .iter()
                .any(|k| n.eq_ignore_ascii_case(k))
        })
    {
        pk.push(r.clone());
    }
    if pk.is_empty() && sqlite {
        g.read_only_reason =
            Some(t!("sqlite.ro_no_pk_rowid", table = o.table.clone()).into_owned());
    } else if pk.is_empty() {
        g.read_only_reason = Some(t!("edit.ro_no_pk", table = o.table.clone()).into_owned());
    } else if let Some(missing) = pk
        .iter()
        .find(|k| !names.iter().any(|n| n.eq_ignore_ascii_case(k)))
    {
        g.read_only_reason = Some(t!("edit.ro_pk_missing", column = missing.clone()).into_owned());
    } else {
        let key = pk
            .iter()
            .map(|k| {
                names
                    .iter()
                    .position(|n| n.eq_ignore_ascii_case(k))
                    .unwrap()
            })
            .collect();
        g.target = Some(EditTarget {
            quoted_table: format!(
                "{}.{}",
                d.quote_identifier(&o.schema),
                d.quote_identifier(&o.table)
            ),
            quoted_columns: names.iter().map(|n| d.quote_identifier(n)).collect(),
            key,
        });
        g.read_only_reason = None;
    }
}
