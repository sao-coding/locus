//! Runs statements on one pinned session and turns the event stream into
//! compact text. Dropping a future here drops its `QueryHandle`, which
//! cancels the query on the server (KILL / interrupt).

use crate::format::{self, Table};
use db_core::{Driver, Outcome, QueryEvent, QueryOptions};
use std::time::Duration;

/// Server-side limit of one statement.
pub const STATEMENT_TIMEOUT: Duration = Duration::from_secs(30);

/// What one statement gave.
#[derive(Debug)]
pub enum StmtOut {
    Tables(Vec<Table>),
    Affected(u64),
    Failed(String),
}

impl StmtOut {
    pub fn render(&self) -> String {
        match self {
            StmtOut::Tables(t) => t.iter().map(Table::render).collect::<Vec<_>>().join("\n\n"),
            StmtOut::Affected(n) => format::affected(*n),
            StmtOut::Failed(m) => format!("error: {m}"),
        }
    }

    /// Rows read or changed (for History).
    pub fn rows(&self) -> u64 {
        match self {
            StmtOut::Tables(t) => t.iter().map(|t| t.total).sum(),
            StmtOut::Affected(n) => *n,
            StmtOut::Failed(_) => 0,
        }
    }
}

/// Short text of a driver error: `[1146] Table 'x' doesn't exist`.
pub fn db_error(e: &db_core::DbError) -> String {
    if e.kind == db_core::ErrorKind::Timeout {
        return format!(
            "timed out after {} s and was stopped",
            STATEMENT_TIMEOUT.as_secs()
        );
    }
    match e.code {
        Some(c) => format!("[{c}] {}", e.message),
        None => e.message.clone(),
    }
}

/// Runs one statement, keeping at most `cap` rows per result set.
pub async fn run_one(d: &dyn Driver, sql: &str, cap: usize) -> StmtOut {
    let opts = QueryOptions {
        timeout: Some(STATEMENT_TIMEOUT),
        ..Default::default()
    };
    let mut h = d.query(sql.to_string(), opts);
    let mut tables: Vec<Table> = Vec::new();
    let mut affected: Option<u64> = None;
    let mut stopped = false;
    // A little over the server-side limit, in case a driver cannot stop it.
    let deadline = tokio::time::Instant::now() + STATEMENT_TIMEOUT + Duration::from_secs(5);
    loop {
        let ev = match tokio::time::timeout_at(deadline, h.next()).await {
            Ok(Some(ev)) => ev,
            Ok(None) => break,
            Err(_) => {
                return StmtOut::Failed(format!(
                    "timed out after {} s and was stopped",
                    STATEMENT_TIMEOUT.as_secs()
                ));
            }
        };
        match ev {
            QueryEvent::Started { .. } => {}
            QueryEvent::Columns(cols) => {
                tables.push(Table::new(cols.into_iter().map(|c| c.name).collect(), cap))
            }
            QueryEvent::Batch(b) => {
                if let Some(t) = tables.last_mut() {
                    t.push_batch(&b);
                    if t.done() && !stopped {
                        // Enough counted: stop reading the rest.
                        stopped = true;
                        h.cancel();
                    }
                }
            }
            QueryEvent::Affected { rows } => *affected.get_or_insert(0) += rows,
            QueryEvent::Finished(o) => match o {
                Outcome::Done { .. } => break,
                Outcome::Cancelled if stopped => break,
                Outcome::Cancelled => return StmtOut::Failed("cancelled".into()),
                Outcome::Failed(e) => return StmtOut::Failed(db_error(&e)),
            },
        }
    }
    match (tables.is_empty(), affected) {
        (false, _) => StmtOut::Tables(tables),
        (true, Some(n)) => StmtOut::Affected(n),
        (true, None) => StmtOut::Affected(0),
    }
}

/// Runs a script statement by statement on `d` and stops at the first
/// failure. `read_only`: the (begin, end) statements of the engine's
/// read-only transaction, wrapped around the script.
pub async fn run_script(
    d: &dyn Driver,
    stmts: &[String],
    cap: usize,
    read_only: Option<(&str, &str)>,
) -> Vec<StmtOut> {
    if let Some((begin, _)) = read_only
        && let StmtOut::Failed(m) = run_one(d, begin, 1).await
    {
        return vec![StmtOut::Failed(format!(
            "could not open a read-only transaction ({m}); nothing was sent"
        ))];
    }
    let mut out = Vec::new();
    for s in stmts {
        let r = run_one(d, s, cap).await;
        let failed = matches!(r, StmtOut::Failed(_));
        out.push(r);
        if failed {
            break;
        }
    }
    if let Some((_, end)) = read_only {
        let _ = run_one(d, end, 1).await;
    }
    out
}

/// The text of a whole run: one block per statement, numbered when there
/// are several; statements after a failure are listed as not run.
pub fn render_script(stmts: &[String], out: &[StmtOut]) -> String {
    if stmts.len() == 1 {
        return out.first().map(StmtOut::render).unwrap_or_default();
    }
    let mut parts = Vec::new();
    for (i, s) in stmts.iter().enumerate() {
        let head = format!("#{} {}", i + 1, head_of(s));
        match out.get(i) {
            Some(o) => parts.push(format!("{head}\n{}", o.render())),
            None => parts.push(format!("{head}\nnot run")),
        }
    }
    parts.join("\n\n")
}

/// First line of a statement, at most 60 chars.
fn head_of(s: &str) -> String {
    let line = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > 60 {
        format!("{}…", line.chars().take(60).collect::<String>())
    } else {
        line.to_string()
    }
}

/// Reads the whole answer of `sql` as text cells (for EXPLAIN).
pub async fn answer(d: &dyn Driver, sql: &str) -> Result<locus_core::plan::Answer, String> {
    let opts = QueryOptions {
        timeout: Some(STATEMENT_TIMEOUT),
        ..Default::default()
    };
    let mut h = d.query(sql.to_string(), opts);
    let mut a = locus_core::plan::Answer::default();
    let deadline = tokio::time::Instant::now() + STATEMENT_TIMEOUT + Duration::from_secs(5);
    loop {
        let ev = match tokio::time::timeout_at(deadline, h.next()).await {
            Ok(Some(ev)) => ev,
            Ok(None) => return Ok(a),
            Err(_) => return Err("timed out".into()),
        };
        match ev {
            QueryEvent::Columns(cols) => {
                a.columns = cols.into_iter().map(|c| c.name).collect();
                a.rows.clear();
            }
            QueryEvent::Batch(b) => {
                for r in 0..b.rows() {
                    a.rows.push(
                        (0..b.columns())
                            .map(|c| match b.cell(r, c) {
                                db_core::Cell::Null => None,
                                db_core::Cell::Text(s) => Some(s.to_string()),
                                db_core::Cell::Bytes(x) => {
                                    Some(String::from_utf8_lossy(x).into_owned())
                                }
                            })
                            .collect(),
                    );
                }
            }
            QueryEvent::Finished(Outcome::Failed(e)) => return Err(db_error(&e)),
            QueryEvent::Finished(_) => return Ok(a),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_text_numbers_statements_and_marks_the_rest() {
        let stmts = vec![
            "SELECT 1".to_string(),
            "UPDATE t SET a = 1\nWHERE b = 2".to_string(),
            "SELECT 3".to_string(),
        ];
        let mut t = Table::new(vec!["1".into()], 10);
        t.rows.push(vec!["1".into()]);
        t.total = 1;
        let out = vec![
            StmtOut::Tables(vec![t]),
            StmtOut::Failed("[1146] nope".into()),
        ];
        assert_eq!(
            render_script(&stmts, &out),
            "#1 SELECT 1\n1\n1\n(1 row)\n\n#2 UPDATE t SET a = 1\nerror: [1146] nope\n\n#3 SELECT 3\nnot run"
        );
        assert_eq!(
            render_script(&stmts[..1], &[StmtOut::Affected(3)]),
            "3 rows affected"
        );
    }

    #[test]
    fn errors_are_short() {
        let e = db_core::DbError::new(db_core::ErrorKind::Query, "Table 'x' doesn't exist")
            .with_code(1146);
        assert_eq!(db_error(&e), "[1146] Table 'x' doesn't exist");
        let t = db_core::DbError::new(db_core::ErrorKind::Timeout, "whatever");
        assert_eq!(db_error(&t), "timed out after 30 s and was stopped");
    }
}
