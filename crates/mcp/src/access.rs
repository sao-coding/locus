//! What happens to SQL an MCP client sends on a connection: the connection's
//! MCP access (Off / Read-only / Read-write) first, then the Production guard
//! (`locus_core::guard::decide` with the connection's environment). Pure, so
//! every branch is unit-tested.

use db_core::classify::{Classified, StatementKind};
use locus_core::guard::{self, Decision, WriteInfo};
use locus_core::store::{DbKind, McpAccess, SavedConnection};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Send it. `read_only`: inside a server-side read-only transaction.
    Send { read_only: bool },
    /// Send nothing; the text goes back to the client.
    Refuse(String),
    /// Production with read-write access and the script writes: Locus asks
    /// the user first (blocks until Allow, Deny or 60 s).
    Confirm(WriteInfo),
}

/// The decision for `stmts` (the classified script) on `conn`.
pub fn decide(conn: &SavedConnection, stmts: &[Classified]) -> Verdict {
    let name = &conn.name;
    let writes = WriteInfo::of(stmts);
    match conn.mcp_access() {
        McpAccess::Off => Verdict::Refuse(format!("connection '{name}' is off for MCP")),
        McpAccess::ReadOnly => {
            if let Some(w) = writes {
                return Verdict::Refuse(format!(
                    "connection '{name}' is read-only for MCP; {} refused (nothing was sent)",
                    w.keywords.join(", ")
                ));
            }
            // COMMIT / ROLLBACK / BEGIN would end the read-only transaction.
            if stmts.iter().any(|s| s.kind == StatementKind::Transaction) {
                return Verdict::Refuse(format!(
                    "connection '{name}' is read-only for MCP; transaction control is not allowed"
                ));
            }
            Verdict::Send { read_only: true }
        }
        McpAccess::ReadWrite => {
            if conn.kind == DbKind::Sqlite && conn.read_only && writes.is_some() {
                return Verdict::Refuse(format!(
                    "connection '{name}' opens its SQLite file read-only; writes refused"
                ));
            }
            // MCP has no unlock: Production is never "locked" here, its
            // writes always go to the confirmation. The non-Production
            // "confirm writes" setting is for the user's tabs, not MCP.
            match guard::decide(conn.env, false, stmts, false) {
                Decision::Send => Verdict::Send { read_only: false },
                Decision::Confirm { info, .. } => Verdict::Confirm(info),
                Decision::Blocked(_) => {
                    Verdict::Refuse(format!("connection '{name}' blocks writes"))
                }
            }
        }
    }
}

/// Statement that opens the read-only transaction on this engine, and the
/// one that ends it. `None`: the engine has no such statement (SQLite uses
/// `PRAGMA query_only` through the driver; SQL Server relies on the
/// classifier alone).
pub fn read_only_txn(kind: DbKind) -> Option<(&'static str, &'static str)> {
    match kind {
        DbKind::Mysql => Some(("START TRANSACTION READ ONLY", "ROLLBACK")),
        DbKind::Postgres => Some(("BEGIN READ ONLY", "ROLLBACK")),
        DbKind::Oracle => Some(("SET TRANSACTION READ ONLY", "ROLLBACK")),
        DbKind::Sqlite | DbKind::SqlServer | DbKind::Redis => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use db_core::classify::classify_script;
    use locus_core::store::{Env, Tls};

    fn conn(env: Env, mcp: Option<McpAccess>) -> SavedConnection {
        SavedConnection {
            kind: DbKind::Mysql,
            id: "c1".into(),
            name: "bench".into(),
            host: "127.0.0.1".into(),
            port: 3306,
            user: "u".into(),
            database: None,
            tls: Tls::Disabled,
            color: None,
            icon: None,
            env,
            read_only: false,
            ssh: None,
            mcp,
        }
    }

    const ENVS: [Env; 5] = [Env::Local, Env::Dev, Env::Staging, Env::Team, Env::Prod];

    #[test]
    fn defaults_follow_the_environment() {
        for env in ENVS {
            let want = if env == Env::Prod {
                McpAccess::ReadOnly
            } else {
                McpAccess::ReadWrite
            };
            assert_eq!(conn(env, None).mcp_access(), want, "{env:?}");
        }
        let mut r = conn(Env::Dev, Some(McpAccess::ReadWrite));
        r.kind = DbKind::Redis;
        assert_eq!(r.mcp_access(), McpAccess::Off);
    }

    /// The whole table: env × access × read / write.
    #[test]
    fn decision_table() {
        let read = classify_script("SELECT 1; SELECT * FROM t");
        let write = classify_script("SELECT 1; DELETE FROM t WHERE id = 1");
        for env in ENVS {
            for access in [McpAccess::Off, McpAccess::ReadOnly, McpAccess::ReadWrite] {
                let c = conn(env, Some(access));
                let r = decide(&c, &read);
                let w = decide(&c, &write);
                match access {
                    McpAccess::Off => {
                        assert!(matches!(r, Verdict::Refuse(_)), "{env:?}");
                        assert!(matches!(w, Verdict::Refuse(_)), "{env:?}");
                    }
                    McpAccess::ReadOnly => {
                        assert_eq!(r, Verdict::Send { read_only: true }, "{env:?}");
                        match w {
                            Verdict::Refuse(m) => {
                                assert!(m.contains("'bench' is read-only for MCP"), "{m}");
                                assert!(m.contains("DELETE"), "{m}");
                            }
                            other => panic!("{env:?}: {other:?}"),
                        }
                    }
                    McpAccess::ReadWrite => {
                        assert_eq!(r, Verdict::Send { read_only: false }, "{env:?}");
                        if env == Env::Prod {
                            match w {
                                Verdict::Confirm(i) => assert_eq!(i.keywords, vec!["DELETE"]),
                                other => panic!("{other:?}"),
                            }
                        } else {
                            assert_eq!(w, Verdict::Send { read_only: false }, "{env:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn default_prod_refuses_writes_without_asking() {
        let c = conn(Env::Prod, None);
        assert!(matches!(
            decide(&c, &classify_script("UPDATE t SET a = 1 WHERE id = 2")),
            Verdict::Refuse(_)
        ));
        assert_eq!(
            decide(&c, &classify_script("SELECT 1")),
            Verdict::Send { read_only: true }
        );
    }

    #[test]
    fn read_only_refuses_ddl_unsure_text_and_transaction_control() {
        let c = conn(Env::Dev, Some(McpAccess::ReadOnly));
        for sql in [
            "DROP TABLE t",
            "CREATE TABLE t (a int)",
            "SELECT * FROM t FOR UPDATE",
            "GRANT ALL ON *.* TO x",
            "SELECT 'unterminated",
            "/*! DROP TABLE t */ SELECT 1",
            "CALL p()",
            "COMMIT",
            "BEGIN",
        ] {
            assert!(
                matches!(decide(&c, &classify_script(sql)), Verdict::Refuse(_)),
                "{sql}"
            );
        }
    }

    #[test]
    fn non_prod_confirm_setting_does_not_apply_and_sqlite_ro_file_refuses() {
        // A Dev write is sent without a question.
        let c = conn(Env::Dev, None);
        assert_eq!(
            decide(&c, &classify_script("DELETE FROM t")),
            Verdict::Send { read_only: false }
        );
        let mut s = conn(Env::Dev, None);
        s.kind = DbKind::Sqlite;
        s.read_only = true;
        assert!(matches!(
            decide(&s, &classify_script("INSERT INTO t VALUES (1)")),
            Verdict::Refuse(_)
        ));
    }

    #[test]
    fn read_only_transaction_statements() {
        assert_eq!(
            read_only_txn(DbKind::Mysql),
            Some(("START TRANSACTION READ ONLY", "ROLLBACK"))
        );
        assert_eq!(
            read_only_txn(DbKind::Postgres),
            Some(("BEGIN READ ONLY", "ROLLBACK"))
        );
        assert_eq!(
            read_only_txn(DbKind::Oracle),
            Some(("SET TRANSACTION READ ONLY", "ROLLBACK"))
        );
        assert_eq!(read_only_txn(DbKind::Sqlite), None);
        assert_eq!(read_only_txn(DbKind::SqlServer), None);
    }
}
