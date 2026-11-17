//! Pinned session and notes of a tab in Manual transaction mode (the mode itself is `QueryTab::tx`;
//! the methods are in `query/txn.rs`).

use gpui_kit::*;

#[derive(Default)]
pub struct TxnSlots {
    /// The pinned connection of Manual mode.
    pub session: Option<locus_core::txn::TxSession>,
    /// Engine and statements of the run in flight on the pinned session.
    pub(crate) run: Option<(locus_core::txn::Engine, Vec<db_core::classify::Classified>)>,
    /// What the pill says besides the count (DDL committed, rolled back by the server, ...).
    pub note: Option<String>,
    pub(crate) idle_task: Option<Task<()>>,
}
