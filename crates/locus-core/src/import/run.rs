//! The import itself: batched multi-row INSERT with bound parameters on one
//! pinned session.
//!
//! * `TxScope::Single`: one transaction around everything; an error (Stop),
//!   Cancel or a failed COMMIT leaves the table as it was.
//! * `TxScope::PerBatch`: one transaction per batch; batches already
//!   committed stay (for very large files).
//!
//! Each batch runs behind a SAVEPOINT. When a batch fails it is rolled back
//! to the savepoint and its rows are inserted one by one, so the rows that
//! fail are known by line (skip mode) or the first of them stops the import.

use super::ColumnMap;
use super::convert::{ConvertOptions, convert};
use super::source::{self, SourceSpec};
use super::sqlgen::{self, NewColumn};
use crate::apply::exec;
use crate::driver::AnyDriver;
use crate::txn::{self, Engine};
use db_core::{Driver, Param};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnError {
    /// Stop at the first bad row.
    Stop,
    /// Skip bad rows and list them.
    Skip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxScope {
    Single,
    PerBatch,
}

#[derive(Clone, Debug)]
pub struct ImportOptions {
    pub convert: ConvertOptions,
    pub on_error: OnError,
    /// Empty the table first (inside the transaction in `Single` scope).
    pub clear_first: bool,
    pub batch_size: usize,
    pub scope: TxScope,
    /// Where rows that failed go (created on the first failure).
    pub error_log: Option<PathBuf>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            convert: ConvertOptions::default(),
            on_error: OnError::Stop,
            clear_first: false,
            batch_size: 1000,
            scope: TxScope::Single,
            error_log: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ImportJob {
    pub source: SourceSpec,
    pub engine: Engine,
    pub schema: Option<String>,
    pub table: String,
    /// `Some`: CREATE TABLE first with these columns (they are the targets of `maps`).
    pub create: Option<Vec<NewColumn>>,
    pub maps: Vec<ColumnMap>,
    pub options: ImportOptions,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub read: u64,
    pub inserted: u64,
    pub skipped: u64,
    pub fraction: f64,
    pub elapsed: Duration,
    pub rows_per_sec: f64,
    pub eta: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowError {
    pub line: u64,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub read: u64,
    /// Rows in the table because of this import when it ended.
    pub inserted: u64,
    pub skipped: u64,
    /// The first rows that failed (the log file has all of them).
    pub errors: Vec<RowError>,
    pub cancelled: bool,
    /// Everything was undone (Single scope: cancel or error).
    pub rolled_back: bool,
    /// A cancelled new-table import dropped the table it had created (still empty).
    pub table_dropped: bool,
    /// What stopped the import.
    pub fatal: Option<String>,
    pub error_log: Option<PathBuf>,
    pub elapsed: Duration,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.fatal.is_none() && !self.cancelled
    }
}

const KEEP_ERRORS: usize = 500;

struct Row {
    line: u64,
    raw: Vec<String>,
    vals: Vec<Option<String>>,
}

struct ErrorLog {
    path: Option<PathBuf>,
    writer: Option<csv::Writer<std::fs::File>>,
    kept: Vec<RowError>,
    count: u64,
    used: bool,
}

impl ErrorLog {
    fn push(&mut self, line: u64, message: String, raw: &[String]) {
        self.count += 1;
        if let Some(p) = &self.path {
            if self.writer.is_none() {
                self.writer = csv::WriterBuilder::new().flexible(true).from_path(p).ok();
                if let Some(w) = self.writer.as_mut() {
                    let _ = w.write_record(["line", "error"]);
                }
            }
            if let Some(w) = self.writer.as_mut() {
                self.used = true;
                let mut rec = vec![line.to_string(), message.clone()];
                rec.extend(raw.iter().cloned());
                let _ = w.write_record(&rec);
            }
        }
        if self.kept.len() < KEEP_ERRORS {
            self.kept.push(RowError { line, message });
        }
    }
}

fn param(v: &Option<String>) -> Param {
    match v {
        Some(s) => Param::Text(s.clone()),
        None => Param::Null,
    }
}

fn msg(e: &db_core::DbError) -> String {
    match e.code {
        Some(c) => format!("[{c}] {}", e.message),
        None => e.message.clone(),
    }
}

struct Ctx<'a> {
    s: &'a dyn Driver,
    engine: Engine,
    table_q: String,
    cols: Vec<(String, super::TargetKind)>,
    per_stmt: usize,
}

impl Ctx<'_> {
    async fn insert(&self, rows: &[Row]) -> Result<(), String> {
        let sql = sqlgen::insert_sql(self.engine, &self.table_q, &self.cols, rows.len());
        let params: Vec<Param> = rows.iter().flat_map(|r| r.vals.iter().map(param)).collect();
        exec(self.s, sql, params)
            .await
            .map(|_| ())
            .map_err(|e| msg(&e))
    }

    async fn control(&self, sql: &str) -> Result<(), String> {
        exec(self.s, sql.to_string(), Vec::new())
            .await
            .map(|_| ())
            .map_err(|e| msg(&e))
    }
}

/// Drops the table the import created when it holds no rows (checked again here, not
/// assumed). True when it was dropped.
async fn drop_if_empty(cx: &Ctx<'_>) -> bool {
    let rows = exec(cx.s, sqlgen::count_sql(&cx.table_q), Vec::new()).await;
    if !matches!(rows, Ok(0)) {
        return false;
    }
    cx.control(&sqlgen::drop_sql(&cx.table_q)).await.is_ok()
}

/// Runs the import. `cancel` is polled between batches. `progress` is called
/// about five times a second and once at the end.
pub async fn run(
    driver: Arc<AnyDriver>,
    job: ImportJob,
    cancel: Arc<AtomicBool>,
    mut progress: impl FnMut(Progress) + Send,
) -> Report {
    let start = Instant::now();
    let mut rep = Report::default();
    let mut log = ErrorLog {
        path: job.options.error_log.clone(),
        writer: None,
        kept: Vec::new(),
        count: 0,
        used: false,
    };
    let r = run_inner(
        &driver,
        &job,
        &cancel,
        &mut progress,
        &mut rep,
        &mut log,
        start,
    )
    .await;
    if let Err(e) = r {
        rep.fatal = Some(e);
    }
    if let Some(w) = log.writer.as_mut() {
        let _ = w.flush();
    }
    rep.skipped = log.count.saturating_sub(
        if rep.fatal.is_some() && job.options.on_error == OnError::Stop {
            1
        } else {
            0
        },
    );
    rep.errors = log.kept;
    rep.error_log = if log.used { log.path.clone() } else { None };
    rep.elapsed = start.elapsed();
    progress(snapshot(&rep, &job, 1.0, start));
    rep
}

fn snapshot(rep: &Report, _job: &ImportJob, fraction: f64, start: Instant) -> Progress {
    let el = start.elapsed();
    let secs = el.as_secs_f64().max(0.001);
    let eta = (fraction > 0.001 && fraction < 1.0)
        .then(|| Duration::from_secs_f64(secs * (1.0 - fraction) / fraction));
    Progress {
        read: rep.read,
        inserted: rep.inserted,
        skipped: rep.skipped,
        fraction,
        elapsed: el,
        rows_per_sec: rep.read as f64 / secs,
        eta,
    }
}

async fn run_inner(
    driver: &Arc<AnyDriver>,
    job: &ImportJob,
    cancel: &Arc<AtomicBool>,
    progress: &mut (impl FnMut(Progress) + Send),
    rep: &mut Report,
    log: &mut ErrorLog,
    start: Instant,
) -> Result<(), String> {
    let o = &job.options;
    if job.maps.is_empty() {
        return Err("no column is mapped".into());
    }
    let mut src = source::open(&job.source)?;
    let session = driver.session().await.map_err(|e| msg(&e))?;
    let cx = Ctx {
        s: &*session,
        engine: job.engine,
        table_q: sqlgen::qualified(job.engine, job.schema.as_deref(), &job.table),
        cols: job
            .maps
            .iter()
            .map(|m| (m.target.clone(), m.kind))
            .collect(),
        per_stmt: sqlgen::rows_per_statement(job.engine, job.maps.len(), o.batch_size),
    };
    let single = o.scope == TxScope::Single;
    if let Some(cols) = &job.create {
        cx.control(&sqlgen::create_table_sql(
            job.engine,
            job.schema.as_deref(),
            &job.table,
            cols,
        ))
        .await?;
    }
    if o.clear_first && !single {
        cx.control(&sqlgen::clear_sql(job.engine, &cx.table_q, false))
            .await?;
    }
    if single {
        cx.control(txn::BEGIN).await?;
        if o.clear_first
            && let Err(e) = cx
                .control(&sqlgen::clear_sql(job.engine, &cx.table_q, true))
                .await
        {
            let _ = cx.control(txn::ROLLBACK).await;
            rep.rolled_back = true;
            return Err(e);
        }
    }
    let batch_size = o.batch_size.max(1);
    let mut raw = Vec::new();
    let mut last_report = Instant::now();
    let mut fatal: Option<String> = None;
    'outer: loop {
        if cancel.load(Ordering::Relaxed) {
            rep.cancelled = true;
            break;
        }
        // Read and convert one batch.
        let mut batch: Vec<Row> = Vec::with_capacity(batch_size);
        let mut eof = false;
        while batch.len() < batch_size {
            match src.rows.next_row(&mut raw) {
                Err(e) => {
                    fatal = Some(e);
                    break 'outer;
                }
                Ok(None) => {
                    eof = true;
                    break;
                }
                Ok(Some(line)) => {
                    rep.read += 1;
                    match convert_row(&raw, &job.maps, &o.convert) {
                        Ok(vals) => batch.push(Row {
                            line,
                            raw: raw.clone(),
                            vals,
                        }),
                        Err(m) => {
                            log.push(line, m.clone(), &raw);
                            if o.on_error == OnError::Stop {
                                fatal = Some(format!("line {line}: {m}"));
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }
        if !batch.is_empty() {
            match flush(&cx, &batch, single, o.on_error, log, &mut rep.inserted).await {
                Ok(()) => {}
                Err(e) => {
                    fatal = Some(e);
                    break;
                }
            }
        }
        if last_report.elapsed() >= Duration::from_millis(200) {
            rep.skipped = log.count;
            progress(snapshot(rep, job, src.rows.fraction(), start));
            last_report = Instant::now();
        }
        if eof {
            break;
        }
    }
    if single {
        if fatal.is_some() || rep.cancelled {
            let _ = cx.control(txn::ROLLBACK).await;
            rep.rolled_back = true;
            rep.inserted = 0;
        } else if let Err(e) = cx.control(txn::COMMIT).await {
            let _ = cx.control(txn::ROLLBACK).await;
            rep.rolled_back = true;
            rep.inserted = 0;
            fatal = Some(e);
        }
    }
    // A cancelled import into a table it created leaves no empty table behind. Only a
    // table that is still empty (counted again just now) is dropped.
    if rep.cancelled && job.create.is_some() {
        rep.table_dropped = drop_if_empty(&cx).await;
    }
    match fatal {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn convert_row(
    raw: &[String],
    maps: &[ColumnMap],
    o: &ConvertOptions,
) -> Result<Vec<Option<String>>, String> {
    maps.iter()
        .enumerate()
        .map(|(i, m)| {
            let cell = raw.get(m.source).map(String::as_str).unwrap_or("");
            convert(cell, m.kind, o).map_err(|e| format!("column {} ({}): {e}", i + 1, m.target))
        })
        .collect()
}

/// Inserts one batch. In `PerBatch` scope the batch has its own transaction.
/// Rows that fail are logged (skip mode) or end the import (stop mode).
async fn flush(
    cx: &Ctx<'_>,
    batch: &[Row],
    single: bool,
    on_error: OnError,
    log: &mut ErrorLog,
    inserted: &mut u64,
) -> Result<(), String> {
    if !single {
        cx.control(txn::BEGIN).await?;
    }
    let mut added = 0u64;
    let res = flush_rows(cx, batch, on_error, log, &mut added).await;
    if !single {
        match &res {
            Ok(()) => {
                if let Err(e) = cx.control(txn::COMMIT).await {
                    let _ = cx.control(txn::ROLLBACK).await;
                    return Err(e);
                }
                *inserted += added;
            }
            Err(_) => {
                let _ = cx.control(txn::ROLLBACK).await;
            }
        }
    } else {
        *inserted += added;
    }
    res
}

async fn flush_rows(
    cx: &Ctx<'_>,
    batch: &[Row],
    on_error: OnError,
    log: &mut ErrorLog,
    added: &mut u64,
) -> Result<(), String> {
    cx.control("SAVEPOINT wb_imp").await?;
    let mut failed = false;
    for chunk in batch.chunks(cx.per_stmt) {
        if cx.insert(chunk).await.is_err() {
            failed = true;
            break;
        }
    }
    if !failed {
        *added += batch.len() as u64;
        return Ok(());
    }
    // Back to the start of the batch, then row by row to find the bad ones.
    cx.control("ROLLBACK TO SAVEPOINT wb_imp").await?;
    for r in batch {
        cx.control("SAVEPOINT wb_row").await?;
        match cx.insert(std::slice::from_ref(r)).await {
            Ok(()) => *added += 1,
            Err(m) => {
                cx.control("ROLLBACK TO SAVEPOINT wb_row").await?;
                log.push(r.line, m.clone(), &r.raw);
                if on_error == OnError::Stop {
                    return Err(format!("line {}: {m}", r.line));
                }
            }
        }
    }
    Ok(())
}
