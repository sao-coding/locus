//! Production protection, pure part: given the connection's environment, its
//! lock state, the classified statements and the settings, what happens to a
//! run. No UI, no I/O, so every branch is unit-tested here.

use crate::store::Env;
use db_core::classify::Classified;

/// What the caller must do with a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Send it: nothing in it writes, or writes need no confirmation here.
    Send,
    /// Locked Production and the script writes: send nothing.
    Blocked(WriteInfo),
    /// Ask first. `type_keyword` is the word the user must type (Production only).
    Confirm {
        info: WriteInfo,
        type_keyword: Option<String>,
    },
}

/// The writing part of a script, for dialogs and the audit log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteInfo {
    /// Distinct keywords of the writing statements, in first-seen order.
    pub keywords: Vec<String>,
    /// Number of writing statements.
    pub count: usize,
    /// Some UPDATE / DELETE has no top-level WHERE.
    pub missing_where: bool,
}

impl WriteInfo {
    /// `None` when no statement writes.
    pub fn of(stmts: &[Classified]) -> Option<WriteInfo> {
        let mut keywords: Vec<String> = Vec::new();
        let mut count = 0;
        let mut missing_where = false;
        for s in stmts.iter().filter(|s| s.writes) {
            count += 1;
            missing_where |= s.missing_where;
            let k = if s.keyword.is_empty() {
                "?".to_string()
            } else {
                s.keyword.clone()
            };
            if !keywords.contains(&k) {
                keywords.push(k);
            }
        }
        (count > 0).then_some(WriteInfo {
            keywords,
            count,
            missing_where,
        })
    }

    /// The word to type to confirm: the keyword of the first writing statement.
    pub fn keyword(&self) -> &str {
        self.keywords.first().map(String::as_str).unwrap_or("?")
    }
}

/// Decides what a run of `stmts` on a connection of environment `env` does.
/// `locked` is only meaningful for Production (every other environment has no lock).
pub fn decide(env: Env, locked: bool, stmts: &[Classified], confirm_non_prod: bool) -> Decision {
    let Some(info) = WriteInfo::of(stmts) else {
        return Decision::Send;
    };
    match env {
        Env::Prod if locked => Decision::Blocked(info),
        Env::Prod => {
            let kw = info.keyword().to_string();
            Decision::Confirm {
                info,
                type_keyword: Some(kw),
            }
        }
        _ if confirm_non_prod => Decision::Confirm {
            info,
            type_keyword: None,
        },
        _ => Decision::Send,
    }
}

/// Whether what the user typed enables the button (case-insensitive, trimmed).
pub fn keyword_ok(typed: &str, keyword: &str) -> bool {
    !keyword.is_empty() && typed.trim().eq_ignore_ascii_case(keyword)
}

/// Whether the unlock dialog's Unlock button is enabled: the exact connection
/// name (case-sensitive, no trimming) and a non-empty reason.
pub fn unlock_ok(typed_name: &str, name: &str, reason: &str) -> bool {
    typed_name == name && !reason.trim().is_empty()
}

/// How long an unlock lasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnlockMode {
    /// For a duration (15 minutes).
    Timed,
    /// Until one confirmed write was sent.
    NextRun,
    /// Until the connection is closed or locked by hand.
    UntilDisconnect,
}

/// Duration of a timed unlock: 15 minutes. A `dev` build may shorten it with
/// `LOCUS_UNLOCK_SECS` (for tests); it can never lengthen it.
pub fn unlock_duration() -> std::time::Duration {
    const MAX: u64 = 15 * 60;
    #[cfg(feature = "dev")]
    let secs = std::env::var("LOCUS_UNLOCK_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .map_or(MAX, |s| s.min(MAX));
    #[cfg(not(feature = "dev"))]
    let secs = MAX;
    std::time::Duration::from_secs(secs)
}

/// "N min" text for the remaining time, rounded up (never "0 min" while unlocked).
pub fn minutes_left(secs: u64) -> u64 {
    secs.div_ceil(60).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use db_core::classify::classify_script;

    fn c(sql: &str) -> Vec<Classified> {
        classify_script(sql)
    }

    #[test]
    fn reads_always_send() {
        for env in [Env::Local, Env::Dev, Env::Staging, Env::Team, Env::Prod] {
            for locked in [true, false] {
                assert_eq!(
                    decide(env, locked, &c("SELECT 1; SELECT 2"), true),
                    Decision::Send,
                    "{env:?} locked={locked}"
                );
            }
        }
        assert_eq!(decide(Env::Prod, true, &[], true), Decision::Send);
    }

    #[test]
    fn locked_prod_blocks_any_write() {
        let d = decide(Env::Prod, true, &c("DELETE FROM t WHERE id = 1"), false);
        match d {
            Decision::Blocked(i) => assert_eq!(
                (i.keywords, i.count, i.missing_where),
                (vec!["DELETE".to_string()], 1, false)
            ),
            other => panic!("{other:?}"),
        }
        // A read first, a write later: still blocked as a whole.
        assert!(matches!(
            decide(Env::Prod, true, &c("SELECT 1; DROP TABLE t"), false),
            Decision::Blocked(_)
        ));
        // The setting for non-Production never loosens Production.
        assert!(matches!(
            decide(Env::Prod, true, &c("UPDATE t SET a = 1"), false),
            Decision::Blocked(_)
        ));
    }

    #[test]
    fn locked_prod_blocks_select_for_update() {
        assert!(matches!(
            decide(Env::Prod, true, &c("SELECT * FROM t FOR UPDATE"), true),
            Decision::Blocked(_)
        ));
    }

    #[test]
    fn unlocked_prod_asks_for_the_keyword() {
        match decide(Env::Prod, false, &c("DELETE FROM t WHERE id = 1"), false) {
            Decision::Confirm { info, type_keyword } => {
                assert_eq!(type_keyword.as_deref(), Some("DELETE"));
                assert!(!info.missing_where);
            }
            other => panic!("{other:?}"),
        }
        // The non-Production setting does not switch the Production dialog off.
        assert!(matches!(
            decide(Env::Prod, false, &c("INSERT INTO t VALUES (1)"), false),
            Decision::Confirm {
                type_keyword: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn missing_where_is_reported() {
        match decide(Env::Prod, false, &c("DELETE FROM t"), true) {
            Decision::Confirm { info, .. } => assert!(info.missing_where),
            other => panic!("{other:?}"),
        }
        match decide(Env::Dev, false, &c("UPDATE t SET a = 1"), true) {
            Decision::Confirm { info, .. } => assert!(info.missing_where),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn non_prod_confirms_without_keyword_when_enabled() {
        for env in [Env::Local, Env::Dev, Env::Staging, Env::Team] {
            // `locked` has no meaning outside Production.
            for locked in [true, false] {
                match decide(env, locked, &c("TRUNCATE TABLE t"), true) {
                    Decision::Confirm { info, type_keyword } => {
                        assert_eq!(type_keyword, None);
                        assert_eq!(info.keywords, vec!["TRUNCATE".to_string()]);
                    }
                    other => panic!("{env:?}: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn non_prod_sends_when_confirmation_is_off() {
        for env in [Env::Local, Env::Dev, Env::Staging, Env::Team] {
            assert_eq!(
                decide(env, true, &c("DELETE FROM t"), false),
                Decision::Send
            );
        }
    }

    #[test]
    fn several_writes_list_keywords_and_count() {
        let d = decide(
            Env::Prod,
            false,
            &c(
                "UPDATE t SET a = 1 WHERE b = 2; SELECT 1; DELETE FROM t WHERE x = 1; UPDATE u SET c = 3 WHERE d = 4",
            ),
            true,
        );
        match d {
            Decision::Confirm { info, type_keyword } => {
                assert_eq!(
                    info.keywords,
                    vec!["UPDATE".to_string(), "DELETE".to_string()]
                );
                assert_eq!(info.count, 3);
                assert_eq!(type_keyword.as_deref(), Some("UPDATE"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn keyword_and_unlock_checks() {
        assert!(keyword_ok("delete", "DELETE"));
        assert!(keyword_ok("  DELETE ", "DELETE"));
        assert!(!keyword_ok("DELET", "DELETE"));
        assert!(!keyword_ok("", ""));
        assert!(unlock_ok("prod-db", "prod-db", "JIRA-1"));
        assert!(!unlock_ok("Prod-db", "prod-db", "JIRA-1"));
        assert!(!unlock_ok("prod-db ", "prod-db", "JIRA-1"));
        assert!(!unlock_ok("prod-db", "prod-db", "   "));
    }

    #[test]
    fn minutes_round_up() {
        assert_eq!(minutes_left(0), 1);
        assert_eq!(minutes_left(1), 1);
        assert_eq!(minutes_left(60), 1);
        assert_eq!(minutes_left(61), 2);
        assert_eq!(minutes_left(900), 15);
    }
}
