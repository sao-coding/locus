//! File and unsaved-change state of a query tab.

#[derive(Default)]
pub struct DocState {
    /// Text changed since the last time the Space was written to disk.
    pub dirty: bool,
    /// The .sql file of this tab, if any.
    pub path: Option<String>,
    /// Changed since opened, saved or created (the unsaved marker).
    pub modified: bool,
    /// Editor text as opened or last saved: a change event that leaves the text equal to it
    /// (the editor reporting its initial value) is not an edit.
    pub baseline: String,
}
