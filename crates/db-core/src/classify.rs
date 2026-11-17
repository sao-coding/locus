//! Statement classifier: what a statement does, so the app can guard
//! Production connections.
//!
//! The answer errs towards "write". Anything the scanner is not sure about
//! (an unterminated string or comment, a string whose end depends on whether
//! the server treats `\` as an escape, a MySQL `/*! .. */` comment that the
//! server runs as code, an unknown first keyword) is reported as a write.

use crate::sql::split_statements;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatementKind {
    Select,
    Insert,
    Update,
    Delete,
    Merge,
    Ddl,
    Grant,
    Call,
    Transaction,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Classified {
    pub kind: StatementKind,
    /// Upper-case first keyword, e.g. `DELETE`, `CREATE`. Empty when the
    /// statement has no words.
    pub keyword: String,
    /// The statement may change data, schema, privileges or server state.
    pub writes: bool,
    /// An UPDATE or DELETE without a top-level `WHERE`.
    pub missing_where: bool,
}

/// Classifies one statement (no terminating `;` needed).
pub fn classify(statement: &str) -> Classified {
    let lx = lex(statement);
    let mut c = classify_toks(&lx.toks);
    if lx.unsure {
        c.writes = true;
    }
    c
}

/// Classifies every statement of a script, in order.
///
/// Statements are split with [`split_statements`]. A `BEGIN .. END` block or
/// procedure body that contains `;` is split into pieces as well; the piece
/// that opens the block is a `Call` / `Ddl` write, so the script as a whole
/// still reports a write.
pub fn classify_script(sql: &str) -> Vec<Classified> {
    split_statements(sql)
        .into_iter()
        .map(|r| classify(&sql[r]))
        .collect()
}

#[derive(Debug, PartialEq)]
enum T {
    /// Bare word, upper-cased.
    Word(String),
    Punct(u8),
    /// String, quoted identifier, number: never a keyword.
    Lit,
}

struct Lexed {
    toks: Vec<T>,
    unsure: bool,
}

/// End of the quoted run opening at `i`, or `None` when it never closes.
fn quote_end(b: &[u8], i: usize, backslash: bool) -> Option<usize> {
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() {
        let c = b[j];
        if c == b'\\' && backslash {
            j += 2;
        } else if c == q {
            if b.get(j + 1) == Some(&q) {
                j += 2;
            } else {
                return Some(j + 1);
            }
        } else {
            j += 1;
        }
    }
    None
}

fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'$' | b'#') || c >= 0x80
}

fn lex(sql: &str) -> Lexed {
    let b = sql.as_bytes();
    let n = b.len();
    let mut toks: Vec<T> = Vec::new();
    let mut unsure = false;
    let mut i = 0;
    while i < n {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'-' && b.get(i + 1) == Some(&b'-') {
            i = sql[i..].find('\n').map_or(n, |p| i + p);
        } else if c == b'#' {
            // `#>` is a PostgreSQL operator; `FROM #tmp` is a SQL Server temp
            // table. Everything else is a MySQL line comment.
            let temp = b
                .get(i + 1)
                .is_some_and(|x| x.is_ascii_alphanumeric() || *x == b'_' || *x == b'#')
                && matches!(toks.last(), Some(T::Word(w)) if matches!(w.as_str(), "FROM" | "JOIN" | "INTO" | "UPDATE" | "TABLE"));
            if temp {
                let mut j = i + 1;
                while j < n && is_word_byte(b[j]) {
                    j += 1;
                }
                toks.push(T::Word(sql[i..j].to_ascii_uppercase()));
                i = j;
            } else if b.get(i + 1) == Some(&b'>') {
                toks.push(T::Punct(b'#'));
                i += 1;
            } else {
                i = sql[i..].find('\n').map_or(n, |p| i + p);
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            match sql[i + 2..].find("*/") {
                None => {
                    unsure = true;
                    i = n;
                }
                Some(p) => {
                    let body = &sql[i + 2..i + 2 + p];
                    // `/*! .. */` and `/*M! .. */` are executed by MySQL /
                    // MariaDB; a nested `/*` ends differently per server.
                    if body.starts_with('!') || body.starts_with("M!") || body.contains("/*") {
                        unsure = true;
                    }
                    i += 2 + p + 2;
                }
            }
        } else if c == b'\'' || c == b'"' || c == b'`' {
            let std = quote_end(b, i, false);
            if c == b'`' {
                match std {
                    Some(e) => i = e,
                    None => {
                        unsure = true;
                        i = n;
                    }
                }
            } else {
                let esc = quote_end(b, i, true);
                if esc != std {
                    unsure = true; // `\'` ends the string on one server and not on another
                }
                match std.or(esc) {
                    Some(e) => i = e,
                    None => {
                        unsure = true;
                        i = n;
                    }
                }
            }
            toks.push(T::Lit);
        } else if c == b'$' {
            // `$tag$ .. $tag$`; `$1` is a parameter.
            let mut j = i + 1;
            if b.get(j)
                .is_some_and(|x| x.is_ascii_alphabetic() || *x == b'_')
            {
                while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
            }
            if b.get(j) == Some(&b'$') {
                let needle = &sql[i..=j];
                match sql[j + 1..].find(needle) {
                    Some(p) => i = j + 1 + p + needle.len(),
                    None => {
                        unsure = true;
                        i = n;
                    }
                }
                toks.push(T::Lit);
            } else {
                toks.push(T::Punct(b'$'));
                i += 1;
            }
        } else if c == b'[' {
            // SQL Server quoted identifier; `]]` is an escaped `]`.
            let mut j = i + 1;
            let mut close = None;
            while let Some(p) = sql[j..].find(']') {
                j += p + 1;
                if b.get(j) == Some(&b']') {
                    j += 1;
                } else {
                    close = Some(j);
                    break;
                }
            }
            match close {
                Some(e) => {
                    toks.push(T::Lit);
                    i = e;
                }
                None => {
                    toks.push(T::Punct(b'['));
                    i += 1;
                }
            }
        } else if c.is_ascii_digit() {
            let mut j = i;
            while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            toks.push(T::Lit);
            i = j;
        } else if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            let mut j = i;
            while j < n && is_word_byte(b[j]) {
                j += 1;
            }
            let w = sql[i..j].to_ascii_uppercase();
            // Oracle alternative quoting: q'[ .. ]', nq'{ .. }'
            if (w == "Q" || w == "NQ")
                && b.get(j) == Some(&b'\'')
                && j + 1 < n
                && !b[j + 1].is_ascii_whitespace()
            {
                let open = b[j + 1];
                let close = match open {
                    b'[' => b']',
                    b'(' => b')',
                    b'{' => b'}',
                    b'<' => b'>',
                    o => o,
                };
                let pat = [close, b'\''];
                match b[j + 2..].windows(2).position(|x| x == pat) {
                    Some(p) => i = j + 2 + p + 2,
                    None => {
                        unsure = true;
                        i = n;
                    }
                }
                toks.push(T::Lit);
            } else {
                toks.push(T::Word(w));
                i = j;
            }
        } else {
            toks.push(T::Punct(c));
            i += 1;
        }
    }
    Lexed { toks, unsure }
}

fn w(t: &[T], i: usize) -> Option<&str> {
    match t.get(i) {
        Some(T::Word(s)) => Some(s.as_str()),
        _ => None,
    }
}

fn is_p(t: &[T], i: usize, c: u8) -> bool {
    matches!(t.get(i), Some(T::Punct(x)) if *x == c)
}

fn result(kw: &str, kind: StatementKind, writes: bool) -> Classified {
    Classified {
        kind,
        keyword: kw.to_string(),
        writes,
        missing_where: false,
    }
}

/// A `WHERE` outside every parenthesis.
fn has_top_where(t: &[T]) -> bool {
    let mut depth = 0i32;
    for x in t {
        match x {
            T::Punct(b'(') => depth += 1,
            T::Punct(b')') => depth -= 1,
            T::Word(s) if depth == 0 && s == "WHERE" => return true,
            _ => {}
        }
    }
    false
}

/// Signs that a read-looking statement writes or locks: `INTO`, `FOR UPDATE`
/// / `FOR SHARE`, `LOCK IN SHARE MODE`, and a data-modifying statement inside
/// a parenthesis (`WITH d AS (DELETE .. RETURNING *) SELECT ..`).
fn write_marker(t: &[T]) -> bool {
    for i in 0..t.len() {
        let Some(x) = w(t, i) else { continue };
        match x {
            "INTO" => return true,
            "FOR" if matches!(w(t, i + 1), Some("UPDATE" | "NO" | "SHARE" | "KEY")) => return true,
            "LOCK" if w(t, i + 1) == Some("IN") => return true,
            "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "REPLACE" | "TRUNCATE"
                if i > 0 && is_p(t, i - 1, b'(') =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Built-in functions that change server state when a plain `SELECT` calls
/// them (lower-case). `SLEEP` / `BENCHMARK` only occupy the session and are
/// not listed; `GET_LOCK` holds a lock others can wait on, so it is.
/// User-defined functions cannot be judged and stay reads.
const STATE_FUNCTIONS: &[&str] = &[
    // PostgreSQL
    "pg_terminate_backend",
    "pg_cancel_backend",
    "pg_reload_conf",
    "pg_rotate_logfile",
    "setval",
    "nextval",
    "pg_advisory_lock",
    "pg_advisory_xact_lock",
    "pg_try_advisory_lock",
    "pg_try_advisory_xact_lock",
    "pg_advisory_lock_shared",
    "pg_advisory_unlock",
    "pg_advisory_unlock_all",
    "pg_switch_wal",
    "pg_create_restore_point",
    "set_config",
    "lo_import",
    "lo_export",
    "lo_unlink",
    "pg_file_write",
    "dblink_exec",
    // MySQL / MariaDB
    "get_lock",
    "release_lock",
    "release_all_locks",
    "load_file",
    // SQLite
    "load_extension",
];

/// The first state-changing function call: a listed built-in followed by `(`,
/// or an Oracle `DBMS_*.` / `UTL_*.` package call (`DBMS_LOCK.SLEEP` aside).
fn side_effect_call(t: &[T]) -> Option<String> {
    for i in 0..t.len() {
        let Some(x) = w(t, i) else { continue };
        if is_p(t, i + 1, b'(') && STATE_FUNCTIONS.contains(&x.to_ascii_lowercase().as_str()) {
            return Some(x.to_string());
        }
        if (x.starts_with("DBMS_") || x.starts_with("UTL_")) && is_p(t, i + 1, b'.') {
            if x == "DBMS_LOCK" && w(t, i + 2) == Some("SLEEP") {
                continue;
            }
            return Some(x.to_string());
        }
    }
    None
}

const MAIN_VERBS: [&str; 7] = [
    "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "VALUES", "REPLACE",
];
const STARTERS: [&str; 15] = [
    "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "REPLACE", "WITH", "VALUES", "CREATE",
    "ALTER", "DROP", "TRUNCATE", "CALL", "EXECUTE", "EXEC",
];

fn classify_toks(all: &[T]) -> Classified {
    let mut s = 0;
    while is_p(all, s, b'(') {
        s += 1;
    }
    let t = &all[s..];
    let Some(first) = t.first() else {
        return result("", StatementKind::Other, s > 0);
    };
    let Some(kw) = w(t, 0) else {
        let kw = match first {
            T::Punct(c) => (*c as char).to_string(),
            _ => String::new(),
        };
        return result(&kw, StatementKind::Other, true);
    };
    use StatementKind as K;
    match kw {
        "SELECT" | "VALUES" => match side_effect_call(t) {
            Some(f) => result(&f, K::Call, true),
            None => result(kw, K::Select, write_marker(t)),
        },
        "WITH" => {
            let mut depth = 0i32;
            for (k, x) in t.iter().enumerate().skip(1) {
                match x {
                    T::Punct(b'(') => depth += 1,
                    T::Punct(b')') => depth -= 1,
                    T::Word(v) if depth == 0 => {
                        if MAIN_VERBS.contains(&v.as_str()) {
                            let mut c = classify_toks(&t[k..]);
                            c.keyword = "WITH".into();
                            c.writes |= write_marker(t);
                            if let Some(f) = side_effect_call(t) {
                                c = result(&f, K::Call, true);
                            }
                            return c;
                        }
                        // Oracle `WITH FUNCTION .. BEGIN .. END; SELECT ..`
                        if matches!(v.as_str(), "FUNCTION" | "PROCEDURE" | "BEGIN") {
                            return result("WITH", K::Call, true);
                        }
                    }
                    _ => {}
                }
            }
            result("WITH", K::Other, true)
        }
        "INSERT" | "REPLACE" => result(kw, K::Insert, true),
        "UPDATE" => Classified {
            missing_where: !has_top_where(t),
            ..result(kw, K::Update, true)
        },
        "DELETE" => Classified {
            missing_where: !has_top_where(t),
            ..result(kw, K::Delete, true)
        },
        "MERGE" => result(kw, K::Merge, true),
        "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "RENAME" | "COMMENT" => result(kw, K::Ddl, true),
        "GRANT" | "REVOKE" | "DENY" => result(kw, K::Grant, true),
        "CALL" | "EXEC" | "EXECUTE" | "DO" | "DECLARE" => result(kw, K::Call, true),
        "BEGIN" => {
            let tx = match w(t, 1) {
                None => t.len() == 1 || is_p(t, 1, b';'),
                Some(
                    "TRANSACTION" | "TRAN" | "WORK" | "ISOLATION" | "DEFERRABLE" | "IMMEDIATE"
                    | "EXCLUSIVE" | "DISTRIBUTED",
                ) => true,
                Some("READ") => matches!(w(t, 2), Some("ONLY" | "WRITE")),
                Some("NOT") => w(t, 2) == Some("DEFERRABLE"),
                Some(_) => false,
            };
            if tx {
                result(kw, K::Transaction, false)
            } else {
                result(kw, K::Call, true)
            }
        }
        "COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE" | "ABORT" => {
            result(kw, K::Transaction, false)
        }
        "START" if w(t, 1) == Some("TRANSACTION") => result(kw, K::Transaction, false),
        "EXPLAIN" => {
            let analyze = t
                .iter()
                .take_while(|x| !matches!(x, T::Word(v) if STARTERS.contains(&v.as_str())))
                .any(|x| matches!(x, T::Word(v) if v == "ANALYZE" || v == "ANALYSE"));
            if !analyze {
                return result(kw, K::Other, false);
            }
            // The inner statement is the first top-level starter word.
            let mut depth = 0i32;
            for (k, x) in t.iter().enumerate().skip(1) {
                match x {
                    T::Punct(b'(') => depth += 1,
                    T::Punct(b')') => depth -= 1,
                    T::Word(v) if depth == 0 && STARTERS.contains(&v.as_str()) => {
                        let mut c = classify_toks(&t[k..]);
                        c.keyword = "EXPLAIN".into();
                        return c;
                    }
                    _ => {}
                }
            }
            result(kw, K::Other, true)
        }
        "DESCRIBE" | "DESC" | "SHOW" | "USE" => result(kw, K::Other, false),
        // SQLite: the schema-reading pragmas are reads; every other pragma may change state.
        "PRAGMA" => {
            let at = if is_p(t, 2, b'.') { 3 } else { 1 };
            let sets = t.iter().any(|x| matches!(x, T::Punct(b'=')));
            let read = matches!(
                w(t, at),
                Some(
                    "TABLE_INFO"
                        | "TABLE_XINFO"
                        | "TABLE_LIST"
                        | "INDEX_LIST"
                        | "INDEX_INFO"
                        | "INDEX_XINFO"
                        | "FOREIGN_KEY_LIST"
                        | "DATABASE_LIST"
                        | "COLLATION_LIST"
                        | "FUNCTION_LIST"
                        | "MODULE_LIST"
                        | "PRAGMA_LIST"
                        | "COMPILE_OPTIONS"
                )
            );
            result(kw, K::Other, sets || !read)
        }
        "SET" => {
            if w(t, 1) == Some("TRANSACTION") || w(t, 1) == Some("TRAN") {
                return result(kw, K::Transaction, false);
            }
            // Server-wide or account-level settings are not session variables.
            let server = w(t, 1) == Some("DEFAULT") || t.iter().skip(1).take(6).any(|x| matches!(x, T::Word(v) if matches!(v.as_str(), "GLOBAL" | "PERSIST" | "PERSIST_ONLY" | "PASSWORD")));
            result(kw, K::Other, server)
        }
        _ => result(kw, K::Other, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use StatementKind as K;

    fn c(sql: &str) -> Classified {
        classify(sql)
    }

    fn check(sql: &str, kind: K, writes: bool, missing_where: bool) {
        let r = c(sql);
        assert_eq!(
            (r.kind, r.writes, r.missing_where),
            (kind, writes, missing_where),
            "{sql}: {r:?}"
        );
    }

    #[test]
    fn state_changing_functions_are_writes() {
        let all = [
            "pg_terminate_backend(1)",
            "pg_cancel_backend(1)",
            "pg_reload_conf()",
            "pg_rotate_logfile()",
            "setval('s',1)",
            "nextval('s')",
            "pg_advisory_lock(1)",
            "pg_advisory_xact_lock(1)",
            "pg_try_advisory_lock(1)",
            "pg_advisory_unlock(1)",
            "pg_advisory_unlock_all()",
            "pg_switch_wal()",
            "pg_create_restore_point('x')",
            "set_config('a','b',false)",
            "lo_import('/f')",
            "lo_export(1,'/f')",
            "lo_unlink(1)",
            "pg_file_write('f','x',false)",
            "dblink_exec('c','x')",
            "GET_LOCK('x',1)",
            "RELEASE_LOCK('x')",
            "RELEASE_ALL_LOCKS()",
            "LOAD_FILE('/etc/passwd')",
            "load_extension('x')",
            "DBMS_LOCK.REQUEST(1)",
            "UTL_HTTP.REQUEST('http://x')",
            "pg_catalog.setval('s',1)",
            "NEXTVAL ('s')",
        ];
        for f in all {
            let r = c(&format!("SELECT {f}"));
            assert_eq!((r.kind, r.writes), (K::Call, true), "{f}: {r:?}");
            let r = c(&format!("WITH x AS (SELECT 1) SELECT {f} FROM x"));
            assert!(r.writes, "{f}: {r:?}");
        }
        assert_eq!(c("select nextval('s')").keyword, "NEXTVAL");
    }

    #[test]
    fn functions_that_only_wait_are_reads() {
        check("SELECT SLEEP(1)", K::Select, false, false);
        check("SELECT BENCHMARK(100, 1)", K::Select, false, false);
        check(
            "SELECT DBMS_LOCK.SLEEP(1) FROM dual",
            K::Select,
            false,
            false,
        );
        check("SELECT my_function(1)", K::Select, false, false);
    }

    #[test]
    fn state_function_lookalikes_are_reads() {
        check("SELECT 'setval(' FROM t", K::Select, false, false);
        check("SELECT nextval_col FROM t", K::Select, false, false);
        check("SELECT setval FROM t", K::Select, false, false);
        check("SELECT 1 /* nextval('s') */", K::Select, false, false);
        check(
            "SELECT 1 -- pg_terminate_backend(1)",
            K::Select,
            false,
            false,
        );
        check("SELECT \"nextval\"(1)", K::Select, false, false);
    }

    #[test]
    fn plain_select_is_a_read() {
        check("SELECT * FROM t WHERE a = 1", K::Select, false, false);
        assert_eq!(c("select 1").keyword, "SELECT");
    }

    #[test]
    fn leading_parentheses_and_union() {
        check("((SELECT 1)) UNION (SELECT 2)", K::Select, false, false);
        check("  (  delete from t)", K::Delete, true, true);
    }

    #[test]
    fn comments_are_ignored() {
        check("-- hello\nDELETE FROM t", K::Delete, true, true);
        check(
            "/* x */ update t set a = 1 where id = 1",
            K::Update,
            true,
            false,
        );
        check("# note\nSELECT 1", K::Select, false, false);
        check("SELECT 1 -- DELETE FROM t", K::Select, false, false);
        check(
            "/* a */ /* b */ -- c\n  INSERT INTO t VALUES (1)",
            K::Insert,
            true,
            false,
        );
    }

    #[test]
    fn literals_hide_keywords() {
        check(
            "SELECT 'DELETE FROM t; DROP TABLE x'",
            K::Select,
            false,
            false,
        );
        check("SELECT \"delete\" FROM \"update\"", K::Select, false, false);
        check("SELECT `insert` FROM `merge`", K::Select, false, false);
        check("SELECT [delete] FROM [update]", K::Select, false, false);
        check("SELECT 'it''s INTO' FROM t", K::Select, false, false);
        check("SELECT $$ DELETE FROM t $$", K::Select, false, false);
        check(
            "SELECT $tag$ a $$ INTO b $tag$ FROM t",
            K::Select,
            false,
            false,
        );
        check(
            "SELECT q'[it's INTO DELETE]' FROM dual",
            K::Select,
            false,
            false,
        );
        check("SELECT $1, $2 FROM t", K::Select, false, false);
    }

    #[test]
    fn missing_where_looks_at_top_level_only() {
        check("DELETE FROM t", K::Delete, true, true);
        check("DELETE FROM t WHERE a = 1", K::Delete, true, false);
        check(
            "UPDATE t SET a = (SELECT 1 FROM u WHERE u.id = 1)",
            K::Update,
            true,
            true,
        );
        check(
            "DELETE FROM t WHERE id IN (SELECT id FROM u)",
            K::Delete,
            true,
            false,
        );
        check("UPDATE t SET a = 1 -- WHERE b = 2", K::Update, true, true);
        check("UPDATE t SET a = 'WHERE'", K::Update, true, true);
        check(
            "DELETE t1 FROM t1 JOIN t2 ON t1.id = t2.id",
            K::Delete,
            true,
            true,
        );
        check("DELETE TOP (10) FROM t", K::Delete, true, true);
        check(
            "update t set a = 1 where current of cur",
            K::Update,
            true,
            false,
        );
    }

    #[test]
    fn with_selects_are_reads() {
        check(
            "WITH a AS (SELECT 1) SELECT * FROM a",
            K::Select,
            false,
            false,
        );
        check(
            "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r) SELECT * FROM r",
            K::Select,
            false,
            false,
        );
        check(
            "WITH a AS (SELECT 1), b AS (SELECT 2) SELECT * FROM a, b",
            K::Select,
            false,
            false,
        );
        assert_eq!(c("WITH a AS (SELECT 1) SELECT 1").keyword, "WITH");
    }

    #[test]
    fn with_writes() {
        check(
            "WITH a AS (SELECT 1 x) INSERT INTO t SELECT x FROM a",
            K::Insert,
            true,
            false,
        );
        check(
            "WITH a AS (SELECT 1 x) UPDATE t SET b = 1",
            K::Update,
            true,
            true,
        );
        check(
            "WITH a AS (SELECT id FROM u) DELETE FROM t WHERE id IN (SELECT id FROM a)",
            K::Delete,
            true,
            false,
        );
        check(
            "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
            K::Select,
            true,
            false,
        );
        check("WITH a AS (SELECT 1) FOO", K::Other, true, false);
        check(
            "WITH FUNCTION f RETURN NUMBER IS BEGIN RETURN 1; END; SELECT f FROM dual",
            K::Call,
            true,
            false,
        );
    }

    #[test]
    fn select_into_and_locking_are_writes() {
        check("SELECT * INTO new_t FROM t", K::Select, true, false);
        check("SELECT * INTO #tmp FROM t", K::Select, true, false);
        check(
            "SELECT a FROM t INTO OUTFILE '/tmp/x'",
            K::Select,
            true,
            false,
        );
        check(
            "SELECT * FROM t WHERE id = 1 FOR UPDATE",
            K::Select,
            true,
            false,
        );
        check("SELECT * FROM t FOR UPDATE NOWAIT", K::Select, true, false);
        check("SELECT * FROM t FOR NO KEY UPDATE", K::Select, true, false);
        check("SELECT * FROM t FOR SHARE", K::Select, true, false);
        check("SELECT * FROM t LOCK IN SHARE MODE", K::Select, true, false);
        check("SELECT * FROM #tmp WHERE a = 1", K::Select, false, false);
        check(
            "SELECT SUBSTRING(x FROM 1 FOR 2) FROM t",
            K::Select,
            false,
            false,
        );
        check("SELECT * FROM t FOR XML AUTO", K::Select, false, false);
    }

    #[test]
    fn explain_describe_show() {
        check("EXPLAIN SELECT * FROM t", K::Other, false, false);
        check("EXPLAIN DELETE FROM t", K::Other, false, false);
        check(
            "EXPLAIN FORMAT=JSON UPDATE t SET a = 1",
            K::Other,
            false,
            false,
        );
        check("EXPLAIN PLAN FOR DELETE FROM t", K::Other, false, false);
        check("EXPLAIN ANALYZE SELECT * FROM t", K::Select, false, false);
        check("EXPLAIN ANALYZE DELETE FROM t", K::Delete, true, true);
        check(
            "EXPLAIN (ANALYZE, VERBOSE) UPDATE t SET a = 1 WHERE b = 2",
            K::Update,
            true,
            false,
        );
        check(
            "explain analyse insert into t values (1)",
            K::Insert,
            true,
            false,
        );
        check("EXPLAIN ANALYZE", K::Other, true, false);
        assert_eq!(c("EXPLAIN ANALYZE DELETE FROM t").keyword, "EXPLAIN");
        check("DESCRIBE t", K::Other, false, false);
        check("DESC t", K::Other, false, false);
        check("SHOW TABLES", K::Other, false, false);
        check("show create table t", K::Other, false, false);
    }

    #[test]
    fn ddl() {
        for s in [
            "CREATE TABLE t (id int)",
            "ALTER TABLE t ADD c int",
            "DROP TABLE t",
            "TRUNCATE TABLE t",
            "RENAME TABLE a TO b",
            "COMMENT ON TABLE t IS 'x'",
            "create or replace view v as select 1",
            "ALTER SESSION SET nls_date_format = 'YYYY'",
        ] {
            check(s, K::Ddl, true, false);
        }
    }

    #[test]
    fn calls() {
        check("CALL p(1)", K::Call, true, false);
        check("EXEC sp_who", K::Call, true, false);
        check("EXECUTE IMMEDIATE 'x'", K::Call, true, false);
        check("BEGIN DELETE FROM t; END", K::Call, true, false);
        check("BEGIN\n  p;\nEND;", K::Call, true, false);
        check("BEGIN TRY SELECT 1 END TRY", K::Call, true, false);
        check("DO $$ BEGIN DELETE FROM t; END $$", K::Call, true, false);
        check("DECLARE x int; BEGIN NULL; END;", K::Call, true, false);
    }

    #[test]
    fn transactions() {
        for s in [
            "BEGIN",
            "BEGIN;",
            "BEGIN TRANSACTION",
            "BEGIN TRAN",
            "BEGIN WORK",
            "BEGIN READ ONLY",
            "BEGIN ISOLATION LEVEL SERIALIZABLE",
            "COMMIT",
            "ROLLBACK TO SAVEPOINT a",
            "START TRANSACTION",
            "SAVEPOINT a",
            "RELEASE SAVEPOINT a",
            "SET TRANSACTION READ ONLY",
        ] {
            check(s, K::Transaction, false, false);
        }
    }

    #[test]
    fn set_and_use() {
        check("SET NAMES utf8mb4", K::Other, false, false);
        check("SET @a = 1", K::Other, false, false);
        check("SET search_path TO public", K::Other, false, false);
        check("SET SESSION sql_mode = ''", K::Other, false, false);
        check("SET NOCOUNT ON", K::Other, false, false);
        check("USE shop", K::Other, false, false);
        check("SET GLOBAL max_connections = 10", K::Other, true, false);
        check("SET @@global.sql_mode = ''", K::Other, true, false);
        check("SET PERSIST x = 1", K::Other, true, false);
        check("SET PASSWORD = 'x'", K::Other, true, false);
        check("SET DEFAULT ROLE ALL TO u", K::Other, true, false);
    }

    #[test]
    fn unknown_keywords_are_writes() {
        check("VACUUM", K::Other, true, false);
        check("PRAGMA table_info(t)", K::Other, false, false);
        check("pragma main.index_list(\"t\")", K::Other, false, false);
        check("PRAGMA journal_mode = WAL", K::Other, true, false);
        check("PRAGMA writable_schema", K::Other, true, false);
        check("LOAD DATA INFILE 'x' INTO TABLE t", K::Other, true, false);
        check("LOCK TABLES t WRITE", K::Other, true, false);
        check("FLUSH PRIVILEGES", K::Other, true, false);
        check("FOO bar", K::Other, true, false);
        check("@x", K::Other, true, false);
        check("COPY t FROM '/x'", K::Other, true, false);
        assert_eq!(c("vacuum full").keyword, "VACUUM");
    }

    #[test]
    fn dml_and_grants() {
        check("INSERT INTO t VALUES (1)", K::Insert, true, false);
        check("REPLACE INTO t VALUES (1)", K::Insert, true, false);
        check(
            "INSERT ALL INTO a VALUES (1) INTO b VALUES (2) SELECT 1 FROM dual",
            K::Insert,
            true,
            false,
        );
        check(
            "INSERT INTO t VALUES (1) ON CONFLICT DO UPDATE SET a = 1",
            K::Insert,
            true,
            false,
        );
        check(
            "MERGE INTO t USING s ON (t.id = s.id) WHEN MATCHED THEN DELETE",
            K::Merge,
            true,
            false,
        );
        check("GRANT SELECT ON t TO u", K::Grant, true, false);
        check("REVOKE ALL ON t FROM u", K::Grant, true, false);
        check("DENY SELECT ON t TO u", K::Grant, true, false);
    }

    #[test]
    fn unsure_input_is_a_write() {
        check("SELECT 'abc", K::Select, true, false);
        check("SELECT 1 /* never closed", K::Select, true, false);
        check("SELECT 'a\\'; DELETE FROM t", K::Select, true, false);
        check("SELECT 'a\\\\' FROM t", K::Select, false, false);
        check("/*! DELETE FROM t */", K::Other, true, false);
        check("SELECT 1 /*!50000 , 2 */", K::Select, true, false);
        check("SELECT 1 /* a /* b */ c */", K::Select, true, false);
        check("SELECT $x$ never closed", K::Select, true, false);
        check("SELECT /*+ INDEX(t i) */ * FROM t", K::Select, false, false);
    }

    #[test]
    fn empty_input() {
        let r = c("   -- nothing\n  ");
        assert_eq!(
            (r.kind, r.writes, r.keyword.as_str()),
            (K::Other, false, "")
        );
        assert!(c("()").writes);
    }

    #[test]
    fn script() {
        let s = "SELECT 1; DELETE FROM t; -- c\nUPDATE t SET a = 1 WHERE b = 2;;\nUSE x";
        let r = classify_script(s);
        let kinds: Vec<_> = r
            .iter()
            .map(|x| (x.kind, x.writes, x.missing_where))
            .collect();
        assert_eq!(
            kinds,
            [
                (K::Select, false, false),
                (K::Delete, true, true),
                (K::Update, true, false),
                (K::Other, false, false)
            ]
        );
        assert!(classify_script("").is_empty());
        let d = classify_script(
            "DELIMITER //\nCREATE PROCEDURE p() BEGIN DELETE FROM t; END//\nDELIMITER ;\nSELECT 1",
        );
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].kind, K::Ddl);
        assert_eq!(d[1].kind, K::Select);
    }

    #[test]
    fn multibyte_text() {
        check(
            "SELECT '日本語', 名前 FROM 表 WHERE a = 'é'",
            K::Select,
            false,
            false,
        );
        check("DELETE FROM 表 -- 全部", K::Delete, true, true);
    }

    #[test]
    fn mixed_dialect_samples() {
        // PostgreSQL
        check(
            "SELECT a::int, b->>'x' FROM t WHERE c #> '{a}' = '1'",
            K::Select,
            false,
            false,
        );
        check(
            "DELETE FROM t USING u WHERE t.id = u.id RETURNING *",
            K::Delete,
            true,
            false,
        );
        // Oracle
        check(
            "SELECT * FROM dual WHERE ROWNUM <= 1",
            K::Select,
            false,
            false,
        );
        check("UPDATE emp SET sal = sal * 2", K::Update, true, true);
        // SQL Server
        check(
            "SELECT TOP 5 * FROM [dbo].[t] WITH (NOLOCK)",
            K::Select,
            false,
            false,
        );
        check("DELETE FROM #tmp", K::Delete, true, true);
        // MySQL
        check(
            "UPDATE t SET a = 1 ORDER BY id LIMIT 1",
            K::Update,
            true,
            true,
        );
        check("INSERT INTO t SET a = 1", K::Insert, true, false);
    }
}
