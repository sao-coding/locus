//! One query tab: SQL editor, its own results (one tab per result set plus
//! Messages), its own run status and its own query task. Tabs keep streaming
//! while another tab is in front; switching Space or pane never stops them.

use crate::query::editor::SqlEditor;
use crate::query::export::ExportJob;
use crate::query::status::RunStatus;
use crate::results::grid::{EditTarget, ResultGrid, open_viewer, param_text, truncate_batch};
use db_core::{Canceller, DbError, Driver, ErrorKind, Outcome, QueryEvent, QueryOptions};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Editor;
use gpui_kit::component::menu::PopupMenu;
use locus_core::apply::Statement;
use locus_core::fmt;
use locus_core::pool::{self, Slot};
use locus_core::status::{Phase, error_title};
use locus_core::store::SavedConnection;

use crate::shell::PopDialog;
use crate::shell::scale as sc;
use gpui_kit::component::table::{DataTable, TableEvent, TableSelection, TableState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex,
    resizable_panel, v_flex, v_resizable,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rust_i18n::t;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    /// Selection if any, else the statement at the cursor (Ctrl+Enter).
    Current,
    /// The whole editor, as one call (Ctrl+Shift+Enter).
    Script,
}

/// What the tab asks the shell for (it owns connections and passwords).
pub enum TabEvent {
    /// Title, connection, pin or text changed: persist.
    Changed,
    /// Run `last_sql` again (after Apply, to show the stored values).
    Rerun,
    /// Run the query again with the header-menu filters and orders around it.
    RerunView,
    /// A run ended: one query-history entry.
    Finished(locus_core::history::Entry),
    /// Open a table in a new tab (diagram double-click).
    OpenTable { conn_id: String, sql: String },
    /// "Relations" of the result: a diagram limited to the tables of its query.
    ShowQueryRelations {
        conn_id: String,
        tables: Vec<(Option<String>, String)>,
    },
    /// Diagram tab: centre on a table (opens or reuses a centred tab).
    DiagramCentre {
        conn_id: String,
        schema: String,
        table: String,
    },
    /// Diagram tab: a relation line was clicked; `sql` is the JOIN clause.
    DiagramJoin { sql: String },
    /// Diagram tab: show this table in the Schema panel.
    DiagramShowInSchema { table: String },
    /// Diagram tab: header or footer must be redrawn (zoom, scope, counts).
    DiagramChanged,
    /// Apply was clicked: the shell checks the connection's lock and
    /// confirmations, then calls `apply_edits`.
    ApplyRequest,
    /// Apply ended (done or failed).
    ApplyDone,
    /// Source tab: Compile / Save to database was clicked.
    CompileSource,
    /// Source tab: read the object's source again.
    ReloadSource,
    /// Structure tab: open another table's structure (foreign key click, result strip; empty schema = the default one).
    ShowStructure {
        conn_id: String,
        schema: String,
        table: String,
    },
    /// Structure tab: open a trigger's Source tab.
    OpenTrigger {
        conn_id: String,
        schema: String,
        name: String,
    },
}

#[derive(Clone, Debug)]
pub enum MsgKind {
    Rows(u64),
    Affected(u64),
    /// Error with the rows of this statement that had arrived.
    Error(DbError, u64),
    Cancelled(u64),
    Truncated(u64),
}

#[derive(Clone, Debug)]
pub struct Message {
    pub index: usize,
    pub sql: Option<String>,
    pub kind: MsgKind,
    pub elapsed: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Note {
    Cancelled,
    Failed,
    Truncated(u64),
}

pub struct ResultTab {
    pub table: Entity<TableState<ResultGrid>>,
    pub note: Option<Note>,
    pub rows: u64,
    /// The single table the statement read, for the tab label.
    pub name: Option<String>,
    /// The statement, elided, for the tab tooltip.
    pub tip: String,
    _sub: Subscription,
}

struct Job {
    _task: Task<()>,
    canceller: Option<Canceller>,
}

pub struct QueryTab {
    pub id: String,
    pub title: Option<String>,
    pub conn_id: Option<String>,
    pub pinned: bool,
    /// This tab shows a relationship diagram instead of an editor.
    pub diagram: Option<(String, String)>,
    pub limit: Option<u64>,
    pub editor: Entity<SqlEditor>,
    pub status: Entity<RunStatus>,
    /// Cursor position for the status bar.
    pub caret: Entity<crate::query::status::Caret>,
    /// Result area state: panes, filter, plan, timing.
    pub ui: crate::results::ui::ResultsUi,
    pub last_sql: String,
    /// Header-menu filters and orders (colview.rs).
    pub(crate) cv: colview::ColState,
    pub driver: Option<Arc<locus_core::driver::AnyDriver>>,
    pub slot: Option<Slot>,
    job: Option<Job>,
    pub has_run: bool,
    /// Cached first line of SQL, the title when the user set none.
    pub auto_title: String,
    run_at: u64,
    pub export: Option<ExportJob>,
    /// The grid's right-click menu, while open.
    grid_menu: Option<(Entity<PopupMenu>, Point<Pixels>, Subscription)>,
    /// The find / replace bar (`query/findbar.rs`).
    pub(crate) find: Option<crate::query::findbar::FindBar>,
    /// What autocomplete completes against (connection + its slot).
    pub completion: Arc<parking_lot::Mutex<crate::query::completion::Target>>,
    /// The Space's connections, for the picker (set by the shell).
    pub space_conns: Vec<SavedConnection>,
    /// Source tab of a stored program unit (`crate::routines`); not saved
    /// with the Space: a restored tab is a plain query tab.
    pub(crate) routine: Option<crate::routines::Source>,
    /// Structure tab of a table (`crate::structure`); its editor is the read-only DDL.
    pub(crate) structure: Option<crate::structure::Structure>,
    /// Transaction mode and state (`query/txn.rs`).
    pub tx: locus_core::txn::TxState,
    _subs: Vec<Subscription>,
    /// Apply of staged edits (`tab/edit.rs`).
    pub ap: crate::query::apply_state::ApplyState,
    /// Paging and the background row count (`tab/paging.rs`).
    pub pg: crate::query::paging_state::Paging,
    /// Diagram tab state (`crate::diagram::tab`).
    pub dgm: crate::query::diagram_pane::DiagramPane,
    /// Pinned session of Manual mode (`query/txn.rs`).
    pub txn: crate::query::txn_slots::TxnSlots,
    /// History / Settings page in place of the editor.
    pub pageslot: crate::query::page_slot::PageSlot,
    /// Parameter values and the statement as written.
    pub bind: crate::query::run_binding::RunBinding,
    /// Statements of the run in flight.
    pub(crate) trace: crate::query::run_trace::RunTrace,
    /// File and unsaved state.
    pub doc: crate::query::doc_state::DocState,
    /// Result tabs, Messages and row counters of the last run.
    pub out: crate::query::run_output::RunOutput,
}

impl EventEmitter<TabEvent> for QueryTab {}

impl QueryTab {
    pub fn new(
        data: &locus_core::spaces::TabData,
        cache: crate::query::completion::Cache,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| SqlEditor::new(&data.sql, window, cx));
        let completion = Arc::new(parking_lot::Mutex::new(
            crate::query::completion::Target::default(),
        ));
        {
            let provider = std::rc::Rc::new(crate::query::completion::SqlCompletions {
                cache,
                target: completion.clone(),
            });
            let state = editor.read(cx).state.clone();
            state.update(cx, |s, cx| {
                s.lsp_mut().completion_provider = Some(provider);
                s.refresh(cx);
            });
        }
        let status = cx.new(|_| RunStatus::new());
        let plain = editor.read(cx).plain;
        status.update(cx, |s, _| s.plain_editor = plain);
        let mut subs = Vec::new();
        subs.push(cx.observe(&editor, |this: &mut Self, editor, cx| {
            let plain = editor.read(cx).plain;
            this.status.update(cx, |s, cx| {
                if s.plain_editor != plain {
                    s.plain_editor = plain;
                    cx.notify();
                }
            });
        }));
        let state = editor.read(cx).state.clone();
        let auto_title = first_line_title(state.read(cx));
        let caret = cx.new(|cx| crate::query::status::Caret::new(&state, cx));
        subs.push(cx.subscribe(
            &state,
            |this: &mut Self, _, ev: &gpui_kit::component::input::InputEvent, cx| {
                if let gpui_kit::component::input::InputEvent::Change = ev {
                    let t = first_line_title(this.editor.read(cx).state.read(cx));
                    if t != this.auto_title {
                        this.auto_title = t;
                        cx.notify();
                    }
                    let unchanged = this.editor.read(cx).state.read(cx).value().as_ref()
                        == this.doc.baseline.as_str();
                    if !unchanged && (!this.doc.dirty || !this.doc.modified) {
                        this.doc.dirty = true;
                        this.doc.modified = true;
                        cx.notify();
                    }
                    cx.emit(TabEvent::Changed);
                }
            },
        ));
        Self {
            id: data.id.clone(),
            title: data.title.clone(),
            conn_id: data.conn_id.clone(),
            pinned: data.pinned,
            diagram: data.diagram.clone(),
            limit: data.limit,
            editor,
            status,
            caret,
            ui: crate::results::ui::ResultsUi::new(window, cx),
            last_sql: String::new(),
            cv: Default::default(),
            driver: None,
            slot: None,
            job: None,
            has_run: false,
            auto_title,
            run_at: 0,
            export: None,
            grid_menu: None,
            find: None,
            tx: Default::default(),
            completion,
            space_conns: Vec::new(),
            routine: None,
            structure: None,
            _subs: subs,
            ap: Default::default(),
            pg: Default::default(),
            dgm: crate::query::diagram_pane::DiagramPane {
                view: None,
                state: crate::diagram::tab::DiagramState::from_data(data),
                driver: None,
                error: None,
                loading: false,
                task: None,
                save: None,
                sub: None,
                menu: None,
                menu_req: None,
            },
            txn: Default::default(),
            pageslot: crate::query::page_slot::PageSlot {
                kind: data.page,
                ..Default::default()
            },
            bind: Default::default(),
            trace: Default::default(),
            doc: crate::query::doc_state::DocState {
                path: data.path.clone(),
                modified: data.modified,
                baseline: data.sql.clone(),
                dirty: false,
            },
            out: Default::default(),
        }
    }

    pub fn data(&self, cx: &App) -> locus_core::spaces::TabData {
        locus_core::spaces::TabData {
            id: self.id.clone(),
            title: self.title.clone(),
            conn_id: self.conn_id.clone(),
            sql: self.editor.read(cx).state.read(cx).value().to_string(),
            pinned: self.pinned,
            limit: self.limit,
            path: self.doc.path.clone(),
            modified: self.doc.modified,
            diagram: self.diagram.clone(),
            diagram_tables: self.diagram_legacy_tables(cx),
            diagram_state: self
                .diagram
                .is_some()
                .then(|| self.diagram_state_now(cx))
                .and_then(|s| serde_json::to_value(s).ok()),
            page: self.pageslot.kind,
            obj: self.obj_ref(),
        }
    }

    /// Title as shown: the user's, else the first line of SQL.
    pub fn display_title(&self, cx: &App) -> String {
        if let Some(t) = &self.title {
            return t.clone();
        }
        match self.pageslot.kind {
            Some(locus_core::spaces::TabPage::History) => {
                return t!("hp.tab_title").into_owned();
            }
            Some(locus_core::spaces::TabPage::Settings) => {
                return t!("settings.title").into_owned();
            }
            Some(locus_core::spaces::TabPage::Saved) => return t!("sq.tab_title").into_owned(),
            Some(locus_core::spaces::TabPage::Redis | locus_core::spaces::TabPage::Activity)
            | None => {}
        }
        if self.diagram.is_some() {
            return self.diagram_title(cx);
        }
        if let Some(s) = &self.structure {
            return s.r.table.clone();
        }
        if let Some(p) = &self.doc.path {
            return std::path::Path::new(p)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone());
        }
        let _ = cx;
        if self.auto_title.is_empty() {
            t!("tab.untitled").into_owned()
        } else {
            self.auto_title.clone()
        }
    }

    /// Pushes the inputs of the run-state mapping (`locus_core::runview`) to the status entity.
    pub(crate) fn sync_status(&self, cx: &mut Context<Self>) {
        let (limit, total, released, capped) = (
            self.limit,
            self.pg.total_count,
            self.pg.expired,
            self.out.truncated.is_some(),
        );
        self.status.update(cx, |s, cx| {
            if (s.limit, s.total, s.released, s.capped) != (limit, total, released, capped) {
                (s.limit, s.total, s.released, s.capped) = (limit, total, released, capped);
                cx.notify();
            }
        });
    }

    pub fn busy(&self, cx: &App) -> bool {
        self.status.read(cx).phase.busy()
    }

    /// The grid of the result tab in front, if a result set is in front.
    pub fn current_table(&self) -> Option<&Entity<TableState<ResultGrid>>> {
        self.out.results.get(self.out.active).map(|r| &r.table)
    }
}
/// `in_tx`: the tab is in Manual mode, so Apply joins the transaction behind a savepoint and commits nothing.
pub fn preview_text(stmts: &[Statement], in_tx: bool) -> String {
    let mut out = String::from(if in_tx {
        "SAVEPOINT wb_apply;\n"
    } else {
        "START TRANSACTION;\n"
    });
    for s in stmts {
        out.push_str(&s.sql);
        out.push_str(";\n");
        if !s.params.is_empty() {
            let vals: Vec<String> = s.params.iter().map(param_text).collect();
            out.push_str(&format!("  -- ? = {}\n", vals.join(", ")));
        }
    }
    if !in_tx {
        out.push_str("COMMIT;\n");
    }
    out
}

/// The server does not understand the client command `DELIMITER`.
fn has_delimiter(sql: &str) -> bool {
    sql.lines().any(|l| {
        l.trim_start()
            .get(..10)
            .is_some_and(|p| p.eq_ignore_ascii_case("delimiter "))
    })
}

/// `db_core::sql::statement_at`.
fn statement_at(sql: &str, cursor: usize) -> Option<std::ops::Range<usize>> {
    db_core::sql::statement_at(sql, cursor)
}

fn split_statements(sql: &str) -> Vec<std::ops::Range<usize>> {
    db_core::sql::split_statements(sql)
}

// ---- rendering ---------------------------------------------------------------
mod colview;
mod edit;
mod paging;
mod render;
mod run;

pub(crate) use render::empty;
pub use render::first_line_title;
