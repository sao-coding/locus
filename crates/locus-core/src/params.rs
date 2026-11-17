//! Query parameters: finding the placeholders of a statement per engine and
//! turning a statement plus typed values into `?` SQL plus bound values.
//! Values are never spliced into the text, so the Production classifier and
//! the audit still see the statement the user wrote.
//!
//! Placeholder styles by engine:
//!
//! | Engine | Detected |
//! |---|---|
//! | MySQL / MariaDB | `?`, `:name` |
//! | PostgreSQL | `$1`, `:name` (`?` is a jsonb operator there) |
//! | Oracle | `:name`, `:1`, `&name` / `&&name` (a PL/SQL block is left alone) |
//! | SQLite | `?`, `?NNN`, `:name`, `@name`, `$name` |
//!
//! `@name` is a user variable on MySQL, so it is only a parameter on SQLite.
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

/// Placeholders of `sql` in order of appearance.
pub fn detect(sql: &str, kind: DbKind) -> Vec<Placeholder> {
    if matches!(kind, DbKind::Redis) {
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
    let push = |out: &mut Vec<Placeholder>, name: &str, r: Range<usize>| {
        out.push(Placeholder {
            name: name.to_string(),
            range: r,
        })
    };
    while i < b.len() {
        match b[i] {
            b'\'' => i = skip_quoted(b, i, b'\'', mysql),
            b'"' => i = skip_quoted(b, i, b'"', mysql),
            b'`' if mysql || kind == DbKind::Sqlite => i = skip_quoted(b, i, b'`', false),
            b'[' if kind == DbKind::Sqlite => {
                i = b[i..]
                    .iter()
                    .position(|x| *x == b']')
                    .map_or(b.len(), |p| i + p + 1)
            }
            b'-' if b.get(i + 1) == Some(&b'-')
                && (!mysql || b.get(i + 2).is_none_or(|x| x.is_ascii_whitespace())) =>
            {
                i = b[i..]
                    .iter()
                    .position(|x| *x == b'\n')
                    .map_or(b.len(), |p| i + p + 1)
            }
            b'#' if mysql => {
                i = b[i..]
                    .iter()
                    .position(|x| *x == b'\n')
                    .map_or(b.len(), |p| i + p + 1)
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = sql[i + 2..].find("*/").map_or(b.len(), |p| i + 2 + p + 2)
            }
            b'q' | b'Q'
                if kind == DbKind::Oracle && b.get(i + 1) == Some(&b'\'') && !prev_word(sql, i) =>
            {
                i = skip_q_quote(sql, i)
            }
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
                } else {
                    i += 1;
                }
            }
            b'$' | b'@' if kind == DbKind::Sqlite => {
                let n = name_len(&sql[i + 1..]);
                if n > 0 && !prev_word(sql, i) {
                    push(&mut out, &sql[i + 1..i + 1 + n], i..i + 1 + n);
                    i += 1 + n;
                } else {
                    i += 1;
                }
            }
            b'?' if mysql || kind == DbKind::Sqlite => {
                let mut j = i + 1;
                if kind == DbKind::Sqlite {
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
                if n > 0 && ok_start && (!digits || matches!(kind, DbKind::Oracle | DbKind::Sqlite))
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
}
