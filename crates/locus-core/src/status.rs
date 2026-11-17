//! Run phase of a query tab and error titles (the status bar view is in the app).

use db_core::ErrorKind;
use rust_i18n::t;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Connecting,
    Running,
    Cancelling,
    /// A paged query waits for "Next" (the server cursor is held open).
    Paused,
    Done,
    Cancelled,
    Failed,
}

impl Phase {
    pub fn busy(self) -> bool {
        matches!(self, Phase::Connecting | Phase::Running | Phase::Cancelling)
    }

    pub fn label(self) -> String {
        match self {
            Phase::Idle => t!("state.idle"),
            Phase::Connecting => t!("state.connecting"),
            Phase::Running => t!("state.running"),
            Phase::Cancelling => t!("state.cancelling"),
            Phase::Paused => t!("state.paused"),
            Phase::Done => t!("state.done"),
            Phase::Cancelled => t!("state.cancelled"),
            Phase::Failed => t!("state.failed"),
        }
        .into_owned()
    }
}

pub fn error_title(kind: ErrorKind) -> String {
    match kind {
        ErrorKind::Connect => t!("error.connect"),
        ErrorKind::Auth => t!("error.auth"),
        ErrorKind::Tls => t!("error.tls"),
        ErrorKind::Query => t!("error.query"),
        ErrorKind::Timeout => t!("error.timeout"),
        ErrorKind::Internal => t!("error.internal"),
    }
    .into_owned()
}
