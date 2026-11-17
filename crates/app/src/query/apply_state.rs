//! Apply of staged grid edits (the methods are in `tab/edit.rs`).

use gpui_kit::*;

#[derive(Default)]
pub struct ApplyState {
    /// Apply in flight.
    pub applying: bool,
    pub error: Option<String>,
    pub(crate) task: Option<Task<()>>,
}
