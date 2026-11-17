//! Run state and the status bar. A separate entity, so the 10 Hz elapsed-time
//! tick while a query runs re-renders only the status bar.

use db_core::DbError;
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use locus_core::fmt;
use locus_core::status::Phase;
use rust_i18n::t;
use std::time::{Duration, Instant};

pub struct RunStatus {
    pub phase: Phase,
    pub rows: u64,
    pub started: Option<Instant>,
    pub elapsed: Duration,
    /// When the last query's `Finished` arrived (bench mode measures cancel latency with it).
    pub finished_at: Option<Instant>,
    pub session_id: Option<u64>,
    pub error: Option<DbError>,
    pub plain_editor: bool,
    /// Inputs of `locus_core::runview`, kept in sync by `QueryTab::sync_status`.
    pub limit: Option<u64>,
    pub total: Option<u64>,
    pub released: bool,
    pub capped: bool,
    ticker: Option<Task<()>>,
}

impl RunStatus {
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            rows: 0,
            started: None,
            elapsed: Duration::ZERO,
            finished_at: None,
            session_id: None,
            error: None,
            plain_editor: false,
            limit: None,
            total: None,
            released: false,
            capped: false,
            ticker: None,
        }
    }

    /// The one mapping from run state to button / status / footer / actions.
    pub fn view(&self) -> locus_core::runview::RunView {
        locus_core::runview::view(&locus_core::runview::RunInput {
            phase: self.phase,
            rows: self.rows,
            limit: self.limit,
            total: self.total,
            released: self.released,
            capped: self.capped,
        })
    }

    pub fn set_phase(&mut self, phase: Phase, cx: &mut Context<Self>) {
        let was_busy = self.phase.busy();
        self.phase = phase;
        if phase.busy() && !was_busy {
            self.started = Some(Instant::now());
            self.elapsed = Duration::ZERO;
            self.rows = 0;
            self.error = None;
            self.finished_at = None;
            // Repaint the elapsed time while busy; nothing ticks when idle.
            self.ticker = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        } else if !phase.busy() {
            if let Some(t) = self.started {
                self.elapsed = t.elapsed();
            }
            if was_busy {
                self.finished_at = Some(Instant::now());
            }
            self.ticker = None;
        }
        cx.notify();
    }

    pub fn elapsed_now(&self) -> Duration {
        match (self.phase.busy(), self.started) {
            (true, Some(t)) => t.elapsed(),
            _ => self.elapsed,
        }
    }
}

impl Render for RunStatus {
    /// The run part of the status bar (v3): state while not idle, last run time, rows.
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let dot = match self.phase {
            Phase::Idle | Phase::Done => None,
            Phase::Connecting | Phase::Running | Phase::Paused => Some(theme.primary),
            Phase::Cancelling | Phase::Cancelled => Some(theme.warning),
            Phase::Failed => Some(theme.danger),
        };
        let show_counts = !matches!(self.phase, Phase::Idle | Phase::Connecting);
        let item = |id: &'static str| {
            h_flex()
                .id(id)
                .flex_shrink_0()
                .gap_1p5()
                .items_center()
                .whitespace_nowrap()
        };
        h_flex()
            .gap_4()
            .items_center()
            .when(self.plain_editor, |b| {
                b.child(item("plain").child(t!("editor.plain_mode").to_string()))
            })
            .when_some(dot, |b, dot| {
                b.child(
                    item("phase")
                        .child(
                            if matches!(self.phase, Phase::Connecting | Phase::Running) {
                                crate::shell::style::spinner(dot)
                            } else {
                                div().size(px(6.)).rounded_full().bg(dot).into_any_element()
                            },
                        )
                        .child(self.view().status),
                )
            })
            .when(show_counts, |b| {
                b.child(
                    item("elapsed").child(
                        t!("shell.ran_in", t = fmt::duration(self.elapsed_now())).into_owned(),
                    ),
                )
                .child(item("rows").child(fmt::rows(self.rows)))
            })
            .when_some(self.session_id.filter(|_| self.phase.busy()), |b, id| {
                b.child(
                    item("session").child(t!("status.session", id = id.to_string()).into_owned()),
                )
            })
    }
}

/// "Ln, Col" of one editor, for the status bar. Its own entity so that only
/// this text re-renders when the cursor moves (the shell never reads the
/// editor while rendering).
pub struct Caret {
    pub line: u32,
    pub col: u32,
    _sub: Subscription,
}

impl Caret {
    pub fn new(
        state: &Entity<gpui_kit::component::input::EditorState>,
        cx: &mut Context<Self>,
    ) -> Self {
        let p = state.read(cx).cursor_position();
        let _sub = cx.observe(state, |this: &mut Self, state, cx| {
            let p = state.read(cx).cursor_position();
            if (p.line, p.character) != (this.line, this.col) {
                this.line = p.line;
                this.col = p.character;
                cx.notify();
            }
        });
        Self {
            line: p.line,
            col: p.character,
            _sub,
        }
    }
}

impl Render for Caret {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().flex_none().whitespace_nowrap().child(
            t!(
                "shell.ln_col",
                ln = (self.line + 1).to_string(),
                col = (self.col + 1).to_string()
            )
            .into_owned(),
        )
    }
}
