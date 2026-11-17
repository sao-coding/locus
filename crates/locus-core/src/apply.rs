//! Apply staged grid edits: one transaction on one pinned server connection
//! (`AnyDriver::session`), every value a bound parameter. Each UPDATE /
//! DELETE / INSERT must affect exactly one row; anything else, or any error,
//! rolls the whole transaction back and the edits stay staged.

use crate::driver::AnyDriver;
use db_core::{DbError, Driver, Outcome, Param, QueryEvent, QueryOptions};
use rust_i18n::t;
use std::sync::Arc;

/// One statement of an Apply: SQL with `?` placeholders and its values.
#[derive(Clone, Debug)]
pub struct Statement {
    pub sql: String,
    pub params: Vec<Param>,
    /// UPDATE / DELETE must touch exactly one row.
    pub expect_one: bool,
    /// For UPDATE: a `SELECT COUNT(*)` on the same key, run when the UPDATE
    /// reports 0 changed rows (the new value equals the stored one).
    pub check: Option<(String, Vec<Param>)>,
}

/// Runs one statement and returns its affected-row count (or the first
/// column of the first row, for a `SELECT COUNT(*)`).
pub(crate) async fn exec(s: &dyn Driver, sql: String, params: Vec<Param>) -> Result<u64, DbError> {
    let mut h = if params.is_empty() {
        s.query(sql, QueryOptions::default())
    } else {
        s.execute(sql, params, QueryOptions::default())
    };
    let mut n = 0u64;
    while let Some(ev) = h.next().await {
        match ev {
            QueryEvent::Affected { rows } => n = rows,
            QueryEvent::Batch(b) if b.rows() > 0 => {
                if let db_core::Cell::Text(v) = b.cell(0, 0) {
                    n = v.parse().unwrap_or(0);
                }
            }
            QueryEvent::Finished(Outcome::Failed(e)) => return Err(e),
            QueryEvent::Finished(Outcome::Cancelled) => {
                return Err(DbError::new(
                    db_core::ErrorKind::Query,
                    t!("edit.err_cancelled").to_string(),
                ));
            }
            QueryEvent::Finished(_) => return Ok(n),
            _ => {}
        }
    }
    Err(DbError::new(
        db_core::ErrorKind::Internal,
        t!("error.driver_gone").to_string(),
    ))
}

fn describe(e: &DbError) -> String {
    match e.code {
        Some(c) => format!("[{c}] {}", e.message),
        None => e.message.clone(),
    }
}

pub async fn apply(driver: Arc<AnyDriver>, stmts: Vec<Statement>) -> Result<(), String> {
    let session = driver.session().await.map_err(|e| describe(&e))?;
    let s: &dyn Driver = &*session;
    exec(s, "START TRANSACTION".into(), Vec::new())
        .await
        .map_err(|e| describe(&e))?;
    for (i, st) in stmts.into_iter().enumerate() {
        let res = exec(s, st.sql.clone(), st.params.clone()).await;
        let fail = match res {
            Err(e) => Some(
                t!(
                    "edit.err_statement",
                    n = (i + 1).to_string(),
                    msg = describe(&e)
                )
                .into_owned(),
            ),
            Ok(1) => None,
            Ok(0) if st.check.is_some() => {
                // MySQL counts changed rows: an UPDATE to an equal value
                // affects 0. Count the key's rows in the same transaction.
                let (sql, params) = st.check.clone().unwrap();
                match exec(s, sql, params).await {
                    Ok(1) => None,
                    Ok(n) => Some(
                        t!(
                            "edit.err_rows",
                            n = (i + 1).to_string(),
                            rows = n.to_string()
                        )
                        .into_owned(),
                    ),
                    Err(e) => Some(
                        t!(
                            "edit.err_statement",
                            n = (i + 1).to_string(),
                            msg = describe(&e)
                        )
                        .into_owned(),
                    ),
                }
            }
            Ok(n) if !st.expect_one => {
                let _ = n;
                None
            }
            Ok(n) => Some(
                t!(
                    "edit.err_rows",
                    n = (i + 1).to_string(),
                    rows = n.to_string()
                )
                .into_owned(),
            ),
        };
        if let Some(msg) = fail {
            let _ = exec(s, "ROLLBACK".into(), Vec::new()).await;
            return Err(format!("{msg} {}", t!("edit.rolled_back")));
        }
    }
    if let Err(e) = exec(s, "COMMIT".into(), Vec::new()).await {
        let _ = exec(s, "ROLLBACK".into(), Vec::new()).await;
        return Err(format!("{} {}", describe(&e), t!("edit.rolled_back")));
    }
    Ok(())
}

/// Apply inside the tab's open Manual transaction: the edits run behind a
/// savepoint on the pinned session, nothing is committed. A failure rolls
/// back to the savepoint only, so earlier work in the transaction stays.
/// `begin` sends `START TRANSACTION` first (no transaction open yet).
pub async fn apply_in_tx(
    session: &dyn Driver,
    begin: bool,
    stmts: Vec<Statement>,
) -> Result<(), String> {
    let s = session;
    if begin {
        exec(s, crate::txn::BEGIN.into(), Vec::new())
            .await
            .map_err(|e| describe(&e))?;
    }
    exec(s, "SAVEPOINT wb_apply".into(), Vec::new())
        .await
        .map_err(|e| describe(&e))?;
    for (i, st) in stmts.into_iter().enumerate() {
        let fail = match exec(s, st.sql.clone(), st.params.clone()).await {
            Err(e) => Some(
                t!(
                    "edit.err_statement",
                    n = (i + 1).to_string(),
                    msg = describe(&e)
                )
                .into_owned(),
            ),
            Ok(1) => None,
            Ok(0) if st.check.is_some() => {
                let (sql, params) = st.check.clone().unwrap();
                match exec(s, sql, params).await {
                    Ok(1) => None,
                    Ok(n) => Some(
                        t!(
                            "edit.err_rows",
                            n = (i + 1).to_string(),
                            rows = n.to_string()
                        )
                        .into_owned(),
                    ),
                    Err(e) => Some(
                        t!(
                            "edit.err_statement",
                            n = (i + 1).to_string(),
                            msg = describe(&e)
                        )
                        .into_owned(),
                    ),
                }
            }
            Ok(_) if !st.expect_one => None,
            Ok(n) => Some(
                t!(
                    "edit.err_rows",
                    n = (i + 1).to_string(),
                    rows = n.to_string()
                )
                .into_owned(),
            ),
        };
        if let Some(msg) = fail {
            let _ = exec(s, "ROLLBACK TO SAVEPOINT wb_apply".into(), Vec::new()).await;
            return Err(format!("{msg} {}", t!("txn.apply_undone")));
        }
    }
    Ok(())
}
