//! Bookkeeping of the statements of the run in flight (for the Messages tab and the row count).

use std::time::Instant;

pub struct RunTrace {
    /// Statement texts of the running call, for the Messages tab.
    pub(crate) stmts: Vec<String>,
    pub(crate) stmt_mark: Instant,
    pub(crate) open_stmt: Option<usize>,
    /// Dialect and Production flag of the connection of the last run, for the row count.
    pub(crate) count_ctx: Option<(db_core::sql::SqlDialect, bool)>,
}

impl Default for RunTrace {
    fn default() -> Self {
        Self {
            stmts: Vec::new(),
            stmt_mark: Instant::now(),
            open_stmt: None,
            count_ctx: None,
        }
    }
}
