//! Transaction control of one query tab: Auto-commit (every statement on the
//! shared pool driver commits on its own) or Manual (the tab owns a pinned
//! session, the first writing statement sends `START TRANSACTION`, and the
//! user ends it with Commit / Rollback).
//!
//! [`TxState`] is the pure state machine the tab header, the pill and the
//! close / switch prompts read. [`TxSession`] is the pinned connection.
//!
//! What each engine does when something goes wrong inside a transaction
//! (the pill says it; nothing ends silently):
//!
//! | Event | MySQL / MariaDB | PostgreSQL | SQLite | Oracle |
//! |---|---|---|---|---|
//! | Cancel (KILL QUERY / cancel request / interrupt / break) | statement undone, transaction open | transaction aborted, only Rollback works | an interrupted INSERT / UPDATE / DELETE rolls the whole transaction back | statement undone (ORA-01013), transaction open |
//! | Statement error | statement undone (deadlock 1213: whole transaction rolled back) | transaction aborted | statement undone | statement undone |
//! | DDL | commits the transaction (implicit commit) | transactional | transactional | commits the transaction |
//! | Lost connection | server rolls back | server rolls back | (file: rolled back) | server rolls back |

use crate::driver::AnyDriver;
use db_core::classify::{Classified, StatementKind};
use db_core::{DbError, Driver, ErrorKind, Outcome, QueryEvent, QueryOptions};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The text that opens a transaction on every engine (SQLite maps it to
/// `BEGIN`, the Oracle sidecar to a `*sql.Tx`).
pub const BEGIN: &str = "START TRANSACTION";
pub const COMMIT: &str = "COMMIT";
pub const ROLLBACK: &str = "ROLLBACK";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TxMode {
    #[default]
    Auto,
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Mysql,
    Postgres,
    Sqlite,
    Oracle,
}

impl Engine {
    pub fn of(d: &AnyDriver) -> Engine {
        match d {
            AnyDriver::Mysql(_) => Engine::Mysql,
            AnyDriver::Postgres(_) => Engine::Postgres,
            AnyDriver::Sqlite(_) => Engine::Sqlite,
            AnyDriver::Oracle(_) => Engine::Oracle,
        }
    }

    /// The statement commits an open transaction before (and after) it runs.
    pub fn implicit_commit(self, c: &Classified) -> bool {
        match self {
            Engine::Mysql => {
                matches!(c.kind, StatementKind::Ddl | StatementKind::Grant)
                    || matches!(
                        c.keyword.as_str(),
                        "LOCK" | "UNLOCK" | "ANALYZE" | "OPTIMIZE" | "REPAIR" | "FLUSH"
                    )
            }
            Engine::Oracle => matches!(c.kind, StatementKind::Ddl | StatementKind::Grant),
            Engine::Postgres | Engine::Sqlite => false,
        }
    }
}

/// Why the last transaction ended without the user's Commit / Rollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// A DDL statement committed it (MySQL / Oracle).
    ImplicitCommit,
    /// The server rolled it back (deadlock, interrupted SQLite write).
    ServerRollback,
    /// The connection dropped; the server rolled it back.
    Lost,
    /// A COMMIT / ROLLBACK typed in the editor.
    Typed,
}

#[derive(Clone, Debug, Default)]
pub struct TxState {
    pub mode: TxMode,
    /// A transaction is open on the pinned session.
    pub open: bool,
    /// Statements run since it opened.
    pub statements: u32,
    /// PostgreSQL after an error: only Rollback works.
    pub aborted: bool,
    /// How the previous transaction ended, when not by Commit / Rollback.
    pub ended: Option<Ended>,
    pub last_activity: Option<Instant>,
    warned: bool,
}

/// What a lost connection, cancel or failure did, for the message the tab shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    /// Statement undone, transaction still open.
    StatementUndone,
    /// Transaction still open, but only Rollback works (PostgreSQL).
    Aborted,
    /// The server rolled the transaction back.
    RolledBack,
    /// The connection was lost; the server rolled back.
    Lost,
}

impl TxState {
    pub fn manual(&self) -> bool {
        self.mode == TxMode::Manual
    }

    /// Closing the tab, switching its connection or quitting must ask.
    pub fn needs_prompt(&self) -> bool {
        self.manual() && self.open
    }

    /// Switching mode: Auto → Manual is free; Manual → Auto needs the
    /// transaction ended first (the caller asks).
    pub fn set_mode(&mut self, mode: TxMode) -> bool {
        if self.mode == mode {
            return true;
        }
        if self.open {
            return false;
        }
        *self = TxState {
            mode,
            ..TxState::default()
        };
        true
    }

    /// Before a run in Manual mode: does `START TRANSACTION` go first? Only
    /// a statement that is not a plain SELECT starts one.
    pub fn needs_begin(&self, stmts: &[Classified]) -> bool {
        self.manual()
            && !self.open
            && stmts
                .iter()
                .any(|c| !matches!(c.kind, StatementKind::Select | StatementKind::Transaction))
    }

    pub fn began(&mut self, now: Instant) {
        self.open = true;
        self.statements = 0;
        self.aborted = false;
        self.ended = None;
        self.touch(now);
    }

    fn touch(&mut self, now: Instant) {
        self.last_activity = Some(now);
        self.warned = false;
    }

    /// One statement of a run finished without error.
    pub fn statement_done(&mut self, engine: Engine, c: &Classified, now: Instant) {
        if !self.manual() {
            return;
        }
        self.touch(now);
        if c.kind == StatementKind::Transaction {
            match c.keyword.as_str() {
                "COMMIT" | "ROLLBACK" | "ABORT" | "END" => {
                    if self.open {
                        self.end(Some(Ended::Typed));
                    }
                }
                "START" | "BEGIN" if !self.open => self.began(now),
                _ => {}
            }
            return;
        }
        if !self.open {
            return;
        }
        self.statements += 1;
        if engine.implicit_commit(c) {
            self.end(Some(Ended::ImplicitCommit));
        }
    }

    /// A grid Apply of `n` statements joined the open transaction.
    pub fn applied(&mut self, n: u32, now: Instant) {
        if self.open {
            self.statements += n;
            self.touch(now);
        }
    }

    /// The statement failed with `e` inside the transaction.
    pub fn statement_failed(&mut self, engine: Engine, e: &DbError, now: Instant) -> Effect {
        if !self.manual() {
            return Effect::None;
        }
        self.touch(now);
        if e.kind == ErrorKind::Connect {
            return self.lost();
        }
        if !self.open {
            return Effect::None;
        }
        match engine {
            Engine::Postgres => {
                self.aborted = true;
                Effect::Aborted
            }
            Engine::Mysql if e.code == Some(1213) => {
                self.end(Some(Ended::ServerRollback));
                Effect::RolledBack
            }
            _ => Effect::StatementUndone,
        }
    }

    /// The user cancelled `c` while it ran inside the transaction.
    pub fn statement_cancelled(
        &mut self,
        engine: Engine,
        c: Option<&Classified>,
        now: Instant,
    ) -> Effect {
        if !self.manual() || !self.open {
            return Effect::None;
        }
        self.touch(now);
        match engine {
            Engine::Postgres => {
                self.aborted = true;
                Effect::Aborted
            }
            Engine::Sqlite
                if c.is_some_and(|c| {
                    matches!(
                        c.kind,
                        StatementKind::Insert | StatementKind::Update | StatementKind::Delete
                    )
                }) =>
            {
                self.end(Some(Ended::ServerRollback));
                Effect::RolledBack
            }
            _ => Effect::StatementUndone,
        }
    }

    /// The pinned connection is gone: the server rolled back.
    pub fn lost(&mut self) -> Effect {
        if self.open {
            self.end(Some(Ended::Lost));
            Effect::Lost
        } else {
            Effect::None
        }
    }

    pub fn committed(&mut self) {
        self.end(None);
    }

    pub fn rolled_back(&mut self) {
        self.end(None);
    }

    fn end(&mut self, why: Option<Ended>) {
        self.open = false;
        self.aborted = false;
        self.statements = 0;
        self.ended = why;
        self.warned = false;
    }

    /// True once per idle stretch, when the open transaction has had no
    /// statement for `limit` (0 = never warn).
    pub fn idle_warning_due(&mut self, now: Instant, limit: Duration) -> bool {
        if limit.is_zero() || !self.open || self.warned {
            return false;
        }
        let due = self
            .last_activity
            .is_some_and(|t| now.duration_since(t) >= limit);
        if due {
            self.warned = true;
        }
        due
    }
}

/// Classifies `sql` statement by statement and, for the end of a
/// transaction, tells `ROLLBACK TO ...` apart from `ROLLBACK`.
pub fn classify(sql: &str) -> Vec<Classified> {
    db_core::sql::split_statements(sql)
        .into_iter()
        .map(|r| {
            let text = &sql[r];
            let mut c = db_core::classify::classify(text);
            if c.kind == StatementKind::Transaction && c.keyword == "ROLLBACK" {
                let up = text.to_ascii_uppercase();
                if up.split_whitespace().nth(1).is_some_and(|w| w == "TO") {
                    c.keyword = "ROLLBACK TO".into();
                }
            }
            c
        })
        .collect()
}

/// The tab's pinned connection for Manual mode.
#[derive(Clone)]
pub struct TxSession {
    pub driver: Arc<dyn Driver>,
    pub engine: Engine,
    pub conn_id: String,
}

impl TxSession {
    pub async fn open(d: &AnyDriver, conn_id: &str) -> Result<TxSession, DbError> {
        let s = d.session().await?;
        Ok(TxSession {
            driver: Arc::from(s),
            engine: Engine::of(d),
            conn_id: conn_id.to_string(),
        })
    }

    pub async fn begin(&self) -> Result<(), DbError> {
        control(&*self.driver, BEGIN).await
    }

    pub async fn commit(&self) -> Result<(), DbError> {
        control(&*self.driver, COMMIT).await
    }

    pub async fn rollback(&self) -> Result<(), DbError> {
        control(&*self.driver, ROLLBACK).await
    }
}

/// Runs a statement without a result and waits for its end.
pub async fn control(d: &dyn Driver, sql: &str) -> Result<(), DbError> {
    let mut h = d.query(sql.to_string(), QueryOptions::default());
    while let Some(ev) = h.next().await {
        match ev {
            QueryEvent::Finished(Outcome::Failed(e)) => return Err(e),
            QueryEvent::Finished(Outcome::Cancelled) => {
                return Err(DbError::new(ErrorKind::Query, "cancelled"));
            }
            QueryEvent::Finished(_) => return Ok(()),
            _ => {}
        }
    }
    Err(DbError::new(ErrorKind::Connect, "the connection is gone"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(sql: &str) -> Classified {
        classify(sql).remove(0)
    }

    fn manual() -> TxState {
        TxState {
            mode: TxMode::Manual,
            ..Default::default()
        }
    }

    #[test]
    fn auto_mode_never_opens_or_prompts() {
        let mut s = TxState::default();
        assert!(!s.needs_begin(&classify("INSERT INTO t VALUES (1)")));
        s.statement_done(
            Engine::Mysql,
            &c("INSERT INTO t VALUES (1)"),
            Instant::now(),
        );
        assert!(!s.open && !s.needs_prompt());
    }

    #[test]
    fn first_write_begins_and_counts() {
        let mut s = manual();
        assert!(
            !s.needs_begin(&classify("SELECT 1")),
            "a plain SELECT does not open a transaction"
        );
        let stmts = classify("SELECT 1; INSERT INTO t VALUES (1); UPDATE t SET a = 2");
        assert!(s.needs_begin(&stmts));
        let now = Instant::now();
        s.began(now);
        for st in &stmts {
            s.statement_done(Engine::Mysql, st, now);
        }
        assert!(s.open && s.statements == 3 && s.needs_prompt());
        assert!(!s.needs_begin(&stmts), "already open");
        s.committed();
        assert!(!s.open && s.statements == 0 && s.ended.is_none());
    }

    #[test]
    fn ddl_commits_on_mysql_and_oracle_only() {
        for (e, ends) in [
            (Engine::Mysql, true),
            (Engine::Oracle, true),
            (Engine::Postgres, false),
            (Engine::Sqlite, false),
        ] {
            let mut s = manual();
            s.began(Instant::now());
            s.statement_done(e, &c("INSERT INTO t VALUES (1)"), Instant::now());
            s.statement_done(e, &c("CREATE TABLE x (a int)"), Instant::now());
            assert_eq!(!s.open, ends, "{e:?}");
            assert_eq!(s.ended == Some(Ended::ImplicitCommit), ends, "{e:?}");
        }
    }

    #[test]
    fn typed_commit_and_rollback_end_but_rollback_to_does_not() {
        let mut s = manual();
        s.began(Instant::now());
        s.statement_done(
            Engine::Postgres,
            &c("ROLLBACK TO SAVEPOINT a"),
            Instant::now(),
        );
        assert!(s.open);
        s.statement_done(Engine::Postgres, &c("COMMIT"), Instant::now());
        assert!(!s.open && s.ended == Some(Ended::Typed));
        s.statement_done(Engine::Postgres, &c("BEGIN"), Instant::now());
        assert!(s.open, "a typed BEGIN opens");
        s.statement_done(Engine::Postgres, &c("ROLLBACK"), Instant::now());
        assert!(!s.open);
    }

    #[test]
    fn failures_and_cancels_per_engine() {
        let now = Instant::now();
        let q = DbError::new(ErrorKind::Query, "x");
        let mut s = manual();
        s.began(now);
        assert_eq!(
            s.statement_failed(Engine::Postgres, &q, now),
            Effect::Aborted
        );
        assert!(s.open && s.aborted);

        let mut s = manual();
        s.began(now);
        assert_eq!(
            s.statement_failed(Engine::Mysql, &q, now),
            Effect::StatementUndone
        );
        let mut dl = DbError::new(ErrorKind::Query, "deadlock");
        dl.code = Some(1213);
        assert_eq!(
            s.statement_failed(Engine::Mysql, &dl, now),
            Effect::RolledBack
        );
        assert!(!s.open && s.ended == Some(Ended::ServerRollback));

        let mut s = manual();
        s.began(now);
        assert_eq!(
            s.statement_cancelled(Engine::Mysql, Some(&c("UPDATE t SET a = 1")), now),
            Effect::StatementUndone
        );
        assert!(s.open, "KILL QUERY keeps the MySQL transaction");
        assert_eq!(
            s.statement_cancelled(Engine::Sqlite, Some(&c("SELECT 1")), now),
            Effect::StatementUndone
        );
        assert_eq!(
            s.statement_cancelled(Engine::Sqlite, Some(&c("DELETE FROM t")), now),
            Effect::RolledBack
        );
        assert!(!s.open);

        let mut s = manual();
        s.began(now);
        assert_eq!(
            s.statement_failed(
                Engine::Oracle,
                &DbError::new(ErrorKind::Connect, "gone"),
                now
            ),
            Effect::Lost
        );
        assert!(!s.open && s.ended == Some(Ended::Lost));
    }

    #[test]
    fn mode_switch_needs_closed_transaction() {
        let mut s = manual();
        s.began(Instant::now());
        assert!(!s.set_mode(TxMode::Auto));
        s.rolled_back();
        assert!(s.set_mode(TxMode::Auto) && s.mode == TxMode::Auto);
    }

    #[test]
    fn idle_warning_once_per_stretch() {
        let t0 = Instant::now();
        let mut s = manual();
        let lim = Duration::from_secs(60);
        assert!(!s.idle_warning_due(t0 + lim, lim), "closed: no warning");
        s.began(t0);
        assert!(!s.idle_warning_due(t0 + Duration::from_secs(30), lim));
        assert!(s.idle_warning_due(t0 + lim, lim));
        assert!(!s.idle_warning_due(t0 + lim * 2, lim), "once");
        s.statement_done(Engine::Mysql, &c("INSERT INTO t VALUES (1)"), t0 + lim * 2);
        assert!(
            s.idle_warning_due(t0 + lim * 3, lim),
            "again after activity"
        );
        assert!(!s.idle_warning_due(t0 + lim * 9, Duration::ZERO), "0 = off");
    }
}
