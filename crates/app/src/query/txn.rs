//! Manual transactions of a query tab: the state after each run, Commit /
//! Rollback, the idle warning, the header controls and the Commit /
//! Rollback / Cancel prompt that closing, switching or quitting goes through.
//! The state machine is `locus_core::txn`.
use crate::shell::*;
use gpui_kit::component::Disableable as _;
use locus_core::txn::{Effect, Ended, TxMode, TxState};
use std::time::Instant;

impl QueryTab {
    /// Updates the transaction after a run on the pinned session ended.
    pub(crate) fn tx_after_run(&mut self, outcome: &db_core::Outcome, cx: &mut Context<Self>) {
        if let db_core::Outcome::Failed(e) = outcome
            && e.kind == ErrorKind::Connect
            && !self.tx.open
        {
            self.txn.session = None;
        }
        let Some((engine, classes)) = self.txn.run.take() else {
            return;
        };
        let now = Instant::now();
        let k = self.out.messages.len().min(classes.len());
        let effect = match outcome {
            db_core::Outcome::Done { .. } => {
                for c in &classes {
                    self.tx.statement_done(engine, c, now);
                }
                Effect::None
            }
            db_core::Outcome::Failed(e) => {
                for c in &classes[..k] {
                    self.tx.statement_done(engine, c, now);
                }
                self.tx.statement_failed(engine, e, now)
            }
            db_core::Outcome::Cancelled => {
                for c in &classes[..k] {
                    self.tx.statement_done(engine, c, now);
                }
                if self.out.truncated.is_some() {
                    Effect::None
                } else {
                    self.tx.statement_cancelled(engine, classes.get(k), now)
                }
            }
        };
        self.txn.note = match (effect, self.tx.ended) {
            (Effect::Lost, _) => {
                self.txn.session = None;
                let msg = t!("txn.note_lost").into_owned();
                crate::shell::toast::show(msg.clone(), cx);
                Some(msg)
            }
            (Effect::RolledBack, _) => {
                let msg = t!("txn.note_server_rollback").into_owned();
                crate::shell::toast::show(msg.clone(), cx);
                Some(msg)
            }
            (Effect::Aborted, _) => Some(t!("txn.note_aborted").into_owned()),
            (Effect::StatementUndone, _) => Some(t!("txn.note_statement_undone").into_owned()),
            (_, Some(Ended::ImplicitCommit)) if !self.tx.open => {
                Some(t!("txn.note_ddl").into_owned())
            }
            (_, Some(Ended::Typed)) if !self.tx.open => Some(t!("txn.note_typed").into_owned()),
            _ if self.tx.open => self.txn.note.take().filter(|_| self.tx.aborted),
            _ => None,
        };
        // SQL Server keeps the transaction after a cancelled or failed
        // statement unless the server ended it (XACT_ABORT ON, severe
        // errors): ask it instead of assuming.
        if engine == locus_core::txn::Engine::SqlServer
            && self.tx.open
            && matches!(effect, Effect::StatementUndone)
            && let Some(session) = self.txn.session.clone()
        {
            cx.spawn(async move |this, cx| {
                if session.still_open().await == Some(false) {
                    let _ = this.update(cx, |t, cx| {
                        if matches!(t.tx.server_rolled_back(), Effect::RolledBack) {
                            let msg = t!("txn.note_server_rollback").into_owned();
                            crate::shell::toast::show(msg.clone(), cx);
                            t.txn.note = Some(msg);
                            cx.notify();
                        }
                    });
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// Watches an open transaction for the idle warning.
    pub(crate) fn tx_watch(&mut self, cx: &mut Context<Self>) {
        let limit = crate::settings::get(cx).tx_idle();
        if limit.is_zero() {
            self.txn.idle_task = None;
            return;
        }
        self.txn.idle_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(15))
                    .await;
                let go_on = this.update(cx, |t, cx| {
                    if !t.tx.open {
                        return false;
                    }
                    if t.tx.idle_warning_due(Instant::now(), limit) {
                        let mins = (limit.as_secs() / 60).to_string();
                        crate::shell::toast::show(
                            t!("txn.idle_warning", name = t.display_title(cx), n = mins)
                                .into_owned(),
                            cx,
                        );
                    }
                    true
                });
                if !matches!(go_on, Ok(true)) {
                    break;
                }
            }
        }));
    }

    /// Commit (`true`) or roll back the open transaction. A lost connection
    /// counts as rolled back by the server and is reported as such.
    pub(crate) fn end_tx(
        &mut self,
        commit: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let Some(session) = self.txn.session.clone() else {
            self.tx.rolled_back();
            return Task::ready(Ok(()));
        };
        if self.busy(cx) || self.ap.applying {
            return Task::ready(Err(t!("txn.busy").into_owned()));
        }
        cx.spawn(async move |this, cx| {
            let res = if commit {
                session.commit().await
            } else {
                session.rollback().await
            };
            this.update(cx, |t, cx| {
                let out = match &res {
                    Ok(()) => {
                        if commit {
                            t.tx.committed();
                        } else {
                            t.tx.rolled_back();
                        }
                        t.txn.note = Some(
                            if commit {
                                t!("txn.note_committed")
                            } else {
                                t!("txn.note_rolled_back")
                            }
                            .into_owned(),
                        );
                        Ok(())
                    }
                    Err(e) if e.kind == ErrorKind::Connect => {
                        t.tx.lost();
                        t.txn.session = None;
                        let msg = t!("txn.note_lost").into_owned();
                        t.txn.note = Some(msg.clone());
                        if commit { Err(msg) } else { Ok(()) }
                    }
                    Err(e) => Err(match e.code {
                        Some(c) => format!("[{c}] {}", e.message),
                        None => e.message.clone(),
                    }),
                };
                cx.notify();
                out
            })
            .map_err(|_| t!("error.driver_gone").into_owned())?
        })
    }

    /// Mode change while no transaction is open. Leaving Manual drops the pinned session.
    pub(crate) fn set_tx_mode(&mut self, mode: TxMode, cx: &mut Context<Self>) -> bool {
        if !self.tx.set_mode(mode) {
            return false;
        }
        if mode == TxMode::Auto {
            self.txn.session = None;
        }
        self.txn.note = None;
        cx.notify();
        true
    }

    /// The tab's connection changed: the old session goes (no transaction is open).
    pub(crate) fn reset_tx(&mut self, mode: TxMode) {
        self.txn.session = None;
        self.txn.run = None;
        self.txn.note = None;
        self.txn.idle_task = None;
        self.tx = TxState::default();
        self.tx.mode = mode;
    }
}

impl Locus {
    /// Mode a tab gets on `conn`: Manual for Production when the setting says so.
    pub(crate) fn default_tx_mode(&self, conn: Option<&str>, current: TxMode, cx: &App) -> TxMode {
        let prod = conn
            .and_then(|id| self.conns.iter().find(|c| c.id == id))
            .is_some_and(|c| c.env == locus_core::store::Env::Prod);
        if prod && crate::settings::get(cx).prod_manual_tx() {
            TxMode::Manual
        } else {
            current
        }
    }

    /// Asks Commit / Rollback / Cancel for the open transactions of `tabs`,
    /// ends them, then runs `then`. Cancel leaves everything as it is.
    pub(crate) fn confirm_end_tx(
        &mut self,
        tabs: Vec<Entity<QueryTab>>,
        then: crate::prod::Cont,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let names: Vec<String> = tabs
            .iter()
            .map(|t| {
                let t = t.read(cx);
                let conn = t
                    .conn_id
                    .as_ref()
                    .and_then(|id| self.conns.iter().find(|c| &c.id == id))
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                format!(
                    "{} ({conn}, {})",
                    t.display_title(cx),
                    t!("txn.statements", n = t.tx.statements.to_string())
                )
            })
            .collect();
        let me = cx.entity().downgrade();
        window.open_pop_dialog(cx, move |dialog, _, _| {
            let (m1, m2) = (me.clone(), me.clone());
            let (t1, t2) = (tabs.clone(), tabs.clone());
            let (c1, c2) = (then.clone(), then.clone());
            dialog
                .title(t!("txn.prompt_title").to_string())
                .w(px(scale::W_DLG_S))
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .whitespace_normal()
                                .child(t!("txn.prompt_body").to_string()),
                        )
                        .children(names.iter().map(|n| {
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(n.clone())
                        }))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().flex_1())
                                .child(
                                    Button::new("tx-cancel")
                                        .label(t!("txn.cancel").to_string())
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                )
                                .child(
                                    Button::new("tx-rollback")
                                        .danger()
                                        .label(t!("txn.rollback").to_string())
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let _ = m1.update(cx, |this, cx| {
                                                this.end_tabs(
                                                    t1.clone(),
                                                    false,
                                                    c1.clone(),
                                                    window,
                                                    cx,
                                                )
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("tx-commit")
                                        .primary()
                                        .label(t!("txn.commit").to_string())
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let _ = m2.update(cx, |this, cx| {
                                                this.end_tabs(
                                                    t2.clone(),
                                                    true,
                                                    c2.clone(),
                                                    window,
                                                    cx,
                                                )
                                            });
                                        }),
                                ),
                        ),
                )
        });
    }

    /// [`confirm_end_tx`] from a path that has no `Window` (connection
    /// pickers, sidebar menus): opens the prompt on the active window.
    pub(crate) fn confirm_end_tx_later(
        &mut self,
        tabs: Vec<Entity<QueryTab>>,
        then: crate::prod::Cont,
        cx: &mut Context<Self>,
    ) {
        let me = cx.entity().downgrade();
        cx.defer(move |cx| {
            let Some(w) = cx.active_window() else { return };
            let _ = w.update(cx, |_, window, cx| {
                let _ = me.update(cx, |this, cx| this.confirm_end_tx(tabs, then, window, cx));
            });
        });
    }

    /// Ends every tab's transaction; `then` runs only when all ended.
    pub(crate) fn end_tabs(
        &mut self,
        tabs: Vec<Entity<QueryTab>>,
        commit: bool,
        then: crate::prod::Cont,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let jobs: Vec<_> = tabs
            .iter()
            .map(|t| t.update(cx, |t, cx| t.end_tx(commit, cx)))
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let mut errors = Vec::new();
            for j in jobs {
                if let Err(e) = j.await {
                    errors.push(e);
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                if errors.is_empty() {
                    then(this, window, cx);
                } else {
                    crate::shell::toast::show(
                        t!("txn.end_failed", msg = errors.join("; ")).into_owned(),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Commit / Rollback of the active tab (header buttons, shortcuts, palette).
    pub(crate) fn tx_end_active(
        &mut self,
        commit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.active_tab(cx);
        if !tab.read(cx).tx.open {
            return;
        }
        self.end_tabs(
            vec![tab],
            commit,
            std::rc::Rc::new(|_, _, _| {}),
            window,
            cx,
        );
    }

    /// Auto-commit ⇄ Manual for the active tab; leaving Manual with an open
    /// transaction asks first.
    pub(crate) fn tx_toggle_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab(cx);
        let next = if tab.read(cx).tx.manual() {
            TxMode::Auto
        } else {
            TxMode::Manual
        };
        if tab.read(cx).tx.needs_prompt() {
            let t2 = tab.clone();
            let then: crate::prod::Cont = std::rc::Rc::new(move |_, _, cx| {
                t2.update(cx, |t, cx| t.set_tx_mode(next, cx));
            });
            self.confirm_end_tx(vec![tab], then, window, cx);
        } else {
            tab.update(cx, |t, cx| t.set_tx_mode(next, cx));
        }
        cx.notify();
    }

    /// Tabs (any Space) with an open Manual transaction, optionally on one connection.
    pub(crate) fn open_tx_tabs(&self, conn: Option<&str>, cx: &App) -> Vec<Entity<QueryTab>> {
        self.spaces
            .iter()
            .flat_map(|s| s.tabs.iter())
            .filter(|t| {
                let t = t.read(cx);
                t.tx.needs_prompt() && conn.is_none_or(|c| t.conn_id.as_deref() == Some(c))
            })
            .cloned()
            .collect()
    }

    /// Mode switch and, in Manual mode, the transaction pill with Commit / Rollback.
    pub(crate) fn render_tx_controls(
        &self,
        tab: &Entity<QueryTab>,
        conn: &SavedConnection,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let tr = tab.read(cx);
        let manual = tr.tx.manual();
        let mode = Button::new("tx-mode")
            .ghost()
            .small()
            .label(
                if manual {
                    t!("txn.mode_manual")
                } else {
                    t!("txn.mode_auto")
                }
                .into_owned(),
            )
            .tooltip(t!("txn.mode_tip").to_string())
            .on_click(cx.listener(|this, _, window, cx| this.tx_toggle_mode(window, cx)));
        let mut row = h_flex()
            .min_w_0()
            .flex_shrink(2.)
            .overflow_hidden()
            .gap(px(scale::S3))
            .items_center()
            .child(mode);
        if manual && tr.tx.open {
            let prod = conn.env == locus_core::store::Env::Prod;
            let (bg, fg) = if prod {
                (t.red.opacity(0.14), t.red_text)
            } else {
                (t.yel.opacity(0.16), t.yel_text)
            };
            let label = if tr.tx.aborted {
                t!(
                    "txn.pill_aborted",
                    n = tr.tx.statements.to_string(),
                    conn = conn.name.clone()
                )
            } else {
                t!(
                    "txn.pill_open",
                    n = tr.tx.statements.to_string(),
                    conn = conn.name.clone()
                )
            };
            row = row
                .child(
                    h_flex()
                        .id("tx-pill")
                        .min_w_0()
                        .flex_shrink(2.)
                        .overflow_hidden()
                        .h(px(22.))
                        .px(px(9.))
                        .gap(px(6.))
                        .items_center()
                        .rounded(px(11.))
                        .bg(bg)
                        .text_color(fg)
                        .text_size(px(11.5))
                        .font_weight(FontWeight::MEDIUM)
                        .whitespace_nowrap()
                        .when_some(tr.txn.note.clone(), |d, n| d.tooltip(tip(n)))
                        .child(div().size(px(6.)).rounded_full().bg(fg))
                        .child(div().min_w_0().truncate().child(label.into_owned())),
                )
                .child(
                    Button::new("tx-commit-btn")
                        .xsmall()
                        .primary()
                        .disabled(tr.tx.aborted)
                        .label(t!("txn.commit").to_string())
                        .tooltip(t!("txn.commit_tip").to_string())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.tx_end_active(true, window, cx)),
                        ),
                )
                .child(
                    Button::new("tx-rollback-btn")
                        .xsmall()
                        .danger()
                        .label(t!("txn.rollback").to_string())
                        .tooltip(t!("txn.rollback_tip").to_string())
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.tx_end_active(false, window, cx)
                            }),
                        ),
                );
        } else if let Some(note) = tr.txn.note.clone().filter(|_| manual) {
            row = row.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(scale::TX_XS))
                    .text_color(t.tx3)
                    .child(note),
            );
        }
        row.into_any_element()
    }
}
