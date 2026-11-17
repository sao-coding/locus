//! Query tabs: the tab entity (editor + results + status), the SQL editor and
//! autocomplete, running / cancelling, editor tools (format, find, explain).
pub mod apply_state;
pub mod completion;
pub mod diagram_pane;
pub mod doc_state;
pub mod editor;
pub mod export;
pub mod findbar;
pub mod page_slot;
pub mod paging_state;
pub mod params;
pub mod run;
pub mod run_binding;
pub mod run_output;
pub mod run_trace;
pub mod status;
pub mod tab;
pub mod tools;
pub mod txn;
pub mod txn_slots;
