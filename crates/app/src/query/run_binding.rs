//! Parameter values of a tab and the statement as written, for the next run and its History entry.

#[derive(Default)]
pub struct RunBinding {
    /// Parameter values of the last Run dialog by name (remembered per tab).
    pub values: std::collections::HashMap<String, locus_core::params::ParamValue>,
    /// Bound values for the next `run_sql` (set by the parameter flow, taken there).
    pub bound: Vec<db_core::Param>,
    /// Statement as written plus its values, for the History entry of the next run (set with `bound`).
    pub hist_written: Option<(String, Vec<locus_core::history::HistParam>)>,
    pub(crate) run_written: Option<(String, Vec<locus_core::history::HistParam>)>,
}
