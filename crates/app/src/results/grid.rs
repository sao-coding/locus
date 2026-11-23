//! The results grid: a `TableDelegate` over a growing `db_core::ResultSet`.
//! Cells are read straight from the columnar batches; nothing is converted to
//! a row-of-strings copy. Staged edits (feature 7) sit beside the result set
//! and are drawn over it; the result set itself is never changed.

use db_core::{Batch, BatchBuilder, Cell, ColumnMeta, Param, ResultSet};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableEvent, TableState};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use locus_core::apply::Statement;
use rust_i18n::t;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

/// Longest prefix of a cell drawn in the grid; the full value stays in the
/// result set (the value viewer shows all of it).
const MAX_CELL_CHARS: usize = 200;

/// What makes a result editable: one table, all of its primary-key columns.
#[derive(Clone, Debug)]
pub struct EditTarget {
    /// Result column indexes of the primary key, in key order.
    pub key: Vec<usize>,
    /// Quoted `schema.table` and quoted column names (from the driver).
    pub quoted_table: String,
    pub quoted_columns: Vec<String>,
}

/// Edits waiting for Apply. Rows are source rows (arrival order), so sorting
/// does not move an edit to another row.
#[derive(Clone, Debug, Default)]
pub struct Staged {
    /// `(source row, column) -> new value` (`None` = NULL).
    pub cells: BTreeMap<(usize, usize), Option<String>>,
    /// New rows; a `None` entry in the outer option = column left to its default.
    pub new_rows: Vec<Vec<Option<Option<String>>>>,
    pub deleted: BTreeSet<usize>,
}

impl Staged {
    pub fn count(&self) -> usize {
        let edited_rows: BTreeSet<usize> = self
            .cells
            .keys()
            .map(|(r, _)| *r)
            .filter(|r| !self.deleted.contains(r))
            .collect();
        edited_rows.len() + self.new_rows.len() + self.deleted.len()
    }
}

pub fn param_text(p: &Param) -> String {
    match p {
        Param::Null => "NULL".into(),
        Param::Text(s) => format!("'{}'", s.replace('\'', "''")),
        Param::Bytes(b) => hex(b, usize::MAX),
    }
}

pub struct ResultGrid {
    pub rs: ResultSet,
    widths: Vec<f32>,
    widths_from_data: bool,
    /// Row order after a sort; `None` = arrival order.
    order: Option<Vec<u32>>,
    sort: Option<(usize, ColumnSort)>,
    /// Sorting is off while rows are still arriving.
    pub sortable: bool,
    pub target: Option<EditTarget>,
    /// Shown in one line above a read-only grid (`None` while undecided).
    pub read_only_reason: Option<String>,
    pub staged: Staged,
    editing: Option<(usize, usize, Entity<InputState>, Subscription)>,
    /// Text of the source table for "copy as INSERT" when not editable.
    pub insert_table: String,
    /// Range selection (anchor, end) in display rows / columns.
    pub range: Option<((usize, usize), (usize, usize))>,
    /// Page shown: (first row, page size) over the fetched rows; `None` = all.
    pub page: Option<(usize, usize)>,
    /// Source rows in display order while a sort or a filter is on; `None` = arrival order.
    view: Option<Vec<u32>>,
    /// The row filter (`None` = off), its text, and how far into the candidates it has read.
    filter: Option<locus_core::rowfilter::Filter>,
    pub filter_text: String,
    scan_pos: usize,
    dragging: bool,
    /// Column of the last right click (set from `TableEvent::RightClickedCell`): the delegate cannot read its own entity while the table builds the menu.
    pub right_col: usize,
    /// Foreign-key navigation: the single source table of the result and its keys.
    pub fk: Option<FkCtx>,
    /// Messages up to the query tab (open a tab, selection numbers ready).
    pub on_up: Option<Rc<dyn Fn(GridUp, &mut App)>>,
    sel_job: Option<SelJob>,
    sel_task: Option<Task<()>>,
    /// Numbers of the finished range selection (two or more cells).
    pub sel_summary: Option<locus_core::selstats::Summary>,
    /// Server-side filters and orders of this result (header menu); None = the menu is off (not a plain SELECT).
    pub colview: Option<locus_core::colfilter::ColView>,
    /// Filters saved for this query (enables Reset default filter).
    pub colsaved: bool,
}

impl ResultGrid {
    pub fn new() -> Self {
        Self {
            rs: ResultSet::new(Vec::new()),
            widths: Vec::new(),
            widths_from_data: false,
            order: None,
            sort: None,
            sortable: false,
            target: None,
            read_only_reason: None,
            staged: Staged::default(),
            editing: None,
            insert_table: String::new(),
            range: None,
            page: None,
            view: None,
            filter: None,
            filter_text: String::new(),
            scan_pos: 0,
            dragging: false,
            right_col: 0,
            fk: None,
            on_up: None,
            sel_job: None,
            sel_task: None,
            sel_summary: None,
            colview: None,
            colsaved: false,
        }
    }

    pub fn reset(&mut self, columns: Vec<ColumnMeta>) {
        // Header shows name and type: size for both (type text is smaller).
        self.widths = columns
            .iter()
            .map(|c| width_for(c.name.chars().count() + c.type_name.chars().count() * 3 / 4 + 4))
            .collect();
        self.insert_table = columns
            .iter()
            .find_map(|c| {
                c.origin
                    .as_ref()
                    .map(|o| format!("`{}`", o.table.replace('`', "``")))
            })
            .unwrap_or_else(|| "`table`".into());
        self.rs = ResultSet::new(columns);
        self.widths_from_data = false;
        self.order = None;
        self.view = None;
        self.filter = None;
        self.filter_text.clear();
        self.scan_pos = 0;
        self.sort = None;
        self.sortable = false;
        self.target = None;
        self.read_only_reason = None;
        self.staged = Staged::default();
        self.editing = None;
        self.fk = None;
        self.sel_job = None;
        self.sel_task = None;
        self.sel_summary = None;
    }

    /// Appends a batch. Returns true when the column layout changed (first
    /// batch: widths are fitted to the data) and the table needs a refresh.
    pub fn push(&mut self, batch: Batch) -> bool {
        let mut relayout = false;
        if !self.widths_from_data && batch.rows() > 0 {
            for (c, w) in self.widths.iter_mut().enumerate() {
                let mut chars = 0usize;
                for r in 0..batch.rows().min(200) {
                    chars = chars.max(match batch.cell(r, c) {
                        Cell::Null => 4,
                        Cell::Text(s) => s.chars().take(60).count(),
                        Cell::Bytes(b) => (b.len() * 2 + 12).min(60),
                    });
                }
                *w = w.max(width_for(chars + 1));
            }
            self.widths_from_data = true;
            relayout = true;
        }
        self.rs.push(batch);
        relayout
    }

    /// User-resized widths, kept across refreshes (and the end of the query).
    pub fn set_widths(&mut self, w: &[Pixels]) {
        for (i, p) in w.iter().enumerate() {
            if let Some(x) = self.widths.get_mut(i) {
                *x = f32::from(*p);
            }
        }
        self.widths_from_data = true;
    }

    /// Source row of display row `r`; `None` for a new (staged) row.
    pub fn source_row(&self, r: usize) -> Option<usize> {
        if r >= self.view_rows() {
            return None;
        }
        let abs = self.page.map(|(s, _)| s).unwrap_or(0) + r;
        Some(match &self.view {
            Some(v) => v.get(abs).map(|x| *x as usize).unwrap_or(abs),
            None => abs,
        })
    }

    /// Value as shown now: staged edit over the stored one. `None` = NULL.
    pub fn value(&self, r: usize, c: usize) -> Option<String> {
        match self.source_row(r) {
            Some(src) => {
                if let Some(v) = self.staged.cells.get(&(src, c)) {
                    return v.clone();
                }
                cell_string(self.rs.cell(src, c))
            }
            None => self
                .staged
                .new_rows
                .get(r - self.view_rows())
                .and_then(|row| row[c].clone().flatten()),
        }
    }

    fn is_binary(&self, r: usize, c: usize) -> bool {
        self.source_row(r)
            .is_some_and(|src| matches!(self.rs.cell(src, c), Cell::Bytes(_)))
    }

    pub fn editable(&self) -> bool {
        self.target.is_some()
    }

    // ---- staging ---------------------------------------------------------

    pub fn stage(&mut self, r: usize, c: usize, v: Option<String>) {
        match self.source_row(r) {
            Some(src) => {
                let stored = cell_string(self.rs.cell(src, c));
                if stored == v {
                    self.staged.cells.remove(&(src, c));
                } else {
                    self.staged.cells.insert((src, c), v);
                }
            }
            None => {
                let vr = self.view_rows();
                if let Some(row) = self.staged.new_rows.get_mut(r - vr) {
                    row[c] = Some(v);
                }
            }
        }
    }

    pub fn add_row(&mut self) -> usize {
        self.staged
            .new_rows
            .push(vec![None; self.rs.columns().len()]);
        self.view_rows() + self.staged.new_rows.len() - 1
    }

    pub fn toggle_delete(&mut self, r: usize) {
        match self.source_row(r) {
            Some(src) => {
                if !self.staged.deleted.remove(&src) {
                    self.staged.deleted.insert(src);
                }
            }
            None => {
                let i = r - self.view_rows();
                if i < self.staged.new_rows.len() {
                    self.staged.new_rows.remove(i);
                }
            }
        }
    }

    pub fn discard(&mut self) {
        self.staged = Staged::default();
        self.editing = None;
    }

    /// The statements Apply runs, in order: DELETE, UPDATE, INSERT. Values are
    /// bound parameters, never spliced into the SQL text.
    pub fn statements(&self) -> Vec<Statement> {
        let Some(t) = &self.target else {
            return Vec::new();
        };
        let key_where = |src: usize, params: &mut Vec<Param>| -> String {
            t.key
                .iter()
                .map(|&k| {
                    params.push(to_param(cell_string(self.rs.cell(src, k))));
                    format!("{} = ?", t.quoted_columns[k])
                })
                .collect::<Vec<_>>()
                .join(" AND ")
        };
        let mut out = Vec::new();
        for &src in &self.staged.deleted {
            let mut params = Vec::new();
            let w = key_where(src, &mut params);
            out.push(Statement {
                sql: format!("DELETE FROM {} WHERE {w}", t.quoted_table),
                params,
                expect_one: true,
                check: None,
            });
        }
        let mut by_row: BTreeMap<usize, Vec<(usize, Option<String>)>> = BTreeMap::new();
        for (&(src, c), v) in &self.staged.cells {
            if !self.staged.deleted.contains(&src) {
                by_row.entry(src).or_default().push((c, v.clone()));
            }
        }
        for (src, sets) in by_row {
            let mut params = Vec::new();
            let set = sets
                .into_iter()
                .map(|(c, v)| {
                    params.push(to_param(v));
                    format!("{} = ?", t.quoted_columns[c])
                })
                .collect::<Vec<_>>()
                .join(", ");
            let mut kp = Vec::new();
            let w = key_where(src, &mut kp);
            params.extend(kp.iter().cloned());
            let check = Some((
                format!("SELECT COUNT(*) FROM {} WHERE {w}", t.quoted_table),
                kp,
            ));
            out.push(Statement {
                sql: format!("UPDATE {} SET {set} WHERE {w}", t.quoted_table),
                params,
                expect_one: true,
                check,
            });
        }
        for row in &self.staged.new_rows {
            let mut cols = Vec::new();
            let mut params = Vec::new();
            for (c, v) in row.iter().enumerate() {
                if let Some(v) = v {
                    cols.push(t.quoted_columns[c].clone());
                    params.push(to_param(v.clone()));
                }
            }
            let sql = if cols.is_empty() {
                format!("INSERT INTO {} () VALUES ()", t.quoted_table)
            } else {
                let q = vec!["?"; cols.len()].join(", ");
                format!(
                    "INSERT INTO {} ({}) VALUES ({q})",
                    t.quoted_table,
                    cols.join(", ")
                )
            };
            out.push(Statement {
                sql,
                params,
                expect_one: true,
                check: None,
            });
        }
        out
    }

    // ---- copy --------------------------------------------------------------

    pub fn rows_count_all(&self) -> usize {
        self.view_rows() + self.staged.new_rows.len()
    }

    fn tsv_field(v: Option<String>) -> String {
        v.unwrap_or_default()
            .replace(['\t', '\n'], " ")
            .replace('\r', "")
    }

    pub fn copy_cell(&self, r: usize, c: usize) -> String {
        self.value(r, c).unwrap_or_default()
    }

    pub fn copy_rows_tsv(&self, rows: &[usize], cols: std::ops::Range<usize>) -> String {
        rows.iter()
            .map(|&r| {
                cols.clone()
                    .map(|c| Self::tsv_field(self.value(r, c)))
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .collect::<Vec<_>>()
            .join("\r\n")
    }

    pub fn copy_rows_csv(&self, rows: &[usize]) -> String {
        let n = self.rs.columns().len();
        let head = self
            .rs
            .columns()
            .iter()
            .map(|c| csv_field(Some(c.name.clone())))
            .collect::<Vec<_>>()
            .join(",");
        let mut out = vec![head];
        for &r in rows {
            out.push(
                (0..n)
                    .map(|c| csv_field(self.value(r, c)))
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        out.join("\r\n")
    }

    pub fn copy_rows_insert(&self, rows: &[usize]) -> String {
        let cols = self
            .rs
            .columns()
            .iter()
            .map(|c| format!("`{}`", c.name.replace('`', "``")))
            .collect::<Vec<_>>()
            .join(", ");
        rows.iter()
            .map(|&r| {
                let vals = (0..self.rs.columns().len())
                    .map(|c| match self.source_row(r).map(|s| self.rs.cell(s, c)) {
                        Some(Cell::Bytes(b))
                            if !self
                                .staged
                                .cells
                                .contains_key(&(self.source_row(r).unwrap(), c)) =>
                        {
                            hex(b, usize::MAX)
                        }
                        _ => match self.value(r, c) {
                            None => "NULL".into(),
                            Some(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''")),
                        },
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "INSERT INTO {} ({cols}) VALUES ({vals});",
                    self.insert_table
                )
            })
            .collect::<Vec<_>>()
            .join("\r\n")
    }

    /// Full text of a cell for the value viewer (binary as complete hex).
    pub fn full_text(&self, r: usize, c: usize) -> Option<String> {
        if let Some(src) = self.source_row(r)
            && !self.staged.cells.contains_key(&(src, c))
            && let Cell::Bytes(b) = self.rs.cell(src, c)
        {
            return Some(hex(b, usize::MAX));
        }
        self.value(r, c)
    }

    // ---- in-place editing --------------------------------------------------

    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    pub fn start_edit(
        &mut self,
        r: usize,
        c: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        if !self.editable()
            || r >= self.rows_count_all()
            || c >= self.rs.columns().len()
            || self.is_binary(r, c)
        {
            return;
        }
        if self
            .source_row(r)
            .is_some_and(|s| self.staged.deleted.contains(&s))
        {
            return;
        }
        let value = self.value(r, c).unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
        let sub = cx.subscribe_in(
            &input,
            window,
            |table, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } => {
                    table.delegate_mut().commit_edit(cx);
                    table.focus_handle(cx).focus(window, cx);
                }
                InputEvent::Blur => table.delegate_mut().commit_edit(cx),
                _ => {}
            },
        );
        input.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        self.editing = Some((r, c, input, sub));
        cx.notify();
    }

    pub fn commit_edit(&mut self, cx: &mut Context<TableState<Self>>) {
        if let Some((r, c, input, _)) = self.editing.take() {
            let v = input.read(cx).value().to_string();
            // Typing into a NULL cell and leaving it empty keeps NULL.
            let unchanged_null = v.is_empty() && self.value(r, c).is_none();
            if !unchanged_null {
                self.stage(r, c, Some(v));
            }
            cx.emit(TableEvent::ClearSelection);
            cx.notify();
        }
    }

    pub fn cancel_edit(&mut self, cx: &mut Context<TableState<Self>>) -> bool {
        let was = self.editing.take().is_some();
        if was {
            cx.notify();
        }
        was
    }
}

fn to_param(v: Option<String>) -> Param {
    match v {
        None => Param::Null,
        Some(s) => Param::Text(s),
    }
}

fn csv_field(v: Option<String>) -> String {
    match v {
        None => String::new(),
        Some(s) if s.contains([',', '"', '\n', '\r']) => format!("\"{}\"", s.replace('"', "\"\"")),
        Some(s) => s,
    }
}

pub fn cell_string(cell: Cell<'_>) -> Option<String> {
    match cell {
        Cell::Null => None,
        Cell::Text(s) => Some(s.to_string()),
        Cell::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
    }
}

pub fn hex(b: &[u8], max: usize) -> String {
    locus_core::hexfmt::prefixed(b, max)
}

/// The first `keep` rows of `batch` as a new batch (row limit).
pub fn truncate_batch(batch: &Batch, keep: usize) -> Batch {
    let mut b = BatchBuilder::new(batch.columns());
    for r in 0..keep.min(batch.rows()) {
        for c in 0..batch.columns() {
            b.push(match batch.cell(r, c) {
                Cell::Null => None,
                Cell::Text(s) => Some(s.as_bytes()),
                Cell::Bytes(x) => Some(x),
            });
        }
        b.end_row();
    }
    b.finish()
}

fn width_for(chars: usize) -> f32 {
    (chars as f32 * 8.0 + 20.0).clamp(64.0, 360.0)
}

pub enum Display {
    Null,
    Text(SharedString),
    /// Hex prefix and byte length.
    Binary(SharedString, usize),
}

pub fn cell_display(cell: Cell<'_>) -> Display {
    match cell {
        Cell::Null => Display::Null,
        Cell::Text(s) => Display::Text(short(s)),
        Cell::Bytes(b) => Display::Binary(hex(b, 16).into(), b.len()),
    }
}

fn short(s: &str) -> SharedString {
    let line = s.split(['\n', '\r']).next().unwrap_or("");
    let mut out = match line.char_indices().nth(MAX_CELL_CHARS) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_string(),
    };
    if line.len() < s.len() && !out.ends_with('…') {
        out.push('…');
    }
    out.into()
}

fn null_badge(cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .px_1()
        .rounded_sm()
        .bg(theme.muted)
        .text_xs()
        .italic()
        .text_color(theme.muted_foreground)
        .child(t!("grid.null").to_string())
        .into_any_element()
}

impl TableDelegate for ResultGrid {
    fn columns_count(&self, _: &App) -> usize {
        self.rs.columns().len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows_count_all()
    }

    fn column(&self, i: usize, _: &App) -> Column {
        let name = &self.rs.columns()[i].name;
        let mut col = Column::new(
            SharedString::from(format!("c{i}")),
            SharedString::from(name.clone()),
        )
        .width(px(self.widths.get(i).copied().unwrap_or(120.)))
        .resizable(true);
        if self.sortable {
            col = col.sortable();
            if let Some((c, s)) = self.sort
                && c == i
            {
                col = col.sort(s);
            }
        }
        col
    }

    fn perform_sort(
        &mut self,
        col: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        if !self.sortable {
            return;
        }
        self.editing = None;
        let n = self.rs.rows();
        let rs = &self.rs;
        let key = |r: &u32| match rs.cell(*r as usize, col) {
            Cell::Null => None,
            Cell::Text(s) => Some(s.as_bytes()),
            Cell::Bytes(b) => Some(b),
        };
        let mut order: Vec<u32> = (0..n as u32).collect();
        match sort {
            ColumnSort::Ascending => order.sort_by(|a, b| key(a).cmp(&key(b))),
            ColumnSort::Descending => order.sort_by(|a, b| key(b).cmp(&key(a))),
            _ => {
                self.order = None;
                self.sort = None;
                self.restart_view();
                return;
            }
        }
        self.sort = Some((col, sort));
        self.order = Some(order);
        self.restart_view();
    }

    /// Header: column name and, muted, its server type (`id INT`).
    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let menu = self.header_menu(col_ix, cx);
        let theme = cx.theme();
        let (name, ty) = match self.rs.columns().get(col_ix) {
            Some(c) => (c.name.clone(), c.type_name.to_uppercase()),
            None => (String::new(), String::new()),
        };
        h_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .items_baseline()
            .group(SharedString::from(format!("th{col_ix}")))
            .child(
                div()
                    .flex_shrink_0()
                    .max_w(relative(0.7))
                    .truncate()
                    .child(name),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(theme.muted_foreground.opacity(0.8))
                    .child(ty),
            )
            .children(menu)
    }

    fn render_tr(
        &mut self,
        r: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        let row = div().id(("row", r));
        match self.source_row(r) {
            None => row.bg(theme.success.opacity(0.14)),
            Some(src) if self.staged.deleted.contains(&src) => {
                row.bg(theme.danger.opacity(0.14)).line_through()
            }
            _ => row,
        }
    }

    fn render_td(
        &mut self,
        r: usize,
        c: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        if r >= self.rows_count_all() {
            return div().into_any_element();
        }
        if let Some((er, ec, input, _)) = &self.editing
            && *er == r
            && *ec == c
        {
            // Context `CellEdit` (for user keymaps); Esc is handled by the shell (`esc_step`).
            return div()
                .key_context("CellEdit")
                .size_full()
                .child(
                    Input::new(input)
                        .bordered(false)
                        .focus_bordered(false)
                        .appearance(false),
                )
                .into_any_element();
        }
        let staged = match self.source_row(r) {
            Some(src) => self.staged.cells.get(&(src, c)).cloned(),
            None => self
                .staged
                .new_rows
                .get(r - self.view_rows())
                .and_then(|row| row[c].clone()),
        };
        let theme = cx.theme().clone();
        let content = match &staged {
            Some(None) => null_badge(cx),
            Some(Some(s)) => short(s).into_any_element(),
            None => match self.source_row(r) {
                None => div()
                    .text_color(theme.muted_foreground)
                    .child(t!("grid.default").to_string())
                    .into_any_element(),
                Some(src) => match cell_display(self.rs.cell(src, c)) {
                    Display::Null => null_badge(cx),
                    Display::Text(s) => s.into_any_element(),
                    Display::Binary(h, n) => h_flex()
                        .gap_1()
                        .child(h)
                        .child(div().text_xs().text_color(theme.muted_foreground).child(
                            t!("grid.bytes", n = locus_core::fmt::count(n as u64)).to_string(),
                        ))
                        .into_any_element(),
                },
            },
        };
        let edited = staged.is_some() && self.source_row(r).is_some();
        let in_range = self.in_range(r, c);
        let tk = crate::shell::theme::tokens(cx);
        let active = self.range.is_some_and(|(_, e)| e == (r, c));
        // Range selection: press starts it (Shift extends), drag moves its end.
        let link = self.fk_link_cell(r, c);
        div()
            .id(("cell", r * 4096 + c))
            .group("fk-cell")
            .relative()
            .size_full()
            .when(edited, |d| d.bg(theme.warning.opacity(0.22)))
            .when(in_range, |d| d.bg(tk.sel))
            // One cell: the table draws its own active-cell frame; ours only marks a range end.
            .when(active && self.range.is_some_and(|(a, e)| a != e), |d| {
                d.border_1().border_color(tk.sel_ring)
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |t, ev: &MouseDownEvent, _, cx| {
                    let g = t.delegate_mut();
                    if ev.modifiers.shift
                        && let Some((a, _)) = g.range
                    {
                        g.range = Some((a, (r, c)));
                    } else {
                        g.range = Some(((r, c), (r, c)));
                    }
                    g.dragging = true;
                    // Ctrl+click on a foreign-key cell opens the referenced row.
                    if ev.modifiers.control
                        && !ev.modifiers.shift
                        && let Some(n) = g.nav_out(r, c).into_iter().next()
                    {
                        g.up(GridUp::Open(n.sql), cx);
                    }
                    g.range_changed(cx);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |t, ev: &MouseMoveEvent, _, cx| {
                let g = t.delegate_mut();
                if g.dragging && ev.pressed_button == Some(MouseButton::Left) {
                    if let Some((a, e)) = g.range
                        && e != (r, c)
                    {
                        g.range = Some((a, (r, c)));
                        g.range_changed(cx);
                        cx.notify();
                    }
                } else if g.dragging {
                    g.dragging = false;
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|t, _, _, _| t.delegate_mut().dragging = false),
            )
            .child(content)
            .when_some(link, |d, sql| {
                let tip = t!("fk.open_hint").to_string();
                d.child(
                    div()
                        .id(("fk-link", r * 4096 + c))
                        .absolute()
                        .right(px(2.))
                        .top_0()
                        .h_full()
                        .flex()
                        .items_center()
                        .px(px(3.))
                        .rounded(px(3.))
                        .bg(tk.sel)
                        .text_color(theme.primary)
                        .cursor_pointer()
                        .invisible()
                        .group_hover("fk-cell", |s| s.visible())
                        .tooltip(move |w, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |t, _, _, cx| {
                                cx.stop_propagation();
                                t.delegate().up(GridUp::Open(sql.clone()), cx);
                            }),
                        )
                        .child(crate::shell::icons::icon("link").size(px(12.))),
                )
            })
            .into_any_element()
    }

    fn context_menu(
        &mut self,
        r: usize,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        grid_menu(
            cx.entity().downgrade(),
            r,
            self.right_col,
            self.editable(),
            self.nav_items(r, self.right_col),
            menu,
        )
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(t!("grid.no_rows").to_string())
    }

    fn cell_text(&self, r: usize, c: usize, _: &App) -> String {
        self.value(r, c).unwrap_or_default()
    }
}

/// A read-only dialog with the whole value (Text / JSON / Hex / Image views).
pub fn open_viewer(g: &ResultGrid, r: usize, c: usize, window: &mut Window, cx: &mut App) {
    let Some(col) = g.rs.columns().get(c) else {
        return;
    };
    let title = t!("viewer.title", column = col.name.clone()).into_owned();
    let bytes = match g.source_row(r).map(|src| (src, g.rs.cell(src, c))) {
        Some((src, Cell::Bytes(b))) if !g.staged.cells.contains_key(&(src, c)) => Some(b.to_vec()),
        _ => None,
    };
    let (text, is_null) = match (&bytes, g.value(r, c)) {
        (Some(b), _) => (String::from_utf8_lossy(b).into_owned(), false),
        (None, Some(s)) => (s, false),
        (None, None) => (String::new(), true),
    };
    crate::viewer::open(
        crate::viewer::ViewerInput {
            title,
            is_null,
            bytes,
            text,
            type_name: col.type_name.clone(),
        },
        window,
        cx,
    );
}

impl ResultGrid {
    /// Normalised range: rows and columns, inclusive.
    pub fn range_bounds(
        &self,
    ) -> Option<(
        std::ops::RangeInclusive<usize>,
        std::ops::RangeInclusive<usize>,
    )> {
        let ((r0, c0), (r1, c1)) = self.range?;
        Some((r0.min(r1)..=r0.max(r1), c0.min(c1)..=c0.max(c1)))
    }

    fn in_range(&self, r: usize, c: usize) -> bool {
        match self.range_bounds() {
            // A single cell is the table's own selection; only ranges are tinted.
            Some((rs, cs)) => {
                (rs.start() != rs.end() || cs.start() != cs.end())
                    && rs.contains(&r)
                    && cs.contains(&c)
            }
            None => false,
        }
    }

    /// TSV of the range selection, if it covers more than one cell.
    pub fn copy_range(&self) -> Option<String> {
        let (rs, cs) = self.range_bounds()?;
        if rs.start() == rs.end() && cs.start() == cs.end() {
            return None;
        }
        let rows: Vec<usize> = rs.collect();
        Some(self.copy_rows_tsv(&rows, *cs.start()..*cs.end() + 1))
    }

    /// Shift+arrow: moves the range end, starting from `from` when there is none.
    pub fn extend_range(&mut self, from: (usize, usize), dr: isize, dc: isize) -> (usize, usize) {
        let (a, e) = self.range.unwrap_or((from, from));
        let rows = self.rows_count_all().max(1) as isize;
        let cols = self.rs.columns().len().max(1) as isize;
        let end = (
            (e.0 as isize + dr).clamp(0, rows - 1) as usize,
            (e.1 as isize + dc).clamp(0, cols - 1) as usize,
        );
        self.range = Some((a, end));
        end
    }
}

/// The grid's context menu for display row `r`, column `col`.
pub fn grid_menu(
    me: WeakEntity<TableState<ResultGrid>>,
    r: usize,
    col: usize,
    editable: bool,
    nav: Vec<NavItem>,
    menu: PopupMenu,
) -> PopupMenu {
    use crate::shell::scale::*;
    // Label on the left, the shortcut (if any) right-aligned in `tx4`.
    let item = |label: String,
                kbd: Option<&'static str>,
                f: fn(
        &mut ResultGrid,
        usize,
        usize,
        &mut Window,
        &mut Context<TableState<ResultGrid>>,
    )| {
        let me = me.clone();
        PopupMenuItem::element(move |_, cx| {
            let t = crate::shell::theme::tokens(cx);
            h_flex()
                .w_full()
                .min_w(px(220.))
                .py(px(S2))
                .gap(px(S6))
                .justify_between()
                .items_center()
                .text_size(px(TX_SM))
                .child(div().child(label.clone()))
                .children(kbd.map(|k| div().text_size(px(TX_MONO)).text_color(t.tx4).child(k)))
        })
        .on_click(move |_, window, cx| {
            let _ = me.update(cx, |t, cx| f(t.delegate_mut(), r, col, window, cx));
        })
    };
    let mut menu = menu
        .item(item(
            t!("grid.copy_selection").into_owned(),
            Some("Ctrl+C"),
            |g, r, c, _, cx| {
                let text = g.copy_range().unwrap_or_else(|| g.copy_cell(r, c));
                cx.write_to_clipboard(ClipboardItem::new_string(text))
            },
        ))
        .item(item(
            t!("grid.copy_cell").into_owned(),
            None,
            |g, r, c, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(g.copy_cell(r, c))),
        ))
        .item(item(
            t!("grid.copy_row").into_owned(),
            None,
            |g, r, _, _, cx| {
                let n = g.rs.columns().len();
                cx.write_to_clipboard(ClipboardItem::new_string(g.copy_rows_tsv(&[r], 0..n)))
            },
        ))
        .item(item(
            t!("grid.copy_csv").into_owned(),
            None,
            |g, r, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(g.copy_rows_csv(&[r])))
            },
        ))
        .item(item(
            t!("grid.copy_insert").into_owned(),
            None,
            |g, r, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(g.copy_rows_insert(&[r])))
            },
        ))
        .separator()
        .item(item(
            t!("grid.copy_column_name").into_owned(),
            Some("Ctrl+Shift+C"),
            |g, _, c, _, cx| {
                if let Some(col) = g.rs.columns().get(c) {
                    cx.write_to_clipboard(ClipboardItem::new_string(col.name.clone()))
                }
            },
        ))
        .item(item(
            t!("grid.copy_column_names").into_owned(),
            None,
            |g, _, _, _, cx| {
                let names: Vec<String> = g.rs.columns().iter().map(|c| c.name.clone()).collect();
                cx.write_to_clipboard(ClipboardItem::new_string(names.join(", ")))
            },
        ))
        .separator()
        .item(item(
            t!("grid.view_value").into_owned(),
            None,
            |g, r, c, window, cx| open_viewer(g, r, c, window, cx),
        ));
    if !nav.is_empty() {
        menu = menu.separator();
        for n in nav {
            let me = me.clone();
            let sql = n.sql;
            menu = menu.item(PopupMenuItem::new(n.label).on_click(move |_, _, cx| {
                let _ = me.update(cx, |t, cx| t.delegate().up(GridUp::Open(sql.clone()), cx));
            }));
        }
    }
    if editable {
        menu = menu
            .separator()
            .item(item(
                t!("grid.edit_cell").into_owned(),
                Some("F2"),
                |g, r, c, window, cx| g.start_edit(r, c, window, cx),
            ))
            .item(item(
                t!("grid.set_null").into_owned(),
                None,
                |g, r, c, _, cx| {
                    g.stage(r, c, None);
                    cx.notify();
                },
            ))
            .item(item(
                t!("grid.add_row").into_owned(),
                None,
                |g, _, _, _, cx| {
                    let r = g.add_row();
                    cx.notify();
                    let _ = r;
                },
            ))
            .separator()
            .item({
                let me = me.clone();
                crate::shell::menu::danger(
                    t!("grid.delete_row").into_owned(),
                    None,
                    move |_, cx| {
                        let _ = me.update(cx, |t, cx| {
                            t.delegate_mut().toggle_delete(r);
                            cx.notify();
                        });
                    },
                )
            });
    }
    menu
}

impl ResultGrid {
    /// Fetched rows on the page in front.
    pub fn view_rows(&self) -> usize {
        let n = self.matched();
        match self.page {
            Some((start, len)) => n.saturating_sub(start).min(len),
            None => n,
        }
    }

    /// Rows the sort / filter lets through (all fetched rows without a filter).
    pub fn matched(&self) -> usize {
        self.view
            .as_ref()
            .map(|v| v.len())
            .unwrap_or(self.rs.rows())
    }

    pub fn filter_active(&self) -> bool {
        self.filter.is_some()
    }

    /// Rows read from the server (before the filter).
    pub fn fetched(&self) -> usize {
        self.rs.rows()
    }

    /// Sort or filter changed: rebuild the display order from the start.
    fn restart_view(&mut self) {
        self.range = None;
        self.sel_job = None;
        self.sel_task = None;
        self.sel_summary = None;
        self.editing = None;
        self.scan_pos = 0;
        self.view = if self.filter.is_some() {
            Some(Vec::new())
        } else {
            self.order.clone()
        };
    }

    /// Sets the filter text. Returns false when it is empty (filter off).
    /// The scan itself runs in `filter_step` slices.
    pub fn set_filter(&mut self, text: &str) -> bool {
        self.filter_text = text.to_string();
        let names: Vec<&str> = self.rs.columns().iter().map(|c| c.name.as_str()).collect();
        self.filter = match locus_core::rowfilter::parse(text, &names) {
            locus_core::rowfilter::Parsed::Empty => None,
            locus_core::rowfilter::Parsed::Filter(f) => Some(f),
        };
        self.restart_view();
        self.filter.is_some()
    }

    /// Reads candidate rows for about `budget`. True when every fetched row
    /// has been looked at (rows that arrive later need another call).
    pub fn filter_step(&mut self, budget: std::time::Duration) -> bool {
        let Some(f) = self.filter.take() else {
            return true;
        };
        let t0 = std::time::Instant::now();
        let cols = self.rs.columns().len();
        let limit = match &self.order {
            Some(o) => o.len().min(self.rs.rows()),
            None => self.rs.rows(),
        };
        let mut out = self.view.take().unwrap_or_default();
        let mut pos = self.scan_pos;
        while pos < limit {
            let src = match &self.order {
                Some(o) => o[pos] as usize,
                None => pos,
            };
            let rs = &self.rs;
            let hit = f.matches(cols, |c| match rs.cell(src, c) {
                Cell::Null => None,
                Cell::Text(s) => Some(s),
                Cell::Bytes(b) => std::str::from_utf8(b).ok(),
            });
            if hit {
                out.push(src as u32);
            }
            pos += 1;
            if pos.is_multiple_of(128) && t0.elapsed() >= budget {
                break;
            }
        }
        self.scan_pos = pos;
        self.view = Some(out);
        self.filter = Some(f);
        pos >= limit
    }

    /// A stored value as a copy value (staged edits win).
    fn copy_value(&self, src: usize, c: usize) -> locus_core::copyfmt::Value {
        use locus_core::copyfmt::Value;
        if let Some(v) = self.staged.cells.get(&(src, c)) {
            return match v {
                None => Value::Null,
                Some(s) => Value::Text(s.clone()),
            };
        }
        match self.rs.cell(src, c) {
            Cell::Null => Value::Null,
            Cell::Text(s) => Value::Text(s.to_string()),
            Cell::Bytes(b) => Value::Hex(hex(b, usize::MAX).trim_start_matches("0x").to_string()),
        }
    }

    /// Rows for "copy as": `rows` are display rows of the page in front when
    /// given, else every shown row (all pages, filter and sort applied).
    /// At most `cap` rows; the flag says some were left out.
    pub fn copy_table(
        &self,
        rows: Option<&[usize]>,
        cols: std::ops::Range<usize>,
        cap: usize,
    ) -> (locus_core::copyfmt::Table, bool) {
        let metas = self.rs.columns();
        let cols: Vec<usize> = cols.filter(|c| *c < metas.len()).collect();
        let srcs: Vec<usize> = match rows {
            Some(r) => r.iter().filter_map(|r| self.source_row(*r)).collect(),
            None => (0..self.matched())
                .map(|i| self.view.as_ref().map(|v| v[i] as usize).unwrap_or(i))
                .collect(),
        };
        let cut = srcs.len() > cap;
        let table = locus_core::copyfmt::Table {
            columns: cols.iter().map(|c| metas[*c].name.clone()).collect(),
            numeric: cols
                .iter()
                .map(|c| locus_core::copyfmt::is_numeric_type(&metas[*c].type_name))
                .collect(),
            rows: srcs
                .iter()
                .take(cap)
                .map(|s| cols.iter().map(|c| self.copy_value(*s, *c)).collect())
                .collect(),
        };
        (table, cut)
    }

    /// Name of the table the rows came from (first column with an origin), for INSERT.
    pub fn origin_table(&self) -> Option<String> {
        self.rs
            .columns()
            .iter()
            .find_map(|c| c.origin.as_ref().map(|o| o.table.clone()))
    }
}

impl ResultGrid {
    /// Source rows in display order (filter and sort applied); `None` = all in fetch order.
    pub fn export_view(&self) -> Option<&[u32]> {
        self.view.as_deref()
    }

    /// Text of source row `src` over every fetched row (not just the page), for export.
    pub fn export_text(&self, src: usize, c: usize) -> Option<String> {
        if let Some(v) = self.staged.cells.get(&(src, c)) {
            return v.clone();
        }
        match self.rs.cell(src, c) {
            Cell::Bytes(b) => Some(hex(b, usize::MAX)),
            other => cell_string(other),
        }
    }
}

impl ResultGrid {
    /// A double-click on a read-only cell opens the viewer only for values that do not fit
    /// a cell (long text, several lines, binary); a short value has nothing more to show.
    pub fn wants_viewer(&self, r: usize, c: usize) -> bool {
        if self.is_binary(r, c) {
            return true;
        }
        self.full_text(r, c)
            .is_some_and(|s| s.chars().count() > 80 || s.contains('\n'))
    }
}

// ---- foreign-key navigation ------------------------------------------------

/// What a result grid needs to follow foreign keys: the one table its
/// columns come from, and that schema's keys (filled in when the catalog answers).
pub struct FkCtx {
    pub schema: String,
    pub table: String,
    /// Table column name of each result column.
    pub cols: Vec<String>,
    pub fks: Vec<db_core::ForeignKey>,
    /// Result columns that are part of a key of this table (cheap test while drawing).
    pub out_cols: Vec<bool>,
    pub driver: std::sync::Arc<locus_core::driver::AnyDriver>,
    pub dialect: locus_core::copyfmt::Dialect,
}

/// A grid entry in the cell context menu.
#[derive(Clone, Debug)]
pub struct NavItem {
    pub label: String,
    pub sql: String,
}

/// From the grid up to its query tab.
pub enum GridUp {
    /// Open this SELECT in a new tab on the same connection and run it.
    Open(String),
    /// The numbers of the range selection changed.
    Stats,
    /// A header menu entry (filter / order).
    Col(super::colmenu::ColAct),
}

/// Most "referencing rows" entries offered for one cell.
const MAX_REFERENCING: usize = 12;

impl ResultGrid {
    pub fn up(&self, m: GridUp, cx: &mut App) {
        if let Some(f) = &self.on_up {
            f(m, cx);
        }
    }

    pub fn set_fks(&mut self, fks: Vec<db_core::ForeignKey>) {
        if let Some(f) = &mut self.fk {
            f.out_cols = (0..f.cols.len())
                .map(|c| !locus_core::fknav::outgoing(&fks, &f.table, &f.cols, c).is_empty())
                .collect();
            f.fks = fks;
        }
    }

    /// Key columns of the other side for the cells of display row `r`; `None` when
    /// the row is a new one or a key cell is NULL (nothing to look up).
    fn key_parts(
        &self,
        r: usize,
        link: &locus_core::fknav::Link<'_>,
        names: &[String],
    ) -> Option<Vec<locus_core::fknav::KeyPart>> {
        let src = self.source_row(r)?;
        let metas = self.rs.columns();
        link.indexes
            .iter()
            .zip(names)
            .map(|(idx, name)| {
                let value = self.copy_value(src, *idx);
                (value != locus_core::copyfmt::Value::Null).then(|| locus_core::fknav::KeyPart {
                    column: name.clone(),
                    value,
                    numeric: locus_core::copyfmt::is_numeric_type(&metas[*idx].type_name),
                })
            })
            .collect()
    }

    /// "Open referenced row" for cell (`r`, `c`): one entry per key the column is part of.
    pub fn nav_out(&self, r: usize, c: usize) -> Vec<NavItem> {
        let Some(f) = &self.fk else { return Vec::new() };
        if !f.out_cols.get(c).copied().unwrap_or(false) {
            return Vec::new();
        }
        let q = |s: &str| db_core::Driver::quote_identifier(&*f.driver, s);
        locus_core::fknav::outgoing(&f.fks, &f.table, &f.cols, c)
            .into_iter()
            .filter_map(|l| {
                let parts = self.key_parts(r, &l, &l.fk.ref_columns)?;
                Some(NavItem {
                    label: t!(
                        "fk.open_ref",
                        table = l.fk.ref_table.clone(),
                        cols = l.fk.ref_columns.join(", ")
                    )
                    .into_owned(),
                    sql: locus_core::fknav::select_where(
                        f.dialect,
                        &q,
                        &l.fk.ref_schema,
                        &l.fk.ref_table,
                        &parts,
                    ),
                })
            })
            .collect()
    }

    /// "Show referencing rows" for cell (`r`, `c`): one entry per table that references the key.
    pub fn nav_in(&self, r: usize, c: usize) -> Vec<NavItem> {
        let Some(f) = &self.fk else { return Vec::new() };
        let q = |s: &str| db_core::Driver::quote_identifier(&*f.driver, s);
        locus_core::fknav::incoming(&f.fks, &f.schema, &f.table, &f.cols, c)
            .into_iter()
            .take(MAX_REFERENCING)
            .filter_map(|l| {
                let parts = self.key_parts(r, &l, &l.fk.columns)?;
                Some(NavItem {
                    label: t!(
                        "fk.show_ref",
                        table = l.fk.table.clone(),
                        cols = l.fk.columns.join(", ")
                    )
                    .into_owned(),
                    sql: locus_core::fknav::select_where(
                        f.dialect,
                        &q,
                        &f.schema,
                        &l.fk.table,
                        &parts,
                    ),
                })
            })
            .collect()
    }

    pub fn nav_items(&self, r: usize, c: usize) -> Vec<NavItem> {
        let mut v = self.nav_out(r, c);
        v.extend(self.nav_in(r, c));
        v
    }

    /// The SELECT of the hover link on a foreign-key cell (`None` = no link).
    fn fk_link_cell(&self, r: usize, c: usize) -> Option<String> {
        if !self.fk.as_ref()?.out_cols.get(c).copied().unwrap_or(false) {
            return None;
        }
        self.nav_out(r, c).into_iter().next().map(|n| n.sql)
    }
}

// ---- selection statistics --------------------------------------------------

struct SelJob {
    r1: usize,
    c0: usize,
    c1: usize,
    next: usize,
    acc: locus_core::selstats::Stats,
}

/// Wait after the last change of the selection before counting (a drag moves it every frame).
const STATS_DELAY: std::time::Duration = std::time::Duration::from_millis(40);

impl ResultGrid {
    /// The range changed: forget the numbers and, for two or more cells, count in short
    /// slices on the UI thread (no copy of the data, the grid stays responsive).
    pub fn range_changed(&mut self, cx: &mut Context<TableState<Self>>) {
        let had = self.sel_summary.take().is_some();
        self.sel_job = None;
        self.sel_task = None;
        let multi = self
            .range_bounds()
            .filter(|(r, c)| r.start() != r.end() || c.start() != c.end());
        if let Some((rs, cs)) = multi {
            self.sel_job = Some(SelJob {
                r1: *rs.end(),
                c0: *cs.start(),
                c1: *cs.end(),
                next: *rs.start(),
                acc: Default::default(),
            });
            self.sel_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(STATS_DELAY).await;
                loop {
                    let Ok(done) = this.update(cx, |t, cx| {
                        let done = t
                            .delegate_mut()
                            .stats_step(std::time::Duration::from_millis(3));
                        if done {
                            t.delegate().up(GridUp::Stats, cx);
                        }
                        done
                    }) else {
                        break;
                    };
                    if done {
                        break;
                    }
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(1))
                        .await;
                }
            }));
        }
        if had {
            self.up(GridUp::Stats, cx);
        }
    }

    /// Counts for about `budget`; true when the whole range is done (and `sel_summary` set).
    fn stats_step(&mut self, budget: std::time::Duration) -> bool {
        let Some(mut job) = self.sel_job.take() else {
            return true;
        };
        let t0 = std::time::Instant::now();
        let mut n = 0usize;
        while job.next <= job.r1 {
            let r = job.next;
            for c in job.c0..=job.c1 {
                self.stat_cell(r, c, &mut job.acc);
            }
            job.next += 1;
            n += 1;
            if n.is_multiple_of(64) && t0.elapsed() >= budget {
                break;
            }
        }
        if job.next > job.r1 {
            self.sel_summary = Some(job.acc.summary());
            true
        } else {
            self.sel_job = Some(job);
            false
        }
    }

    fn stat_cell(&self, r: usize, c: usize, acc: &mut locus_core::selstats::Stats) {
        match self.source_row(r) {
            Some(src) => match self.staged.cells.get(&(src, c)) {
                Some(v) => acc.add(v.as_deref()),
                None => match self.rs.cell(src, c) {
                    Cell::Null => acc.add(None),
                    Cell::Text(s) => acc.add(Some(s)),
                    Cell::Bytes(_) => acc.add(Some("")),
                },
            },
            None => acc.add(self.value(r, c).as_deref()),
        }
    }
}
