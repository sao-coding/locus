//! SQL text helpers that do not need a server.

use std::ops::Range;

/// Splits a script into statements and returns the byte range of each one in
/// `sql`, without the terminating `;` and without surrounding whitespace.
/// Empty statements are skipped.
///
/// A `;` does not end a statement inside a string (`'..'`, `".."`), a quoted
/// identifier (`` `..` ``), a line comment (`-- ` or `#` to end of line) or a
/// block comment (`/* .. */`). The MySQL client command `DELIMITER xx` changes
/// the terminator for the statements after it and is not itself returned.
///
/// Comments that sit in front of or inside a statement belong to it; a chunk
/// with nothing but comments and blanks is not a statement.
///
/// PL/SQL: a `CREATE [OR REPLACE] PACKAGE | PACKAGE BODY | PROCEDURE |
/// FUNCTION | TRIGGER | TYPE BODY` (or a `DECLARE` block) whose body starts
/// (a top-level `AS`, `IS`, `BEGIN` or `DECLARE` before the first `;`) is not
/// split on its inner `;`: it ends at a line holding only `/` (the SQL*Plus
/// convention) or at the end of the script. A PostgreSQL body in a dollar
/// quote (`$$ .. $$`, `$tag$ .. $tag$`) is a quoted run, so there the `;`
/// after it ends the statement as usual. A line holding only `/` is never a
/// statement itself.
pub fn split_statements(sql: &str) -> Vec<Range<usize>> {
    let b = sql.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut delim: &str = ";";
    let mut delim_first = b';';
    let mut i = 0;
    // Start of the current chunk (first non-blank byte, comments included)
    // and whether it holds anything besides comments.
    let mut start: Option<usize> = None;
    let mut has_code = false;
    // PL/SQL state of the current chunk: armed by a routine head, `body` once
    // its AS / IS / BEGIN is seen; `dollar` once a dollar quote is seen.
    let mut armed = false;
    let mut body = false;
    let mut dollar = false;

    fn emit(sql: &str, start: usize, end: usize, out: &mut Vec<Range<usize>>) {
        let len = sql[start..end].trim_end().len();
        if len > 0 {
            out.push(start..start + len);
        }
    }

    while i < n {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // A line holding only `/` ends a statement (SQL*Plus).
        if c == b'/' && slash_line(b, i) {
            if has_code {
                emit(sql, start.unwrap_or(i), i, &mut out);
            }
            start = None;
            has_code = false;
            (armed, body, dollar) = (false, false, false);
            i += 1;
            continue;
        }
        // The terminator wins over everything else in normal state.
        if c == delim_first && b[i..].starts_with(delim.as_bytes()) {
            if body && delim == ";" && !dollar {
                // Inside a PL/SQL body: part of the statement.
                i += 1;
                continue;
            }
            if has_code {
                emit(sql, start.unwrap_or(i), i, &mut out);
            }
            start = None;
            has_code = false;
            (armed, body, dollar) = (false, false, false);
            i += delim.len();
            continue;
        }
        // DELIMITER only counts at the start of a statement, at a line start.
        if !has_code && c.eq_ignore_ascii_case(&b'd') && is_delimiter_cmd(b, i) {
            let line_end = sql[i..].find('\n').map_or(n, |p| i + p);
            let arg = sql[i + 9..line_end].trim();
            if !arg.is_empty() {
                delim = arg;
                delim_first = delim.as_bytes()[0];
            }
            start = None;
            i = line_end;
            continue;
        }
        if start.is_none() {
            start = Some(i);
        }
        match c {
            b'#' => i = line_comment_end(sql, i),
            b'-' if b.get(i + 1) == Some(&b'-')
                && b.get(i + 2)
                    .is_none_or(|x| x.is_ascii_whitespace() || *x < 0x20) =>
            {
                i = line_comment_end(sql, i)
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = sql[i + 2..].find("*/").map_or(n, |p| i + 2 + p + 2);
            }
            b'\'' | b'"' | b'`' => {
                has_code = true;
                i = quoted_end(b, i);
            }
            b'$' if (i == 0 || !is_word_byte(b[i - 1])) && dollar_tag_len(b, i).is_some() => {
                let len = dollar_tag_len(b, i).unwrap_or(1);
                let tag = &sql[i..i + len];
                has_code = true;
                dollar = true;
                i = sql[i + len..].find(tag).map_or(n, |p| i + len + p + len);
            }
            c if is_word_start(c) && (i == 0 || !is_word_byte(b[i - 1])) => {
                let mut j = i;
                while j < n && is_word_byte(b[j]) {
                    j += 1;
                }
                if !has_code && delim == ";" {
                    armed = routine_head(sql, i);
                }
                if armed && !body {
                    let w = &sql[i..j];
                    if ["AS", "IS", "BEGIN", "DECLARE"]
                        .iter()
                        .any(|k| w.eq_ignore_ascii_case(k))
                    {
                        body = true;
                    }
                }
                has_code = true;
                i = j;
            }
            _ => {
                has_code = true;
                i += 1;
            }
        }
    }
    if has_code {
        emit(sql, start.unwrap_or(0), n, &mut out);
    }
    out
}

/// `DELIMITER` at `i`, preceded on its line only by blanks, followed by a blank.
fn is_delimiter_cmd(b: &[u8], i: usize) -> bool {
    let word = b"delimiter";
    if b.len() < i + word.len() + 1 || !b[i..i + word.len()].eq_ignore_ascii_case(word) {
        return false;
    }
    if !matches!(b[i + word.len()], b' ' | b'\t') {
        return false;
    }
    let mut j = i;
    while j > 0 {
        match b[j - 1] {
            b'\n' => return true,
            b' ' | b'\t' | b'\r' => j -= 1,
            _ => return false,
        }
    }
    true
}

fn is_word_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

/// `/` at `i` is alone on its line (blanks around it allowed).
fn slash_line(b: &[u8], i: usize) -> bool {
    let blank = |c: &u8| matches!(c, b' ' | b'\t' | b'\r');
    let before = b[..i].iter().rev().take_while(|&&c| c != b'\n').all(blank);
    let after = b[i + 1..].iter().take_while(|&&c| c != b'\n').all(blank);
    before && after
}

/// Length of a dollar-quote opener at `i` (`$$` = 2, `$tag$`), if it is one.
fn dollar_tag_len(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if b.get(j).is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
        j += 1;
    }
    (b.get(j) == Some(&b'$')).then_some(j + 1 - i)
}

/// The words at `i` open a PL/SQL unit: `CREATE [OR REPLACE] [EDITIONABLE |
/// NONEDITIONABLE] PACKAGE | PROCEDURE | FUNCTION | TRIGGER | TYPE BODY`, or `DECLARE`.
pub fn routine_head(sql: &str, i: usize) -> bool {
    let words: Vec<String> = sql[i..]
        .split(|c: char| c.is_whitespace() || c == '(' || c == '"')
        .filter(|w| !w.is_empty())
        .take(6)
        .map(|w| w.to_ascii_uppercase())
        .collect();
    let mut w = words.iter().map(String::as_str);
    match w.next() {
        Some("DECLARE") => return true,
        Some("CREATE") => {}
        _ => return false,
    }
    let mut next = w.next();
    if next == Some("OR") {
        if w.next() != Some("REPLACE") {
            return false;
        }
        next = w.next();
    }
    if matches!(next, Some("EDITIONABLE" | "NONEDITIONABLE")) {
        next = w.next();
    }
    match next {
        Some("PACKAGE" | "PROCEDURE" | "FUNCTION" | "TRIGGER") => true,
        Some("TYPE") => w.next() == Some("BODY"),
        _ => false,
    }
}

fn line_comment_end(sql: &str, i: usize) -> usize {
    sql[i..].find('\n').map_or(sql.len(), |p| i + p)
}

/// Index just past the closing quote of the quoted run that opens at `i`.
fn quoted_end(b: &[u8], i: usize) -> usize {
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() {
        let c = b[j];
        if c == b'\\' && q != b'`' {
            j += 2;
        } else if c == q {
            if b.get(j + 1) == Some(&q) {
                j += 2;
            } else {
                return j + 1;
            }
        } else {
            j += 1;
        }
    }
    b.len()
}

/// The statement that contains byte offset `cursor` (a cursor sitting right
/// after a statement's `;`, or in the blank space after it, belongs to that
/// statement). `None` when the script has no statements.
///
/// A cursor before the first statement selects the first one. When the next
/// statement starts immediately after the terminator (`a;b`), a cursor between
/// them belongs to the earlier statement.
pub fn statement_at(sql: &str, cursor: usize) -> Option<Range<usize>> {
    let ranges = split_statements(sql);
    if ranges.is_empty() {
        return None;
    }
    let cursor = cursor.min(sql.len());
    let idx = ranges
        .partition_point(|r| r.start <= cursor)
        .saturating_sub(1);
    if idx > 0 && cursor == ranges[idx].start {
        let gap = &sql.as_bytes()[ranges[idx - 1].end..cursor];
        if !gap.iter().any(|c| c.is_ascii_whitespace()) {
            return Some(ranges[idx - 1].clone());
        }
    }
    Some(ranges[idx].clone())
}

/// The table a statement reads from, as written (quotes removed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableRef {
    pub schema: Option<String>,
    pub table: String,
}

#[derive(Debug, PartialEq)]
enum Tok {
    /// Bare word (keyword or unquoted identifier), as written.
    Word(String),
    /// Quoted identifier (`..`, "..", [..]), unquoted.
    Quoted(String),
    Punct(u8),
    /// String literal or number: not a name.
    Other,
}

fn tokenize(sql: &str) -> Option<Vec<Tok>> {
    let b = sql.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'#' || (c == b'-' && b.get(i + 1) == Some(&b'-')) {
            i = line_comment_end(sql, i);
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i = sql[i + 2..].find("*/").map(|p| i + 2 + p + 2)?; // unterminated: unsure
        } else if c == b'\'' {
            let e = quoted_end(b, i);
            if e > n || b.get(e - 1) != Some(&b'\'') || e == i + 1 {
                return None;
            }
            out.push(Tok::Other);
            i = e;
        } else if c == b'`' || c == b'"' {
            let e = quoted_end(b, i);
            if e < i + 2 || e > n || b[e - 1] != c {
                return None;
            }
            let q = (c as char).to_string();
            out.push(Tok::Quoted(
                sql[i + 1..e - 1].replace(&format!("{q}{q}"), &q),
            ));
            i = e;
        } else if c == b'[' {
            let mut j = i + 1;
            loop {
                j += sql[j..].find(']')? + 1;
                if b.get(j) == Some(&b']') {
                    j += 1; // ]] is an escaped ]
                } else {
                    break;
                }
            }
            out.push(Tok::Quoted(sql[i + 1..j - 1].replace("]]", "]")));
            i = j;
        } else if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80 {
            let mut j = i;
            while j < n
                && (b[j].is_ascii_alphanumeric()
                    || matches!(b[j], b'_' | b'$' | b'#')
                    || b[j] >= 0x80)
            {
                j += 1;
            }
            let w = &sql[i..j];
            out.push(if c.is_ascii_digit() {
                Tok::Other
            } else {
                Tok::Word(w.to_string())
            });
            i = j;
        } else {
            out.push(Tok::Punct(c));
            i += 1;
        }
    }
    Some(out)
}

fn is_kw(t: &Tok, kw: &str) -> bool {
    matches!(t, Tok::Word(w) if w.eq_ignore_ascii_case(kw))
}

/// The one table a plain `SELECT ... FROM table [alias] [WHERE ...]
/// [ORDER BY ...] [LIMIT / OFFSET / FETCH ...]` reads, so the grid can edit
/// its rows when the server does not report where a column comes from.
///
/// Quoted identifiers (`` `t` ``, `"t"`, `[t]`) and `schema.table` are
/// understood. `None` for everything else, and whenever the text is not
/// clearly of that shape: JOIN, a comma list, a subquery in FROM, UNION /
/// INTERSECT / EXCEPT / MINUS, GROUP BY / HAVING, DISTINCT, aggregates, a
/// CTE, `FOR UPDATE`, several statements, table functions, `FROM ONLY`.
pub fn single_table(sql: &str) -> Option<TableRef> {
    let mut toks = tokenize(sql)?;
    while toks.last() == Some(&Tok::Punct(b';')) {
        toks.pop();
    }
    if toks.contains(&Tok::Punct(b';')) {
        return None;
    }
    if !is_kw(toks.first()?, "SELECT") {
        return None;
    }
    // Select list: up to the first FROM outside parentheses.
    const AGGREGATES: [&str; 9] = [
        "COUNT",
        "SUM",
        "AVG",
        "MIN",
        "MAX",
        "GROUP_CONCAT",
        "LISTAGG",
        "STRING_AGG",
        "ARRAY_AGG",
    ];
    let mut depth = 0i32;
    let mut from = None;
    for (i, t) in toks.iter().enumerate().skip(1) {
        match t {
            Tok::Punct(b'(') => depth += 1,
            Tok::Punct(b')') => depth -= 1,
            _ if is_kw(t, "SELECT") => return None, // scalar subquery
            _ if depth == 0 && is_kw(t, "FROM") => {
                from = Some(i);
                break;
            }
            _ if depth == 0
                && ["DISTINCT", "DISTINCTROW", "UNIQUE", "INTO"]
                    .iter()
                    .any(|k| is_kw(t, k)) =>
            {
                return None;
            }
            // An aggregate: its result is not a table column.
            Tok::Word(w)
                if toks.get(i + 1) == Some(&Tok::Punct(b'('))
                    && AGGREGATES.iter().any(|k| w.eq_ignore_ascii_case(k)) =>
            {
                return None;
            }
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    let mut i = from? + 1;
    let name = |t: Option<&Tok>| -> Option<String> {
        match t? {
            Tok::Quoted(s) => Some(s.clone()),
            Tok::Word(w)
                if !["ONLY", "LATERAL", "SELECT", "TABLE", "VALUES", "UNNEST"]
                    .iter()
                    .any(|k| w.eq_ignore_ascii_case(k)) =>
            {
                Some(w.clone())
            }
            _ => None,
        }
    };
    let first = name(toks.get(i))?;
    i += 1;
    let (schema, table) = if toks.get(i) == Some(&Tok::Punct(b'.')) {
        let second = name(toks.get(i + 1))?;
        i += 2;
        if toks.get(i) == Some(&Tok::Punct(b'.')) {
            return None; // catalog.schema.table
        }
        (Some(first), second)
    } else {
        (None, first)
    };
    if toks.get(i) == Some(&Tok::Punct(b'(')) {
        return None; // table function
    }
    const CLAUSE: [&str; 5] = ["WHERE", "ORDER", "LIMIT", "FETCH", "OFFSET"];
    let is_clause = |t: &Tok| CLAUSE.iter().any(|k| is_kw(t, k));
    // Optional alias: `AS name` or a bare name that is not a clause keyword.
    if toks.get(i).is_some_and(|t| is_kw(t, "AS")) {
        name(toks.get(i + 1))?;
        i += 2;
    } else if let Some(t @ (Tok::Word(_) | Tok::Quoted(_))) = toks.get(i)
        && !is_clause(t)
    {
        i += 1;
    }
    match toks.get(i) {
        None => {}
        Some(t) if is_clause(t) => {}
        Some(_) => return None, // comma, JOIN, GROUP, UNION, hints, ...
    }
    // The rest: nothing that changes what a row is.
    const FORBIDDEN: [&str; 12] = [
        "UNION",
        "INTERSECT",
        "EXCEPT",
        "MINUS",
        "GROUP",
        "HAVING",
        "JOIN",
        "FROM",
        "INTO",
        "FOR",
        "WINDOW",
        "LOCK",
    ];
    let mut depth = 0i32;
    for t in &toks[i..] {
        match t {
            Tok::Punct(b'(') => depth += 1,
            Tok::Punct(b')') => depth -= 1,
            _ if depth == 0 && FORBIDDEN.iter().any(|k| is_kw(t, k)) => return None,
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    if depth != 0 {
        return None;
    }
    Some(TableRef { schema, table })
}

#[cfg(test)]
mod single_table_tests {
    use super::*;

    fn t(schema: Option<&str>, table: &str) -> Option<TableRef> {
        Some(TableRef {
            schema: schema.map(String::from),
            table: table.to_string(),
        })
    }

    #[test]
    fn plain_selects_are_recognised() {
        assert_eq!(single_table("SELECT * FROM student"), t(None, "student"));
        assert_eq!(
            single_table("select id, name from bench.student where id > 3 order by id limit 10;"),
            t(Some("bench"), "student")
        );
        assert_eq!(
            single_table(
                "SELECT s.* FROM `my db`.`we``ird` AS s WHERE s.a = 'x;y' ORDER BY 1 DESC LIMIT 5, 10"
            ),
            t(Some("my db"), "we`ird")
        );
        assert_eq!(
            single_table(r#"SELECT "A", "B" FROM "HR"."EMP" e WHERE ROWNUM <= 5"#),
            t(Some("HR"), "EMP")
        );
        assert_eq!(
            single_table("SELECT * FROM [dbo].[Order Details] WHERE [a]]b] = 1"),
            t(Some("dbo"), "Order Details")
        );
        assert_eq!(
            single_table("SELECT * FROM emp ORDER BY empno OFFSET 5 ROWS FETCH FIRST 10 ROWS ONLY"),
            t(None, "emp")
        );
        assert_eq!(
            single_table("-- note\nSELECT /* c */ a, upper(b), (a + 1) * 2 FROM t /* tail */"),
            t(None, "t")
        );
        assert_eq!(
            single_table(
                "SELECT * FROM t WHERE id IN (SELECT id FROM other WHERE x = 1) AND a IN (1, 2)"
            ),
            t(None, "t")
        );
        assert_eq!(single_table("SELECT * FROM t WHERE a = ?"), t(None, "t"));
        assert_eq!(
            single_table("SELECT EXTRACT(YEAR FROM d) y FROM t"),
            t(None, "t")
        );
        assert_eq!(single_table("SELECT * FROM 表 WHERE 列 = 1"), t(None, "表"));
    }

    #[test]
    fn everything_else_is_none() {
        for sql in [
            "",
            "SELECT 1",
            "SELECT * FROM a JOIN b ON a.id = b.id",
            "SELECT * FROM a LEFT JOIN b USING (id)",
            "SELECT * FROM a, b",
            "SELECT * FROM a, b WHERE a.id = b.id",
            "SELECT * FROM (SELECT * FROM a) x",
            "SELECT * FROM a UNION SELECT * FROM b",
            "SELECT * FROM a UNION ALL SELECT * FROM b",
            "SELECT * FROM a MINUS SELECT * FROM b",
            "SELECT a, count(*) FROM t GROUP BY a",
            "SELECT count(*) FROM t",
            "SELECT DISTINCT a FROM t",
            "WITH c AS (SELECT 1) SELECT * FROM c",
            "SELECT (SELECT max(x) FROM u) m FROM t",
            "SELECT * FROM t WHERE a = 1 GROUP BY a",
            "SELECT * FROM t HAVING a > 1",
            "SELECT * FROM t FOR UPDATE",
            "SELECT * FROM t; SELECT * FROM u",
            "SELECT * FROM a.b.c",
            "SELECT * FROM t USE INDEX (i)",
            "SELECT * FROM generate_series(1, 3)",
            "SELECT * FROM ONLY t",
            "SELECT * FROM t WHERE a = 1 INTO OUTFILE 'x'",
            "UPDATE t SET a = 1",
            "DELETE FROM t",
            "INSERT INTO t SELECT * FROM u",
            "SELECT * FROM t WHERE a = 'unterminated",
            "SELECT * FROM `t",
            "SELECT * FROM t WHERE (a = 1",
            "SELECT * FROM t WHERE a = 1)",
            "SELECT * FROM",
            "SELECT * FROM t /* open",
        ] {
            assert_eq!(single_table(sql), None, "{sql}");
        }
    }
}

/// SQL flavours that differ for [`count_wrapper`] (comment syntax, quoting).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqlDialect {
    /// `#` starts a line comment and `\` escapes in strings.
    Mysql,
    /// `$tag$ .. $tag$` quoting.
    Postgres,
    Oracle,
    Sqlite,
}

/// End of the code in `sql`: the byte after the last character that is not
/// whitespace and not inside a comment. `None` when a string, identifier or
/// comment never closes.
fn code_end(sql: &str, d: SqlDialect) -> Option<usize> {
    let b = sql.as_bytes();
    let n = b.len();
    let mut end = 0;
    let mut i = 0;
    while i < n {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'-' && b.get(i + 1) == Some(&b'-') || c == b'#' && d == SqlDialect::Mysql {
            i = sql[i..].find('\n').map_or(n, |p| i + p);
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2 + sql[i + 2..].find("*/")? + 2;
        } else if matches!(c, b'\'' | b'"' | b'`') {
            let mut j = i + 1;
            loop {
                let x = *b.get(j)?;
                if x == b'\\' && d == SqlDialect::Mysql {
                    j += 2;
                } else if x == c {
                    if b.get(j + 1) == Some(&c) {
                        j += 2;
                    } else {
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            i = j + 1;
            end = i;
        } else if c == b'$' && d == SqlDialect::Postgres {
            let mut j = i + 1;
            while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            if b.get(j) == Some(&b'$') {
                let tag = &sql[i..=j];
                i = j + 1 + sql[j + 1..].find(tag)? + tag.len();
            } else {
                i = j;
            }
            end = i;
        } else {
            i += 1;
            end = i;
        }
    }
    Some(end)
}

/// `SELECT COUNT(*) FROM (<sql>) wb_count` for a paused SELECT, or `None`
/// when the statement cannot be counted safely: not a plain `SELECT` / `WITH`
/// query, more than one statement, `INTO`, `FOR UPDATE` / `SHARE`, a locking
/// clause, a call to a state-changing function, or text the scanner is unsure
/// about (an unterminated string or comment).
///
/// Trailing `;` and comments are removed, and `)` goes on its own line so a
/// last-line `-- comment` cannot swallow it. The alias has no `AS` (Oracle
/// rejects `AS` before a table alias; the others accept both).
pub fn count_wrapper(sql: &str, dialect: SqlDialect) -> Option<String> {
    let s = select_body(sql, dialect)?;
    Some(format!("SELECT COUNT(*) FROM (\n{s}\n) wb_count"))
}

/// The single plain SELECT / WITH statement in `sql` without trailing `;` and
/// comments (see [`count_wrapper`] for what is refused), ready to sit inside
/// a derived table.
pub fn select_body(sql: &str, dialect: SqlDialect) -> Option<String> {
    let stmts = split_statements(sql);
    let [r] = stmts.as_slice() else { return None };
    let mut s = &sql[r.clone()];
    loop {
        let cut = code_end(s, dialect)?;
        let t = s[..cut].trim_end_matches(|c: char| c == ';' || c.is_whitespace());
        if t.len() == s.len() {
            break;
        }
        s = t;
    }
    if s.is_empty() {
        return None;
    }
    let c = crate::classify::classify(s);
    if c.writes
        || c.kind != crate::classify::StatementKind::Select
        || !matches!(c.keyword.as_str(), "SELECT" | "WITH")
    {
        return None;
    }
    Some(s.to_string())
}

/// `SELECT * FROM (<body>) wb_view [WHERE ..] [ORDER BY ..]`: the query with
/// filters and orders applied on the server. `body` comes from [`select_body`].
pub fn view_wrapper(body: &str, where_sql: &str, order_sql: &str) -> String {
    let mut out = format!("SELECT * FROM (\n{body}\n) wb_view");
    if !where_sql.is_empty() {
        out.push_str("\nWHERE ");
        out.push_str(where_sql);
    }
    if !order_sql.is_empty() {
        out.push_str("\nORDER BY ");
        out.push_str(order_sql);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(sql: &str) -> Vec<&str> {
        split_statements(sql).into_iter().map(|r| &sql[r]).collect()
    }

    #[test]
    fn basic_and_no_trailing_semicolon() {
        assert_eq!(
            parts("select 1; select 2;\n select 3"),
            ["select 1", "select 2", "select 3"]
        );
        assert_eq!(parts(""), Vec::<&str>::new());
        assert_eq!(parts("  ;; \n ; "), Vec::<&str>::new());
    }

    #[test]
    fn strings_and_quotes() {
        let sql = r#"insert into t values ('a;b', "c;d", 'it''s;', 'x\';y'); select `a;b`, `we``ird;` from t"#;
        assert_eq!(
            parts(sql),
            [
                r#"insert into t values ('a;b', "c;d", 'it''s;', 'x\';y')"#,
                "select `a;b`, `we``ird;` from t"
            ]
        );
        // backslash does not escape inside backticks
        assert_eq!(
            parts("select `a\\`; select 2"),
            ["select `a\\`", "select 2"]
        );
    }

    #[test]
    fn comments() {
        let sql =
            "-- note; here\nselect 1; # x; y\nselect 2 /* ; */ ; /* only */ ; select 3 -- tail;\n";
        assert_eq!(
            parts(sql),
            [
                "-- note; here\nselect 1",
                "# x; y\nselect 2 /* ; */",
                "select 3 -- tail;"
            ]
        );
        // "--" without a blank after it is not a comment
        assert_eq!(parts("select 1--1; select 2"), ["select 1--1", "select 2"]);
        // comment-only script
        assert_eq!(parts("-- nothing\n/* here */"), Vec::<&str>::new());
        // unterminated block comment
        assert_eq!(parts("select 1; /* open ; select"), ["select 1"]);
    }

    #[test]
    fn delimiter_blocks() {
        let sql = "select 1;\nDELIMITER //\nCREATE PROCEDURE p()\nBEGIN\n  select 1;\n  select 2;\nEND//\ndelimiter ;\nselect 3;\n";
        let got = parts(sql);
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[0], "select 1");
        assert!(
            got[1].starts_with("CREATE PROCEDURE") && got[1].ends_with("END"),
            "{got:?}"
        );
        assert!(got[1].contains("select 1;\n  select 2;"));
        assert_eq!(got[2], "select 3");
        // the word as part of a statement is not a command
        assert_eq!(
            parts("select 1 delimiter ;; select 2"),
            ["select 1 delimiter", "select 2"]
        );
    }

    #[test]
    fn plsql_bodies_are_one_statement() {
        let pkg =
            "CREATE OR REPLACE PACKAGE p AS\n  PROCEDURE a;\n  FUNCTION b RETURN NUMBER;\nEND p;";
        let body = "create or replace package body p is\n  procedure a is begin null; end;\n  function b return number is begin return 1; end;\nend p;";
        let sql = format!("{pkg}\n/\n{body}\n/\nselect 1 from dual;\n");
        assert_eq!(parts(&sql), [pkg, body, "select 1 from dual"]);
        // no trailing slash: the body runs to the end
        assert_eq!(parts(body), [body]);
        // procedures, triggers, type bodies, anonymous blocks
        let p = "CREATE PROCEDURE x IS\nBEGIN\n  NULL;\nEND;";
        assert_eq!(parts(&format!("{p}\n/\n")), [p]);
        let t = "CREATE OR REPLACE EDITIONABLE TRIGGER t BEFORE INSERT ON x FOR EACH ROW\nBEGIN\n  :new.a := 1;\nEND;";
        assert_eq!(parts(&format!("{t}\n  /  \nselect 2;")), [t, "select 2"]);
        let tb =
            "CREATE TYPE BODY ty AS MEMBER FUNCTION f RETURN NUMBER IS BEGIN RETURN 1; END; END;";
        assert_eq!(parts(tb), [tb]);
        let d = "DECLARE x NUMBER; BEGIN x := 1; END;";
        assert_eq!(parts(&format!("{d}\n/\nselect 3")), [d, "select 3"]);
        // a lone slash is never a statement
        assert_eq!(
            parts("select 1 from dual;\n/\nselect 2 from dual\n/\n"),
            ["select 1 from dual", "select 2 from dual"]
        );
        // division is not a terminator
        assert_eq!(
            parts("select 4\n/ 2 from dual; select 5"),
            ["select 4\n/ 2 from dual", "select 5"]
        );
    }

    #[test]
    fn heads_without_a_body_split_as_usual() {
        assert_eq!(
            parts("CREATE FUNCTION f() RETURNS INT RETURN 1; select 2"),
            ["CREATE FUNCTION f() RETURNS INT RETURN 1", "select 2"]
        );
        assert_eq!(
            parts("CREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW SET NEW.a = 1; select 2"),
            [
                "CREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW SET NEW.a = 1",
                "select 2"
            ]
        );
        assert_eq!(
            parts("CREATE TYPE mood AS ENUM ('a', 'b'); select 1"),
            ["CREATE TYPE mood AS ENUM ('a', 'b')", "select 1"]
        );
        assert_eq!(
            parts("create table t as select 1 x; select 2"),
            ["create table t as select 1 x", "select 2"]
        );
        assert_eq!(
            parts(
                "DELIMITER //\nCREATE PROCEDURE p() BEGIN select 1; END//\nDELIMITER ;\nselect 2;"
            )
            .len(),
            2
        );
    }

    #[test]
    fn postgres_dollar_quotes() {
        let f = "CREATE OR REPLACE FUNCTION f() RETURNS int LANGUAGE plpgsql AS $$\nBEGIN\n  RETURN 1;\nEND;\n$$";
        assert_eq!(parts(&format!("{f};\nselect 2;")), [f, "select 2"]);
        let g = "create function g() returns int as $body$ begin return 1; end; $body$ language plpgsql";
        assert_eq!(parts(&format!("{g}; select 3")), [g, "select 3"]);
        assert_eq!(
            parts("DO $$ BEGIN DELETE FROM t; END $$; select 1"),
            ["DO $$ BEGIN DELETE FROM t; END $$", "select 1"]
        );
        assert_eq!(
            parts("select $1, a$b; select 2"),
            ["select $1, a$b", "select 2"]
        );
        assert_eq!(
            parts("select * from v$session; select 2"),
            ["select * from v$session", "select 2"]
        );
    }

    #[test]
    fn crlf() {
        assert_eq!(
            parts("select 1;\r\nselect 2;\r\n"),
            ["select 1", "select 2"]
        );
        assert_eq!(
            parts("DELIMITER $$\r\nselect 1; select 2$$\r\nDELIMITER ;\r\nselect 3;"),
            ["select 1; select 2", "select 3"]
        );
        assert_eq!(parts("-- c\r\nselect 1;"), ["-- c\r\nselect 1"]);
    }

    #[test]
    fn multibyte() {
        let sql = "insert into t values ('你好;世界'); select '数据' as `列;名`;\n  select 'é' ; -- 注释；\n select 3";
        let ranges = split_statements(sql);
        for r in &ranges {
            assert!(sql.is_char_boundary(r.start) && sql.is_char_boundary(r.end));
        }
        let got: Vec<_> = ranges.iter().map(|r| &sql[r.clone()]).collect();
        assert_eq!(
            got,
            [
                "insert into t values ('你好;世界')",
                "select '数据' as `列;名`",
                "select 'é'",
                "-- 注释；
 select 3"
            ]
        );
        // a full-width semicolon is not a terminator
        assert_eq!(parts("select 1；select 2"), ["select 1；select 2"]);
    }

    #[test]
    fn statement_at_positions() {
        let sql = "select 1;\n\nselect 22;  \nselect 3";
        let s1 = 0..8;
        let s2 = 11..20;
        let s3 = 24..32;
        assert_eq!(split_statements(sql), [s1.clone(), s2.clone(), s3.clone()]);
        assert_eq!(statement_at(sql, 0), Some(s1.clone()));
        assert_eq!(statement_at(sql, 4), Some(s1.clone()));
        assert_eq!(statement_at(sql, 8), Some(s1.clone())); // just before ;
        assert_eq!(statement_at(sql, 9), Some(s1.clone())); // right after ;
        assert_eq!(statement_at(sql, 10), Some(s1.clone())); // blank line after
        assert_eq!(statement_at(sql, 11), Some(s2.clone())); // start of next
        assert_eq!(statement_at(sql, 15), Some(s2.clone()));
        assert_eq!(statement_at(sql, 20), Some(s2.clone())); // before the ;
        assert_eq!(statement_at(sql, 21), Some(s2.clone())); // right after ;
        assert_eq!(statement_at(sql, 23), Some(s2.clone())); // trailing blanks
        assert_eq!(statement_at(sql, 24), Some(s3.clone()));
        assert_eq!(statement_at(sql, 999), Some(s3.clone())); // past the end
        // before the first statement
        let lead = "\n\n  select 1; select 2";
        assert_eq!(statement_at(lead, 0), Some(4..12));
        assert_eq!(statement_at(lead, 3), Some(4..12));
        // trailing blank space after the last ;
        let tail = "select 1;   \n\n";
        assert_eq!(statement_at(tail, 13), Some(0..8));
        // adjacent statements
        assert_eq!(statement_at("a;b", 2), Some(0..1));
        assert_eq!(statement_at("a;b", 3), Some(2..3));
        assert_eq!(statement_at("", 0), None);
        assert_eq!(statement_at("  -- x\n ;", 3), None);
    }

    #[test]
    fn split_big_fixture_timing() {
        let Some(path) = std::env::var_os("LOCUS_BIG_SQL") else {
            eprintln!("LOCUS_BIG_SQL not set, skipping");
            return;
        };
        let Ok(sql) = std::fs::read_to_string(&path) else {
            eprintln!("{path:?} not found, skipping");
            return;
        };
        let t = std::time::Instant::now();
        let n = split_statements(&sql).len();
        let dt = t.elapsed();
        println!("split {} bytes into {n} statements in {dt:?}", sql.len());
        assert!(n > 0);
    }

    fn cw(sql: &str) -> Option<String> {
        count_wrapper(sql, SqlDialect::Postgres)
    }

    fn wrapped(body: &str) -> String {
        format!("SELECT COUNT(*) FROM (\n{body}\n) wb_count")
    }

    #[test]
    fn count_wrapper_strips_trailing_comments_and_semicolons() {
        let w = |s| wrapped(s);
        assert_eq!(cw("SELECT * FROM t"), Some(w("SELECT * FROM t")));
        assert_eq!(cw("SELECT * FROM t;"), Some(w("SELECT * FROM t")));
        assert_eq!(cw("SELECT * FROM t -- note"), Some(w("SELECT * FROM t")));
        assert_eq!(cw("SELECT * FROM t; -- note"), Some(w("SELECT * FROM t")));
        assert_eq!(
            cw("SELECT * FROM t -- a\n;\n-- b"),
            Some(w("SELECT * FROM t"))
        );
        assert_eq!(cw("SELECT * FROM t /* note */"), Some(w("SELECT * FROM t")));
        assert_eq!(cw("SELECT * FROM t ;;"), Some(w("SELECT * FROM t")));
        assert_eq!(
            count_wrapper("SELECT * FROM t # note", SqlDialect::Mysql),
            Some(w("SELECT * FROM t"))
        );
        // `#` is an operator in PostgreSQL, not a comment.
        assert_eq!(
            cw("SELECT a #> '{b}' FROM t"),
            Some(w("SELECT a #> '{b}' FROM t"))
        );
    }

    #[test]
    fn count_wrapper_keeps_comment_text_inside_literals() {
        let s = "SELECT '-- not a comment', \"x--\" FROM t";
        assert_eq!(cw(s), Some(wrapped(s)));
        let s = "SELECT 'it''s /* x' FROM t";
        assert_eq!(cw(s), Some(wrapped(s)));
        let s = "SELECT $$ -- $$ FROM t";
        assert_eq!(cw(s), Some(wrapped(s)));
        // A string whose end depends on `\` handling is not trusted.
        assert_eq!(
            count_wrapper("SELECT 'a\\' -- b' FROM t", SqlDialect::Mysql),
            None
        );
    }

    #[test]
    fn count_wrapper_cte_and_leading_comment() {
        let s = "-- lead\nWITH a AS (SELECT 1) SELECT * FROM a";
        assert_eq!(cw(s), Some(wrapped(s)));
        let s = "WITH a AS (SELECT 1) SELECT * FROM a -- x";
        assert_eq!(
            count_wrapper(s, SqlDialect::Oracle),
            Some(wrapped("WITH a AS (SELECT 1) SELECT * FROM a"))
        );
    }

    #[test]
    fn count_wrapper_refuses_what_it_cannot_count() {
        for s in [
            "SELECT * FROM t FOR UPDATE",
            "SELECT * FROM t FOR SHARE",
            "SELECT * FROM t LOCK IN SHARE MODE",
            "SELECT a INTO x FROM t",
            "SELECT * FROM t; SELECT 2",
            "DELETE FROM t",
            "INSERT INTO t VALUES (1)",
            "SELECT nextval('s')",
            "SHOW TABLES",
            "SELECT 'open",
            "SELECT 1 /* open",
            "",
            "-- only a comment",
            ";",
        ] {
            assert_eq!(cw(s), None, "{s}");
        }
    }
}
