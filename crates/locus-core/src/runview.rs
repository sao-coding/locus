//! One mapping from the run state of a query tab to everything the user sees:
//! the Run / Stop button, the status bar text, the result footer and the
//! offered actions. Pure, so every indicator reads the same answer and each
//! state has a unit test. The app only renders what this returns.

use crate::fmt;
use crate::status::Phase;
use rust_i18n::t;

/// What the app knows about the tab's run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunInput {
    pub phase: Phase,
    /// Rows fetched so far (all result sets of the run).
    pub rows: u64,
    /// The tab's row limit (`None` = unlimited).
    pub limit: Option<u64>,
    /// Total rows of the query when the background count is known.
    pub total: Option<u64>,
    /// The paused query was given up (idle timeout or released): the cursor is gone.
    pub released: bool,
    /// Stopped by the memory cap (settings), not by the user.
    pub capped: bool,
}

impl RunInput {
    pub fn new(phase: Phase, rows: u64, limit: Option<u64>) -> Self {
        Self {
            phase,
            rows,
            limit,
            total: None,
            released: false,
            capped: false,
        }
    }
}

/// The run-state a user can tell apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Connecting,
    /// The server executes, no row yet.
    Running,
    /// Rows are arriving.
    Streaming,
    Cancelling,
    /// Paused on the server cursor: the limit / page end was reached, more rows exist.
    PausedMore,
    /// Complete: every row of the result is here.
    Finished,
    /// Stopped by the user; the rows that had arrived stay.
    Cancelled,
    Failed,
    /// The paused query was released (or expired): the rest is gone until Run again.
    Released,
    /// Stopped by the memory cap.
    Capped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Run,
    /// Stop is live.
    Stop,
    /// Stop was pressed; waiting for the server (shown, not clickable).
    Stopping,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Actions {
    /// Fetch the next page ("Load more").
    pub load_more: bool,
    /// Fetch every remaining row.
    pub load_all: bool,
    /// Give the server cursor back, keep the rows.
    pub release: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunView {
    pub state: State,
    pub button: Button,
    /// Status bar text.
    pub status: String,
    /// Result footer text (empty = nothing to say).
    pub footer: String,
    pub actions: Actions,
}

pub fn state_of(i: &RunInput) -> State {
    match i.phase {
        Phase::Idle => State::Idle,
        Phase::Connecting => State::Connecting,
        Phase::Running if i.rows == 0 => State::Running,
        Phase::Running => State::Streaming,
        Phase::Cancelling => State::Cancelling,
        Phase::Paused => State::PausedMore,
        Phase::Done if i.capped => State::Capped,
        Phase::Done if i.released => State::Released,
        Phase::Done => State::Finished,
        Phase::Cancelled => State::Cancelled,
        Phase::Failed => State::Failed,
    }
}

/// The view in the current language.
pub fn view(i: &RunInput) -> RunView {
    view_in(i, &rust_i18n::locale())
}

/// The view in `locale` (tests pin it).
pub fn view_in(i: &RunInput, locale: &str) -> RunView {
    let state = state_of(i);
    let n = fmt::count(i.rows);
    let button = match state {
        State::Connecting | State::Running | State::Streaming => Button::Stop,
        State::Cancelling => Button::Stopping,
        _ => Button::Run,
    };
    let paused = state == State::PausedMore;
    let actions = Actions {
        load_more: paused,
        load_all: paused,
        release: paused,
    };
    let status = match state {
        State::Idle => t!("runview.status_idle", locale = locale),
        State::Connecting => t!("runview.status_connecting", locale = locale),
        State::Running => t!("runview.status_running", locale = locale),
        State::Streaming => t!("runview.status_streaming", locale = locale, n = &n),
        State::Cancelling => t!("runview.status_cancelling", locale = locale),
        State::PausedMore => t!("runview.status_paused", locale = locale),
        State::Finished => t!("runview.status_done", locale = locale),
        State::Cancelled => t!("runview.status_cancelled", locale = locale),
        State::Failed => t!("runview.status_failed", locale = locale),
        State::Released => t!("runview.status_released", locale = locale),
        State::Capped => t!("runview.status_capped", locale = locale),
    }
    .into_owned();
    let footer = match state {
        State::Idle | State::Connecting | State::Running => String::new(),
        State::Streaming | State::Cancelling => {
            t!("runview.footer_streaming", locale = locale, n = &n).into_owned()
        }
        State::PausedMore => match (i.total, i.limit) {
            (Some(total), _) => t!(
                "runview.footer_paused_total",
                locale = locale,
                n = &n,
                total = fmt::count(total)
            )
            .into_owned(),
            (None, Some(_)) => {
                t!("runview.footer_paused_limit", locale = locale, n = &n).into_owned()
            }
            (None, None) => t!("runview.footer_paused", locale = locale, n = &n).into_owned(),
        },
        State::Finished => t!("runview.footer_done", locale = locale, n = &n).into_owned(),
        State::Cancelled => t!("runview.footer_cancelled", locale = locale, n = &n).into_owned(),
        State::Failed => t!("runview.footer_failed", locale = locale, n = &n).into_owned(),
        State::Released => t!("runview.footer_released", locale = locale, n = &n).into_owned(),
        State::Capped => t!("runview.footer_capped", locale = locale, n = &n).into_owned(),
    };
    RunView {
        state,
        button,
        status,
        footer,
        actions,
    }
}

/// Tooltip of the row-limit chip.
pub fn limit_hint(limit: Option<u64>, locale: &str) -> String {
    match limit {
        Some(l) => t!("runview.limit_hint", locale = locale, n = fmt::count(l)).into_owned(),
        None => t!("runview.limit_hint_unlimited", locale = locale).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(phase: Phase, rows: u64, f: impl FnOnce(&mut RunInput)) -> RunView {
        let mut i = RunInput::new(phase, rows, Some(1000));
        f(&mut i);
        view_in(&i, "en")
    }

    #[test]
    fn idle() {
        let r = v(Phase::Idle, 0, |_| {});
        assert_eq!(
            (r.state, r.button, r.actions),
            (State::Idle, Button::Run, Actions::default())
        );
        assert_eq!(r.footer, "");
    }

    #[test]
    fn connecting_and_running_show_stop_without_rows() {
        for (p, s) in [
            (Phase::Connecting, State::Connecting),
            (Phase::Running, State::Running),
        ] {
            let r = v(p, 0, |_| {});
            assert_eq!((r.state, r.button), (s, Button::Stop));
            assert_eq!(r.actions, Actions::default());
        }
    }

    #[test]
    fn streaming_is_stop_with_rows_so_far() {
        let r = v(Phase::Running, 300, |_| {});
        assert_eq!((r.state, r.button), (State::Streaming, Button::Stop));
        assert!(r.footer.contains("300") && !r.footer.contains("more available"));
    }

    #[test]
    fn cancelling_is_stopping() {
        let r = v(Phase::Cancelling, 5, |_| {});
        assert_eq!((r.state, r.button), (State::Cancelling, Button::Stopping));
        assert_eq!(r.actions, Actions::default());
    }

    /// The reported case: limit 1,000 of 40,450 rows. Not Stop, not "all loaded".
    #[test]
    fn paused_at_limit_offers_more_and_is_not_stop() {
        let r = v(Phase::Paused, 1000, |_| {});
        assert_eq!(r.state, State::PausedMore);
        assert_eq!(r.button, Button::Run);
        assert_eq!(
            r.actions,
            Actions {
                load_more: true,
                load_all: true,
                release: true
            }
        );
        assert_eq!(r.footer, "1,000 rows shown (limit) · more available");
        assert!(!r.status.to_lowercase().contains("done") && !r.footer.contains("All "));
    }

    #[test]
    fn paused_with_known_total() {
        let r = v(Phase::Paused, 1000, |i| i.total = Some(40_450));
        assert_eq!(r.footer, "1,000 of 40,450 rows shown · more available");
    }

    #[test]
    fn paused_without_limit_has_no_limit_wording() {
        let r = view_in(&RunInput::new(Phase::Paused, 500, None), "en");
        assert_eq!(r.footer, "500 rows shown · more available");
    }

    #[test]
    fn finished_is_the_only_all_loaded() {
        let r = v(Phase::Done, 40_450, |_| {});
        assert_eq!(
            (r.state, r.button, r.actions),
            (State::Finished, Button::Run, Actions::default())
        );
        assert_eq!(r.footer, "All 40,450 rows loaded");
        for (s, p) in [
            (State::PausedMore, Phase::Paused),
            (State::Released, Phase::Done),
            (State::Capped, Phase::Done),
            (State::Cancelled, Phase::Cancelled),
            (State::Failed, Phase::Failed),
        ] {
            let r = v(p, 1000, |i| {
                i.released = s == State::Released;
                i.capped = s == State::Capped;
            });
            assert_eq!(r.state, s);
            assert!(!r.footer.contains("All "), "{s:?}: {}", r.footer);
        }
    }

    #[test]
    fn cancelled_keeps_rows() {
        let r = v(Phase::Cancelled, 120, |_| {});
        assert_eq!(
            (r.state, r.button, r.footer.as_str()),
            (State::Cancelled, Button::Run, "Cancelled · 120 rows kept")
        );
    }

    #[test]
    fn failed() {
        let r = v(Phase::Failed, 0, |_| {});
        assert_eq!((r.state, r.button), (State::Failed, Button::Run));
    }

    #[test]
    fn released_says_the_rest_is_gone() {
        let r = v(Phase::Done, 1000, |i| i.released = true);
        assert_eq!(
            (r.state, r.button, r.actions),
            (State::Released, Button::Run, Actions::default())
        );
        assert!(r.footer.contains("released") && r.footer.contains("Run again"));
    }

    #[test]
    fn capped_by_memory() {
        let r = v(Phase::Done, 1_000_000, |i| i.capped = true);
        assert_eq!(r.state, State::Capped);
        assert!(r.footer.contains("memory"));
    }

    #[test]
    fn stop_only_while_fetching() {
        for p in [
            Phase::Idle,
            Phase::Paused,
            Phase::Done,
            Phase::Cancelled,
            Phase::Failed,
        ] {
            assert_ne!(v(p, 10, |_| {}).button, Button::Stop, "{p:?}");
        }
    }

    #[test]
    fn zh_cn_every_state_has_text() {
        for p in [
            Phase::Idle,
            Phase::Connecting,
            Phase::Running,
            Phase::Cancelling,
            Phase::Paused,
            Phase::Done,
            Phase::Cancelled,
            Phase::Failed,
        ] {
            let r = view_in(&RunInput::new(p, 9, Some(1000)), "zh-CN");
            assert!(
                !r.status.contains("runview.") && !r.footer.contains("runview."),
                "{p:?}"
            );
        }
        assert!(
            view_in(&RunInput::new(Phase::Paused, 1000, Some(1000)), "zh-CN")
                .footer
                .contains("还有更多")
        );
        assert!(limit_hint(None, "zh-CN").contains("内存"));
    }

    #[test]
    fn zh_tw_every_state_has_text() {
        let cases = [
            (Phase::Idle, 0),
            (Phase::Connecting, 0),
            (Phase::Running, 0),
            (Phase::Running, 5),
            (Phase::Cancelling, 5),
            (Phase::Paused, 1000),
            (Phase::Done, 9),
            (Phase::Cancelled, 9),
            (Phase::Failed, 9),
        ];
        for (p, n) in cases {
            let r = view_in(&RunInput::new(p, n, Some(1000)), "zh-TW");
            assert!(
                !r.status.contains("runview.") && !r.footer.contains("runview."),
                "{p:?}"
            );
        }
        assert!(
            view_in(&RunInput::new(Phase::Paused, 1000, Some(1000)), "zh-TW")
                .footer
                .contains("還有更多")
        );
        assert!(limit_hint(Some(1000), "zh-TW").contains("1,000"));
        assert!(limit_hint(None, "zh-TW").contains("記憶體"));
    }
}
