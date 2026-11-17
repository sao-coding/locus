//! "Export the full query result": runs the statement on a driver session of
//! its own and writes the rows straight to the file, batch by batch. Nothing
//! is kept, so memory stays flat for any row count (the driver's stream is
//! bounded: a slow disk slows the server read down instead of filling RAM).
//!
//! Runs on a plain thread (`QueryHandle::blocking_next`). Cancel goes through
//! the driver's own cancel (KILL QUERY on MySQL / MariaDB).

use crate::export::{ColKind, Format, Row, Spec, Writer, col_kind};
use db_core::{Canceller, Cell, Driver, Outcome, QueryEvent, QueryOptions};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Counters the UI reads while the export runs.
#[derive(Default)]
pub struct Progress {
    pub rows: AtomicU64,
    /// Cell data received from the server, in bytes.
    pub bytes: AtomicU64,
}

/// Cancel switch shared between the UI and the export thread.
#[derive(Default)]
pub struct Cancel {
    flag: AtomicBool,
    canceller: Mutex<Option<Canceller>>,
}

impl Cancel {
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Some(c) = self.canceller.lock().as_ref() {
            c.cancel();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum End {
    Done(u64),
    /// The query was stopped; the partial file is removed.
    Cancelled,
}

fn hex(b: &[u8]) -> String {
    crate::hexfmt::prefixed(b, usize::MAX)
}

/// Runs `sql` on `driver` and writes the first result set to `path` in
/// `format`. `base` carries the options (table name, batch size …); its
/// columns and kinds are replaced by the result's.
pub fn export_query(
    driver: &dyn Driver,
    sql: String,
    format: Format,
    base: Spec,
    path: &std::path::Path,
    progress: &Progress,
    cancel: &Cancel,
) -> Result<End, String> {
    let mut handle = driver.query(
        sql,
        QueryOptions {
            batch_rows: 2000,
            ..Default::default()
        },
    );
    *cancel.canceller.lock() = Some(handle.canceller());
    if cancel.is_cancelled() {
        handle.cancel();
    }
    let mut writer: Option<Writer> = None;
    let mut sets = 0;
    let mut failure: Option<String> = None;
    let outcome = loop {
        let Some(ev) = handle.blocking_next() else {
            break Outcome::Failed(db_core::DbError::new(
                db_core::ErrorKind::Internal,
                "the query stream ended without a result",
            ));
        };
        match ev {
            QueryEvent::Started { .. } => {}
            QueryEvent::Columns(cols) => {
                sets += 1;
                if sets > 1 {
                    // Only the first result set is exported.
                    handle.cancel();
                    continue;
                }
                let mut spec = base.clone();
                spec.columns = cols.iter().map(|c| c.name.clone()).collect();
                spec.kinds = cols.iter().map(|c| col_kind(&c.type_name)).collect();
                match Writer::create_with(path, format, spec) {
                    Ok(w) => writer = Some(w),
                    Err(e) => {
                        failure = Some(e.to_string());
                        handle.cancel();
                    }
                }
            }
            QueryEvent::Batch(b) => {
                if sets != 1 || failure.is_some() {
                    continue;
                }
                let Some(w) = writer.as_mut() else { continue };
                progress
                    .bytes
                    .fetch_add(b.bytes() as u64, Ordering::Relaxed);
                for r in 0..b.rows() {
                    let row: Row = (0..b.columns())
                        .map(|c| match b.cell(r, c) {
                            Cell::Null => None,
                            Cell::Text(s) => Some(s.to_string()),
                            Cell::Bytes(x) => Some(hex(x)),
                        })
                        .collect();
                    if let Err(e) = w.row(&row) {
                        failure = Some(e.to_string());
                        handle.cancel();
                        break;
                    }
                    progress.rows.fetch_add(1, Ordering::Relaxed);
                }
                if cancel.is_cancelled() {
                    handle.cancel();
                }
            }
            QueryEvent::Affected { .. } => {
                failure = Some("the statement returned no rows".into());
            }
            QueryEvent::Finished(o) => break o,
        }
    };
    *cancel.canceller.lock() = None;
    let rows = progress.rows.load(Ordering::Relaxed);
    let discard = |writer: Option<Writer>| {
        drop(writer);
        let _ = std::fs::remove_file(path);
    };
    if let Some(msg) = failure {
        discard(writer);
        return Err(msg);
    }
    match outcome {
        Outcome::Done { .. } => match writer {
            Some(w) => w.finish().map(|_| End::Done(rows)).map_err(|e| {
                let _ = std::fs::remove_file(path);
                e.to_string()
            }),
            None => Err("the statement returned no result set".into()),
        },
        Outcome::Cancelled => {
            discard(writer);
            Ok(End::Cancelled)
        }
        Outcome::Failed(e) => {
            discard(writer);
            Err(e.message)
        }
    }
}

/// The kinds the dialog can show for a column list (used by tests and the grid path).
pub fn kinds_of(type_names: &[String]) -> Vec<ColKind> {
    type_names.iter().map(|t| col_kind(t)).collect()
}
