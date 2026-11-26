//! Query parameters: finding the placeholders of a statement per engine and
//! turning a statement plus typed values into `?` SQL plus bound values.
//! Values are never spliced into the text, so the Production classifier and
//! the audit still see the statement the user wrote. (Client-side variables,
//! `${name}`, are the opposite: text substitution, see `vars`.)
//!
//! Placeholder styles by engine (default settings):
//!
//! | Engine | Detected |
//! |---|---|
//! | MySQL / MariaDB | `?`, `:name` |
//! | PostgreSQL | `$1`, `:name` (`?` is a jsonb operator there) |
//! | Oracle | `:name`, `:1`, `&name` / `&&name` (a PL/SQL block is left alone) |
//! | SQLite | `?`, `?NNN`, `:name`, `@name`, `$name` |
//! | SQL Server | `?`, `:name` (`@name` is a T-SQL variable, not a parameter) |
//!
//! `ParamOpts` (Settings › Queries) switches `:name`, `?`, and, on the engines
//! where they are not native, `@name` and `$name` on or off, like DBeaver's
//! "Named parameter prefix". `@name` stays an engine variable where the script
//! assigns it itself (see `detect_with`).
//! Strings, quoted identifiers, comments, dollar quotes, `::` casts and `:=`
//! assignments never count. A `:` right after a word character (`a[1:n]`,
//! `12:30`) is not a placeholder either.

use crate::store::DbKind;
use db_core::Param;
use std::collections::HashMap;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placeholder {
    /// Name without the marker: `id` for `:id`, `1` for `$1`, `?1` / `?2`
    /// (by position) for a bare `?`.
    pub name: String,
    /// Byte range of the whole placeholder in the statement.
    pub range: Range<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ParamType {
    #[default]
    Text,
    Number,
    Date,
    Null,
}

impl ParamType {
    pub const ALL: [ParamType; 4] = [
        ParamType::Text,
        ParamType::Number,
        ParamType::Date,
        ParamType::Null,
    ];

    pub fn label_key(self) -> &'static str {
        match self {
            ParamType::Text => "qp.t_text",
            ParamType::Number => "qp.t_number",
            ParamType::Date => "qp.t_date",
            ParamType::Null => "qp.t_null",
        }
    }

    pub fn next(self) -> ParamType {
        let i = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParamValue {
    pub ty: ParamType,
    pub text: String,
}

fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Which placeholder styles count (Settings › Queries). The default is what
/// Locus always did: `:name` and anonymous `?`, plus each engine's own styles
/// (`$1` on PostgreSQL, `&name` on Oracle, `@name` / `$name` on SQLite).
///
/// `at` and `dollar` add `@name` / `$name` on the other engines, like
/// DBeaver's "Named parameter prefix" setting. A variable of the engine itself
/// stays untouched; see `detect_with`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParamOpts {
    /// Master switch: off, nothing is a parameter.
    pub enabled: bool,
    /// `:name`.
    pub colon: bool,
    /// `@name` on engines where it is not native.
    pub at: bool,
    /// `$name` on engines where it is not native.
    pub dollar: bool,
    /// Anonymous `?` (where the engine has it).
    pub anon: bool,
}

impl Default for ParamOpts {
    fn default() -> Self {
        ParamOpts {
            enabled: true,
            colon: true,
            at: false,
            dollar: false,
            anon: true,
        }
    }
}

/// Placeholders of `sql` in order of appearance, with the default options.
pub fn detect(sql: &str, kind: DbKind) -> Vec<Placeholder> {
    detect_with(sql, kind, &ParamOpts::default())
}

/// If a quoted run, identifier or comment starts at `i`, the index after it.
fn skip_region(sql: &str, i: usize, kind: DbKind) -> Option<usize> {
    let b = sql.as_bytes();
    let mysql = kind == DbKind::Mysql;
    let line_end = |from: usize| {
        b[from..]
            .iter()
            .position(|x| *x == b'\n')
            .map_or(b.len(), |p| from + p + 1)
    };
    Some(match b[i] {
        b'\'' => skip_quoted(b, i, b'\'', mysql),
        b'"' => skip_quoted(b, i, b'"', mysql),
        b'`' if mysql || kind == DbKind::Sqlite => skip_quoted(b, i, b'`', false),
        b'[' if matches!(kind, DbKind::Sqlite | DbKind::SqlServer) => b[i..]
            .iter()
            .position(|x| *x == b']')
            .map_or(b.len(), |p| i + p + 1),
        b'-' if b.get(i + 1) == Some(&b'-')
            && (!mysql || b.get(i + 2).is_none_or(|x| x.is_ascii_whitespace())) =>
        {
            line_end(i)
        }
        b'#' if mysql => line_end(i),
        b'/' if b.get(i + 1) == Some(&b'*') => {
            sql[i + 2..].find("*/").map_or(b.len(), |p| i + 2 + p + 2)
        }
        b'q' | b'Q'
            if kind == DbKind::Oracle && b.get(i + 1) == Some(&b'\'') && !prev_word(sql, i) =>
        {
            skip_q_quote(sql, i)
        }
        _ => return None,
    })
}

/// Placeholders of `sql` in order of appearance.
///
/// `@name` outside SQLite (only with `opts.at`):
/// - MySQL / MariaDB and SQL Server: a name that the script itself assigns or
///   declares (`SET @n =`, `@n :=`, `SELECT .. INTO @n`, `DECLARE @n`,
///   `SELECT @n = ..`) is a session / T-SQL variable and stays untouched;
///   `@@system` variables never count.
/// - PostgreSQL: `@` is an operator (`@>`, `@@`, `@ x`); only `@` directly
///   followed by an identifier character counts.
/// - Oracle: `@file` at the start of a line is a SQL*Plus script call.
pub fn detect_with(sql: &str, kind: DbKind, opts: &ParamOpts) -> Vec<Placeholder> {
    if !opts.enabled || matches!(kind, DbKind::Redis) {
        return Vec::new();
    }
    if kind == DbKind::Oracle && is_plsql(sql) {
        return Vec::new();
    }
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut qmarks = 0;
    let mut i = 0;
    let mysql = kind == DbKind::Mysql;
    let sqlite = kind == DbKind::Sqlite;
    let own_vars = if opts.at && matches!(kind, DbKind::Mysql | DbKind::SqlServer) {
        script_vars(sql, kind)
    } else {
        Default::default()
    };
    let push = |out: &mut Vec<Placeholder>, name: &str, r: Range<usize>| {
        out.push(Placeholder {
            name: name.to_string(),
            range: r,
        })
    };
    while i < b.len() {
        if let Some(next) = skip_region(sql, i, kind) {
            i = next;
            continue;
        }
        match b[i] {
            b'$' if kind == DbKind::Postgres => {
                // `$1` parameter, or a dollar quote `$tag$ ... $tag$`.
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                if j > i + 1 && !prev_word(sql, i) {
                    push(&mut out, &sql[i + 1..j], i..j);
                    i = j;
                    continue;
                }
                let mut j = i + 1;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                if b.get(j) == Some(&b'$') && !prev_word(sql, i) {
                    let tag = &sql[i..=j];
                    i = sql[j + 1..]
                        .find(tag)
                        .map_or(b.len(), |p| j + 1 + p + tag.len());
                } else if opts.dollar && j > i + 1 && !prev_word(sql, i) {
                    push(&mut out, &sql[i + 1..j], i..j);
                    i = j;
                } else {
                    i += 1;
                }
            }
            b'$' if sqlite || opts.dollar => {
                let n = name_len(&sql[i + 1..]);
                if n > 0 && !prev_word(sql, i) {
                    push(&mut out, &sql[i + 1..i + 1 + n], i..i + 1 + n);
                    i += 1 + n;
                } else {
                    i += 1;
                }
            }
            b'@' if sqlite || opts.at => {
                if b.get(i + 1) == Some(&b'@') {
                    i += 2; // `@@system` variable, `@@` operator
                    continue;
                }
                let n = name_len(&sql[i + 1..]);
                let name = &sql[i + 1..i + 1 + n];
                let line_start = sql[..i]
                    .rsplit('\n')
                    .next()
                    .is_some_and(|l| l.trim().is_empty());
                let own = !sqlite
                    && (own_vars.contains(&name.to_ascii_lowercase())
                        || (kind == DbKind::Oracle && line_start));
                if n > 0 && !prev_word(sql, i) && !own {
                    push(&mut out, name, i..i + 1 + n);
                    i += 1 + n;
                } else {
                    i += 1;
                }
            }
            b'?' if opts.anon && (mysql || matches!(kind, DbKind::Sqlite | DbKind::SqlServer)) => {
                let mut j = i + 1;
                if sqlite {
                    while j < b.len() && b[j].is_ascii_digit() {
                        j += 1;
                    }
                }
                if j > i + 1 {
                    push(&mut out, &sql[i..j], i..j);
                } else {
                    qmarks += 1;
                    push(&mut out, &format!("?{qmarks}"), i..i + 1);
                }
                i = j;
            }
            b':' => {
                if b.get(i + 1) == Some(&b':') {
                    i += 2; // a cast `::type`
                    continue;
                }
                let n = name_len(&sql[i + 1..]);
                let ok_start = !prev_word(sql, i) && !prev_is(sql, i, b']');
                let digits = sql[i + 1..i + 1 + n].chars().all(|c| c.is_ascii_digit());
                // `:1` is positional on Oracle / SQLite only.
                if opts.colon
                    && n > 0
                    && ok_start
                    && (!digits || matches!(kind, DbKind::Oracle | DbKind::Sqlite))
                {
                    push(&mut out, &sql[i + 1..i + 1 + n], i..i + 1 + n);
                    i += 1 + n;
                } else {
                    i += 1;
                }
            }
            b'&' if kind == DbKind::Oracle => {
                let amp = if b.get(i + 1) == Some(&b'&') { 2 } else { 1 };
                let n = name_len(&sql[i + amp..]);
                if n > 0 {
                    push(&mut out, &sql[i + amp..i + amp + n], i..i + amp + n);
                    i += amp + n;
                } else {
                    i += amp;
                }
            }
            _ => i += sql[i..].chars().next().map_or(1, char::len_utf8),
        }
    }
    out
}

/// Names (lower-case, no `@`) that the script assigns or declares itself on
/// MySQL / SQL Server: those `@name` are session variables, not parameters.
fn script_vars(sql: &str, kind: DbKind) -> std::collections::HashSet<String> {
    let b = sql.as_bytes();
    let mut set = std::collections::HashSet::new();
    let mut i = 0;
    while i < b.len() {
        if let Some(next) = skip_region(sql, i, kind) {
            i = next;
            continue;
        }
        if b[i] != b'@' {
            i += sql[i..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        if b.get(i + 1) == Some(&b'@') {
            i += 2;
            continue;
        }
        let n = name_len(&sql[i + 1..]);
        let end = i + 1 + n;
        if n > 0 && !prev_word(sql, i) {
            let after = sql[end..].trim_start();
            let before = sql[..i].trim_end();
            let prev_w = before
                .rsplit(|c: char| !word(c))
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            let head = before[before.rfind(';').map_or(0, |p| p + 1)..].to_ascii_uppercase();
            let in_list = before.ends_with(',');
            let assign = after.starts_with(":=")
                || (after.starts_with('=')
                    && !after.starts_with("==")
                    && (prev_w == "SET"
                        || (prev_w == "SELECT" && kind == DbKind::SqlServer)
                        || (in_list
                            && (head.contains("SET ")
                                || (head.contains("SELECT ") && kind == DbKind::SqlServer)))));
            let declared = matches!(prev_w.as_str(), "DECLARE" | "INTO")
                || (in_list
                    && (head.contains("DECLARE ")
                        || (head.contains("INTO ") && !head.contains("FROM "))));
            if assign || declared {
                set.insert(sql[i + 1..end].to_ascii_lowercase());
            }
        }
        i = end.max(i + 1);
    }
    set
}

/// Length in bytes of the identifier at the start of `s`.
fn name_len(s: &str) -> usize {
    s.chars().take_while(|c| word(*c)).map(char::len_utf8).sum()
}

fn prev_word(sql: &str, i: usize) -> bool {
    sql[..i].chars().next_back().is_some_and(word)
}

fn prev_is(sql: &str, i: usize, c: u8) -> bool {
    i > 0 && sql.as_bytes()[i - 1] == c
}

/// Index after the quoted run that starts at `i`. A doubled quote stays
/// inside; `backslash` (MySQL strings) escapes the next byte.
fn skip_quoted(b: &[u8], i: usize, q: u8, backslash: bool) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        if backslash && b[j] == b'\\' && q != b'`' {
            j += 2;
            continue;
        }
        if b[j] == q {
            if b.get(j + 1) == Some(&q) {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    b.len()
}

/// `q'[...]'` style literal starting at `i` (the `q`).
fn skip_q_quote(sql: &str, i: usize) -> usize {
    let rest = &sql[i + 2..];
    let Some(open) = rest.chars().next() else {
        return sql.len();
    };
    let close = match open {
        '[' => ']',
        '(' => ')',
        '{' => '}',
        '<' => '>',
        c => c,
    };
    let body = open.len_utf8();
    let end = format!("{close}'");
    rest[body..]
        .find(&end)
        .map_or(sql.len(), |p| i + 2 + body + p + end.len())
}

/// A PL/SQL block or a stored unit definition: its `:new`, `:old` and bind
/// names belong to the block, not to the run dialog.
fn is_plsql(sql: &str) -> bool {
    let up: String = strip_lead(sql)
        .chars()
        .take(120)
        .collect::<String>()
        .to_ascii_uppercase();
    let first = up.split_whitespace().next().unwrap_or("");
    if matches!(first, "BEGIN" | "DECLARE") {
        return true;
    }
    first == "CREATE"
        && up
            .split_whitespace()
            .any(|w| matches!(w, "FUNCTION" | "PROCEDURE" | "PACKAGE" | "TRIGGER" | "TYPE"))
}

fn strip_lead(sql: &str) -> &str {
    let mut s = sql;
    loop {
        let t = s.trim_start();
        if let Some(r) = t.strip_prefix("--") {
            s = r.split_once('\n').map_or("", |x| x.1);
        } else if let Some(r) = t.strip_prefix("/*") {
            s = r.split_once("*/").map_or("", |x| x.1);
        } else {
            return t;
        }
    }
}

/// Distinct parameter names in order of first appearance: what the dialog
/// shows one row for.
pub fn names(phs: &[Placeholder]) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for p in phs {
        if !v.contains(&p.name) {
            v.push(p.name.clone());
        }
    }
    v
}

/// `sql` with every placeholder replaced by `?` and the values in the same
/// order (a name used twice sends its value twice). A missing value binds NULL.
pub fn bind(
    sql: &str,
    phs: &[Placeholder],
    values: &HashMap<String, ParamValue>,
) -> (String, Vec<Param>) {
    let mut out = String::with_capacity(sql.len());
    let mut params = Vec::with_capacity(phs.len());
    let mut at = 0;
    for p in phs {
        out.push_str(&sql[at..p.range.start]);
        out.push('?');
        at = p.range.end;
        params.push(match values.get(&p.name) {
            None => Param::Null,
            Some(v) if v.ty == ParamType::Null => Param::Null,
            Some(v) => Param::Text(v.text.clone()),
        });
    }
    out.push_str(&sql[at..]);
    (out, params)
}

/// Whether a Number value parses (the dialog marks the row otherwise).
pub fn number_ok(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.parse::<f64>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(sql: &str, k: DbKind) -> Vec<String> {
        detect(sql, k).into_iter().map(|p| p.name).collect()
    }

    #[test]
    fn sqlserver_placeholders() {
        let k = DbKind::SqlServer;
        assert_eq!(
            n("select * from [a?b] where x = ? and y = :y", k),
            ["?1", "y"]
        );
        assert!(n("declare @x int = 1; select @x from #t", k).is_empty());
    }

    #[test]
    fn mysql_qmarks_and_named() {
        assert_eq!(
            n("select * from t where a = ? and b = ?", DbKind::Mysql),
            ["?1", "?2"]
        );
        assert_eq!(
            n("select * from t where a = :id and b = :name", DbKind::Mysql),
            ["id", "name"]
        );
    }

    #[test]
    fn mysql_user_variables_are_not_params() {
        assert!(n("select @a := 1, @@global.x, @b", DbKind::Mysql).is_empty());
        assert!(n("set @x = 5", DbKind::Mysql).is_empty());
        assert!(n("select @a:=1", DbKind::Mysql).is_empty());
    }

    #[test]
    fn strings_comments_idents_skipped() {
        let sql = "select '?', \"?\", `?`, 'it''s :x' -- ? :c\n, /* :d ? */ 1 # ?\n, :real";
        assert_eq!(n(sql, DbKind::Mysql), ["real"]);
        assert_eq!(n("select 'a\\'?' , ?", DbKind::Mysql), ["?1"]);
    }

    #[test]
    fn mysql_double_dash_needs_space() {
        assert_eq!(n("select 1--?\n", DbKind::Mysql), ["?1"]);
    }

    #[test]
    fn postgres_numbered_and_casts() {
        assert_eq!(
            n("select $1::int, $2, $1", DbKind::Postgres),
            ["1", "2", "1"]
        );
        assert!(n("select a::text from t where b ? 'k'", DbKind::Postgres).is_empty());
        assert!(n("select a[1:3], '12:30'::time, x::int", DbKind::Postgres).is_empty());
        assert_eq!(
            n("select * from t where id = :id::int", DbKind::Postgres),
            ["id"]
        );
    }

    #[test]
    fn postgres_dollar_quotes_skipped() {
        assert_eq!(
            n(
                "do $$ begin perform :x; end $$; select $1",
                DbKind::Postgres
            ),
            ["1"]
        );
        assert_eq!(n("select $tag$ $1 $tag$, $2", DbKind::Postgres), ["2"]);
        assert!(n("select 'cost $1'", DbKind::Postgres).is_empty());
        assert!(n("select a$1 from t", DbKind::Postgres).is_empty());
    }

    #[test]
    fn oracle_colon_and_ampersand() {
        assert_eq!(
            n(
                "select * from t where a = :a and b = :1 and c = &c and d = &&d",
                DbKind::Oracle
            ),
            ["a", "1", "c", "d"]
        );
        assert!(n("select 'a&b', 'x:y' from dual where a && b", DbKind::Oracle).is_empty());
        assert!(
            n(
                "select to_char(sysdate, 'hh24:mi') from dual",
                DbKind::Oracle
            )
            .is_empty()
        );
    }

    #[test]
    fn oracle_plsql_ignored() {
        assert!(n("begin :x := 1; end;", DbKind::Oracle).is_empty());
        assert!(n("declare v number; begin v := :a; end;", DbKind::Oracle).is_empty());
        assert!(n("-- c\ncreate or replace trigger t before insert on x for each row begin :new.a := 1; end;", DbKind::Oracle).is_empty());
        assert!(n("create or replace package body p is end;", DbKind::Oracle).is_empty());
        assert_eq!(
            n(
                "create table t as select * from u where a = :a",
                DbKind::Oracle
            ),
            ["a"]
        );
    }

    #[test]
    fn oracle_q_quote_and_assign() {
        assert_eq!(
            n("select q'[it's :no]', :yes from dual", DbKind::Oracle),
            ["yes"]
        );
        assert!(n("select 1 from dual where x:=1", DbKind::Oracle).is_empty());
        assert_eq!(n("select seq.nextval, :a from dual", DbKind::Oracle), ["a"]);
    }

    #[test]
    fn sqlite_styles() {
        assert_eq!(
            n("select ?, ?2, :a, @b, $c", DbKind::Sqlite),
            ["?1", "?2", "a", "b", "c"]
        );
        assert!(n("select 'a?', \"b?\", [c?] from t -- ?", DbKind::Sqlite).is_empty());
        assert!(n("select json_extract(j, '$.a') from t", DbKind::Sqlite).is_empty());
    }

    #[test]
    fn redis_none() {
        assert!(n("GET :a", DbKind::Redis).is_empty());
    }

    #[test]
    fn unicode_does_not_panic() {
        assert_eq!(
            n(
                "select '測試', 名稱 from t where a = :名稱 and b = ?",
                DbKind::Mysql
            ),
            ["名稱", "?1"]
        );
    }

    #[test]
    fn names_distinct() {
        let p = detect("select :a, :b, :a", DbKind::Mysql);
        assert_eq!(names(&p), ["a", "b"]);
    }

    #[test]
    fn bind_rewrites_and_repeats() {
        let sql = "select * from t where a = :a or b = :b or c = :a";
        let phs = detect(sql, DbKind::Mysql);
        let mut v = HashMap::new();
        v.insert(
            "a".to_string(),
            ParamValue {
                ty: ParamType::Number,
                text: "7".into(),
            },
        );
        v.insert(
            "b".to_string(),
            ParamValue {
                ty: ParamType::Null,
                text: "x".into(),
            },
        );
        let (q, p) = bind(sql, &phs, &v);
        assert_eq!(q, "select * from t where a = ? or b = ? or c = ?");
        assert_eq!(
            p,
            vec![
                Param::Text("7".into()),
                Param::Null,
                Param::Text("7".into())
            ]
        );
    }

    #[test]
    fn bind_keeps_unicode_and_strings() {
        let sql = "select '?:x', 名 from t where a = ?";
        let phs = detect(sql, DbKind::Mysql);
        let (q, p) = bind(sql, &phs, &HashMap::new());
        assert_eq!(q, sql);
        assert_eq!(p, vec![Param::Null]);
    }

    #[test]
    fn postgres_bind_dollar() {
        let sql = "select $2, $1, $2";
        let phs = detect(sql, DbKind::Postgres);
        let mut v = HashMap::new();
        v.insert(
            "1".to_string(),
            ParamValue {
                ty: ParamType::Text,
                text: "a".into(),
            },
        );
        v.insert(
            "2".to_string(),
            ParamValue {
                ty: ParamType::Text,
                text: "b".into(),
            },
        );
        let (q, p) = bind(sql, &phs, &v);
        assert_eq!(q, "select ?, ?, ?");
        assert_eq!(
            p,
            vec![
                Param::Text("b".into()),
                Param::Text("a".into()),
                Param::Text("b".into())
            ]
        );
    }

    #[test]
    fn number_check() {
        assert!(number_ok("1.5") && number_ok(" -3 ") && !number_ok("") && !number_ok("abc"));
    }

    // ---- configurable prefixes (ParamOpts) ----------------------------------

    fn o(colon: bool, at: bool, dollar: bool, anon: bool) -> ParamOpts {
        ParamOpts {
            enabled: true,
            colon,
            at,
            dollar,
            anon,
        }
    }

    fn nw(sql: &str, k: DbKind, opts: ParamOpts) -> Vec<String> {
        detect_with(sql, k, &opts)
            .into_iter()
            .map(|p| p.name)
            .collect()
    }

    const ALL_KINDS: [DbKind; 5] = [
        DbKind::Mysql,
        DbKind::Postgres,
        DbKind::Oracle,
        DbKind::Sqlite,
        DbKind::SqlServer,
    ];

    #[test]
    fn defaults_are_todays_behaviour() {
        assert_eq!(ParamOpts::default(), o(true, false, false, true));
        // `@name` / `$name` stay off outside SQLite.
        for k in [
            DbKind::Mysql,
            DbKind::Postgres,
            DbKind::Oracle,
            DbKind::SqlServer,
        ] {
            assert!(n("select @a, $b from t", k).is_empty(), "{k:?}");
        }
    }

    #[test]
    fn master_switch_off_disables_everything() {
        let off = ParamOpts {
            enabled: false,
            ..o(true, true, true, true)
        };
        for k in ALL_KINDS {
            assert!(nw("select :a, ?, @b, $1, &c", k, off).is_empty(), "{k:?}");
        }
    }

    #[test]
    fn colon_prefix_toggle() {
        for k in ALL_KINDS {
            assert_eq!(
                nw("select :a", k, o(true, false, false, true)),
                ["a"],
                "{k:?}"
            );
            assert!(
                nw("select :a", k, o(false, false, false, true)).is_empty(),
                "{k:?}"
            );
        }
        assert_eq!(
            nw("select ?, :a", DbKind::Mysql, o(false, false, false, true)),
            ["?1"]
        );
        assert_eq!(
            nw("select ?, :a", DbKind::Mysql, o(true, false, false, false)),
            ["a"]
        );
    }

    #[test]
    fn anon_switch_only_where_the_engine_has_it() {
        let off = o(true, false, false, false);
        for k in [DbKind::Mysql, DbKind::Sqlite, DbKind::SqlServer] {
            assert_eq!(n("select ?", k), ["?1"], "{k:?}");
            assert!(nw("select ?", k, off).is_empty(), "{k:?}");
        }
        assert!(n("select a ? 'k'", DbKind::Postgres).is_empty());
        assert!(n("select ?", DbKind::Oracle).is_empty());
        // Postgres `$1` is the engine's own style, not the `?` switch.
        assert_eq!(nw("select $1", DbKind::Postgres, off), ["1"]);
    }

    #[test]
    fn at_prefix_on_every_engine() {
        let at = o(true, true, false, true);
        for k in ALL_KINDS {
            assert_eq!(nw("select * from t where id = @id", k, at), ["id"], "{k:?}");
        }
        // `@` without the switch: only SQLite.
        let no = o(true, false, false, true);
        for k in [
            DbKind::Mysql,
            DbKind::Postgres,
            DbKind::Oracle,
            DbKind::SqlServer,
        ] {
            assert!(nw("select @id", k, no).is_empty(), "{k:?}");
        }
        assert_eq!(nw("select @id", DbKind::Sqlite, no), ["id"]);
    }

    #[test]
    fn dollar_prefix_on_every_engine() {
        let d = o(true, false, true, true);
        for k in ALL_KINDS {
            assert_eq!(nw("select * from t where id = $id", k, d), ["id"], "{k:?}");
        }
        assert!(nw("select $id", DbKind::Mysql, o(true, false, false, true)).is_empty());
        assert!(nw("select $id", DbKind::Oracle, o(true, false, false, true)).is_empty());
        // Postgres: `$1` and dollar quotes keep their meaning.
        assert_eq!(nw("select $1, $name", DbKind::Postgres, d), ["1", "name"]);
        assert_eq!(nw("select $t$ $x $t$, $y", DbKind::Postgres, d), ["y"]);
        assert_eq!(
            nw("do $$ select $x $$; select $z", DbKind::Postgres, d),
            ["z"]
        );
        // A `$` inside a word is not a parameter (Oracle `v$session`).
        assert!(nw("select * from v$session", DbKind::Oracle, d).is_empty());
        // `${var}` is a client variable, never a `$` parameter.
        assert!(nw("select ${v}", DbKind::Mysql, d).is_empty());
    }

    #[test]
    fn all_prefixes_together() {
        let all = o(true, true, true, true);
        assert_eq!(
            nw("select :a, @b, $c, ?", DbKind::Mysql, all),
            ["a", "b", "c", "?1"]
        );
        assert_eq!(
            nw("select :a, @b, $c, $1", DbKind::Postgres, all),
            ["a", "b", "c", "1"]
        );
    }

    #[test]
    fn at_skips_strings_comments_and_words() {
        let at = o(true, true, true, true);
        let sql = "select '@a', \"@b\", `@c` -- @d\n, /* @e */ x@y, @real";
        assert_eq!(nw(sql, DbKind::Mysql, at), ["real"]);
        assert_eq!(nw("select 'it''s @a', @b", DbKind::Postgres, at), ["b"]);
        assert_eq!(
            nw("select q'[@a]', @b from dual", DbKind::Oracle, at),
            ["b"]
        );
        assert_eq!(nw("select $$ @a $$, @b", DbKind::Postgres, at), ["b"]);
        assert_eq!(nw("select [@a], @b", DbKind::SqlServer, at), ["b"]);
    }

    #[test]
    fn mysql_at_keeps_script_variables() {
        let at = o(true, true, false, true);
        let k = DbKind::Mysql;
        assert!(nw("set @x = 1; select @x", k, at).is_empty());
        assert!(nw("SET @x := 1; select @x + 1", k, at).is_empty());
        assert!(nw("select @a := 1, @a + 1", k, at).is_empty());
        assert!(nw("select count(*) into @n from t; select @n", k, at).is_empty());
        assert!(nw("set @a = 1, @b = 2; select @a, @b", k, at).is_empty());
        // Not assigned in the script: a parameter; the assigned one is not.
        assert_eq!(nw("set @x = 1; select @x, @id", k, at), ["id"]);
        assert_eq!(nw("select * from t where id = @id", k, at), ["id"]);
        // System variables and a comparison are not assignments.
        assert!(nw("select @@global.max_connections, @@version", k, at).is_empty());
        assert_eq!(nw("select @a = 1", k, at), ["a"]);
        // An assignment in a string does not count.
        assert_eq!(nw("select 'set @x = 1', @x", k, at), ["x"]);
        // Case-insensitive names.
        assert!(nw("set @Foo = 1; select @foo", k, at).is_empty());
        assert_eq!(nw("set @x = 1; select @x, :id", k, at), ["id"]);
    }

    #[test]
    fn sqlserver_at_keeps_declared_variables() {
        let at = o(true, true, false, true);
        let k = DbKind::SqlServer;
        assert!(nw("declare @x int = 1; select @x from #t", k, at).is_empty());
        assert!(nw("declare @a int, @b int; set @a = 1; select @a, @b", k, at).is_empty());
        assert!(
            nw(
                "declare @a int; select @a = max(id) from t; select @a",
                k,
                at
            )
            .is_empty()
        );
        assert_eq!(nw("declare @x int = 1; select @x, @id", k, at), ["id"]);
        assert!(nw("select @@rowcount, @@version", k, at).is_empty());
    }

    #[test]
    fn postgres_at_is_an_operator_unless_glued_to_a_name() {
        let at = o(true, true, false, true);
        let k = DbKind::Postgres;
        assert!(nw("select a @> b, c @@ to_tsquery('x'), d @ e from t", k, at).is_empty());
        assert!(nw("select @ x, @ 1, @-@ y", k, at).is_empty());
        assert_eq!(nw("select @ x, @id", k, at), ["id"]);
        assert_eq!(nw("select abs(@id) , @-@ y", k, at), ["id"]);
        assert!(nw("select a@b from t", k, at).is_empty());
        assert!(nw("select a ?| b, c ? 'k'", k, at).is_empty());
    }

    #[test]
    fn oracle_at_line_start_is_a_script_call() {
        let at = o(true, true, false, true);
        let k = DbKind::Oracle;
        assert!(nw("@script.sql", k, at).is_empty());
        assert!(nw("  @c:/dir/file.sql", k, at).is_empty());
        assert!(nw("select 1 from dual;\n@file\nselect 2 from dual", k, at).is_empty());
        assert_eq!(nw("select 1 from dual where x = @id", k, at), ["id"]);
        // A database link is glued to a word: not a parameter.
        assert!(nw("select * from t@remote", k, at).is_empty());
        assert_eq!(nw("select * from t\nwhere a = @id", k, at), ["id"]);
        // PL/SQL blocks are left alone as before.
        assert!(nw("begin x := @id; end;", k, at).is_empty());
    }

    #[test]
    fn sqlite_native_styles_stay_on() {
        // `@name` / `$name` are SQLite's own styles: not tied to the switches.
        let none = o(true, false, false, true);
        assert_eq!(
            nw("select @a, $b, :c, ?", DbKind::Sqlite, none),
            ["a", "b", "c", "?1"]
        );
        assert_eq!(
            nw(
                "select @a, $b, :c",
                DbKind::Sqlite,
                o(false, false, false, false)
            ),
            ["a", "b"]
        );
    }

    #[test]
    fn redis_never() {
        assert!(nw("GET @a :b $c ?", DbKind::Redis, o(true, true, true, true)).is_empty());
    }

    #[test]
    fn unicode_names_with_at() {
        assert_eq!(
            nw(
                "select '測試', @名稱 from t",
                DbKind::Mysql,
                o(true, true, false, true)
            ),
            ["名稱"]
        );
    }

    #[test]
    fn bind_works_for_at_and_dollar() {
        let sql = "select * from t where a = @a and b = $b";
        let phs = detect_with(sql, DbKind::Mysql, &o(true, true, true, true));
        let mut v = HashMap::new();
        v.insert(
            "a".to_string(),
            ParamValue {
                ty: ParamType::Number,
                text: "1".into(),
            },
        );
        let (q, p) = bind(sql, &phs, &v);
        assert_eq!(q, "select * from t where a = ? and b = ?");
        assert_eq!(p, vec![Param::Text("1".into()), Param::Null]);
    }
}
