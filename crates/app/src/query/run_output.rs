//! What the last run produced: result tabs, Messages and row counters.

use crate::query::tab::{Message, ResultTab};

#[derive(Default)]
pub struct RunOutput {
    pub results: Vec<ResultTab>,
    pub messages: Vec<Message>,
    /// Index into `results`; `results.len()` = the Messages tab.
    pub active: usize,
    pub(crate) rows_total: u64,
    pub(crate) truncated: Option<u64>,
}
