//! Paging and row-count state of a query tab (the methods are in `tab/paging.rs`).

use gpui_kit::*;

/// Page in front, rows to fetch before pausing, pause state and the background COUNT.
pub struct Paging {
    pub page: usize,
    pub(crate) fetch_target: u64,
    pub paused: bool,
    pub expired: bool,
    pub show_all: bool,
    /// Total rows of a paused query (background COUNT), when known.
    pub(crate) total_count: Option<u64>,
    pub(crate) count_task: Option<Task<()>>,
    pub(crate) pager_tx: Option<tokio::sync::mpsc::UnboundedSender<bool>>,
    pub(crate) idle_task: Option<Task<()>>,
    pub(crate) rows_cap: u64,
    pub(crate) row_cap: locus_core::rowcap::RowCap,
}

impl Default for Paging {
    fn default() -> Self {
        Self {
            page: 0,
            fetch_target: u64::MAX,
            paused: false,
            expired: false,
            show_all: false,
            total_count: None,
            count_task: None,
            pager_tx: None,
            idle_task: None,
            rows_cap: 1_000_000,
            row_cap: locus_core::rowcap::RowCap::new(1_000_000),
        }
    }
}
