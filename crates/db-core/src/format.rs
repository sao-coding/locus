//! A conservative SQL formatter.
//!
//! It only changes whitespace and, when asked, the case of well-known
//! keywords. It never adds, removes or reorders a token, never touches the
//! inside of a comment, string, quoted identifier or dollar-quoted string, and
//! never inserts whitespace between two tokens that were written without any,
//! so `<=` or `a.b` cannot be split. Input it cannot read with confidence is
//! returned unchanged (see [`format_sql`]).

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatOptions {
    /// Upper-case the common keywords (`select` -> `SELECT`). Identifiers,
    /// quoted names and anything after a `.`, `:` or `@` are left alone.
    pub uppercase_keywords: bool,
    /// Spaces per indentation level.
    pub indent: usize,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            uppercase_keywords: true,
            indent: 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum K {
    Word,
    /// String, quoted identifier, dollar-quoted string: copied as is.
    Lit,
    LineComment,
    BlockComment,
    Punct,
}

#[derive(Clone, Debug)]
struct Tok<'a> {
    kind: K,
    text: &'a str,
    /// Whitespace came before this token in the input.
    sp: bool,
    /// A newline came before this token in the input.
    nl: bool,
}

impl Tok<'_> {
    fn is_comment(&self) -> bool {
        matches!(self.kind, K::LineComment | K::BlockComment)
    }
    fn is(&self, c: char) -> bool {
        self.kind == K::Punct && self.text.len() == 1 && self.text.starts_with(c)
    }
    fn up(&self) -> Option<String> {
        (self.kind == K::Word).then(|| self.text.to_ascii_uppercase())
    }
}

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

/// `None` when the text cannot be read with confidence.
fn lex(sql: &str) -> Option<Vec<Tok<'_>>> {
    let b = sql.as_bytes();
    let n = b.len();
    let mut out: Vec<Tok> = Vec::new();
    let (mut i, mut sp, mut nl) = (0, false, false);
    while i < n {
        let c = b[i];
        if c.is_ascii_whitespace() {
            sp = true;
            nl |= c == b'\n' || c == b'\r';
            i += 1;
            continue;
        }
        let (kind, end) = if c == b'-' && b.get(i + 1) == Some(&b'-')
            || c == b'#' && b.get(i + 1) != Some(&b'>')
        {
            (
                K::LineComment,
                sql[i..].find(['\n', '\r']).map_or(n, |p| i + p),
            )
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let p = sql[i + 2..].find("*/")?;
            if sql[i + 2..i + 2 + p].contains("/*") {
                return None; // nested comments end differently per server
            }
            (K::BlockComment, i + 2 + p + 2)
        } else if c == b'\'' || c == b'"' || c == b'`' {
            let std = quote_end(b, i, false)?;
            if c != b'`' && quote_end(b, i, true) != Some(std) {
                return None; // `\'` ends the string on one server and not on another
            }
            (K::Lit, std)
        } else if c == b'$' {
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
                let p = sql[j + 1..].find(needle)?;
                (K::Lit, j + 1 + p + needle.len())
            } else {
                (K::Punct, i + 1)
            }
        } else if c == b'[' {
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
                Some(e) => (K::Lit, e),
                None => (K::Punct, i + 1),
            }
        } else if c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80 {
            let mut j = i;
            while j < n && is_word_byte(b[j]) {
                j += 1;
            }
            // Oracle alternative quoting: q'[ .. ]'
            let w = &sql[i..j];
            if (w.eq_ignore_ascii_case("q") || w.eq_ignore_ascii_case("nq"))
                && b.get(j) == Some(&b'\'')
                && j + 1 < n
                && !b[j + 1].is_ascii_whitespace()
            {
                let close = match b[j + 1] {
                    b'[' => b']',
                    b'(' => b')',
                    b'{' => b'}',
                    b'<' => b'>',
                    o => o,
                };
                let p = b[j + 2..].windows(2).position(|x| x == [close, b'\''])?;
                (K::Lit, j + 2 + p + 2)
            } else {
                (K::Word, j)
            }
        } else {
            (K::Punct, i + 1)
        };
        out.push(Tok {
            kind,
            text: &sql[i..end],
            sp: sp || out.is_empty(),
            nl,
        });
        i = end;
        sp = false;
        nl = false;
    }
    Some(out)
}

const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP",
    "BY",
    "ORDER",
    "HAVING",
    "LIMIT",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "OUTER",
    "CROSS",
    "NATURAL",
    "ON",
    "USING",
    "UNION",
    "ALL",
    "INTERSECT",
    "EXCEPT",
    "MINUS",
    "DISTINCT",
    "AS",
    "AND",
    "OR",
    "NOT",
    "IN",
    "IS",
    "NULL",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "ASC",
    "DESC",
    "WITH",
    "RECURSIVE",
    "OVER",
    "PARTITION",
];

const JOIN_MODS: [&str; 6] = ["INNER", "LEFT", "RIGHT", "FULL", "CROSS", "NATURAL"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Top,
    /// A parenthesised subquery: its content is laid out like a statement.
    Sub,
    /// Any other parenthesis: flowed inline.
    Plain,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Clause {
    Other,
    Select,
    Where,
    On,
}

struct Frame {
    kind: FrameKind,
    /// Indent level of the clause lines of this frame.
    base: usize,
    clause: Clause,
    /// The SELECT list has more than one item.
    multi: bool,
    /// Just after `SELECT`: modifiers may still follow before the list starts.
    in_modifiers: bool,
    case_depth: u32,
    between: bool,
    count: usize,
}

impl Frame {
    fn new(kind: FrameKind, base: usize) -> Self {
        Frame {
            kind,
            base,
            clause: Clause::Other,
            multi: false,
            in_modifiers: false,
            case_depth: 0,
            between: false,
            count: 0,
        }
    }
}

struct Writer {
    out: String,
    width: usize,
    pending: Option<usize>,
    line_indent: usize,
}

impl Writer {
    fn put(&mut self, text: &str, sp: bool) {
        match self.pending.take() {
            Some(ind) if !self.out.is_empty() => {
                self.out.push('\n');
                self.out.extend(std::iter::repeat_n(' ', ind * self.width));
                self.line_indent = ind;
            }
            _ => {
                if sp && !self.out.is_empty() && !self.out.ends_with(char::is_whitespace) {
                    self.out.push(' ');
                }
            }
        }
        self.out.push_str(text);
    }
}

/// Formats `sql`. Returns it unchanged when it holds anything the formatter
/// cannot read with confidence: an unterminated string or comment, unbalanced
/// parentheses, a string whose end depends on backslash escaping, nested
/// comments, a procedure / function / trigger body or `BEGIN .. END` block,
/// `DELIMITER`, a SQL Server `GO` or Oracle `/` line.
pub fn format_sql(sql: &str, opts: &FormatOptions) -> String {
    match format_inner(sql, opts) {
        Some(s) => s,
        None => sql.to_string(),
    }
}

fn unsafe_to_format(toks: &[Tok], sql: &str) -> bool {
    if sql.lines().any(|l| l.trim() == "/") {
        return true;
    }
    let mut depth = 0i32;
    for (i, t) in toks.iter().enumerate() {
        if t.is('(') {
            depth += 1;
        } else if t.is(')') {
            depth -= 1;
            if depth < 0 {
                return true;
            }
        } else if t.is(';') && depth != 0 {
            return true;
        }
        if let Some(w) = t.up() {
            match w.as_str() {
                "DELIMITER" | "DECLARE" | "PROCEDURE" | "FUNCTION" | "TRIGGER" | "PACKAGE"
                | "GO" => return true,
                "BEGIN" => {
                    let tx = match toks.get(i + 1) {
                        None => true,
                        Some(n) if n.is(';') => true,
                        Some(n) => {
                            matches!(n.up().as_deref(), Some("TRANSACTION" | "TRAN" | "WORK"))
                        }
                    };
                    if !tx {
                        return true;
                    }
                }
                _ => {}
            }
        }
    }
    depth != 0
}

fn next_sig<'a, 'b>(toks: &'b [Tok<'a>], i: usize) -> Option<&'b Tok<'a>> {
    toks[i + 1..].iter().find(|t| !t.is_comment())
}

fn prev_sig<'a, 'b>(toks: &'b [Tok<'a>], i: usize) -> Option<&'b Tok<'a>> {
    toks[..i].iter().rev().find(|t| !t.is_comment())
}

/// Does the SELECT at `i` list more than one item?
fn select_has_comma(toks: &[Tok], i: usize) -> bool {
    let mut depth = 0i32;
    for (k, t) in toks.iter().enumerate().skip(i + 1) {
        let qualified = (toks[k - 1].is('.') || toks[k - 1].is(':') || toks[k - 1].is('@'))
            && !t.sp
            || toks.get(k + 1).is_some_and(|n| n.is('.') && !n.sp);
        if qualified {
            continue;
        }
        if t.is('(') {
            depth += 1;
        } else if t.is(')') {
            if depth == 0 {
                return false;
            }
            depth -= 1;
        } else if depth == 0 {
            if t.is(',') {
                return true;
            }
            if t.is(';') {
                return false;
            }
            if let Some(w) = t.up()
                && matches!(
                    w.as_str(),
                    "FROM"
                        | "INTO"
                        | "WHERE"
                        | "GROUP"
                        | "HAVING"
                        | "ORDER"
                        | "LIMIT"
                        | "UNION"
                        | "INTERSECT"
                        | "EXCEPT"
                        | "MINUS"
                )
            {
                return false;
            }
        }
    }
    false
}

/// Does a join phrase start at `i` (`JOIN`, `LEFT OUTER JOIN`, `CROSS APPLY`)?
fn join_starts(toks: &[Tok], i: usize, w: &str) -> bool {
    let is_mod = |s: &str| JOIN_MODS.contains(&s) || s == "OUTER";
    if !(is_mod(w) || w == "JOIN" || w == "STRAIGHT_JOIN") {
        return false;
    }
    if prev_sig(toks, i)
        .and_then(|t| t.up())
        .is_some_and(|p| is_mod(&p))
    {
        return false; // continuation of a phrase that already started
    }
    if w == "JOIN" || w == "STRAIGHT_JOIN" {
        return true;
    }
    let mut j = i;
    loop {
        match toks[j..]
            .iter()
            .find(|t| !t.is_comment())
            .and_then(|t| t.up())
        {
            Some(s) if is_mod(&s) => {
                // advance past this word
                j += toks[j..].iter().position(|t| !t.is_comment()).unwrap_or(0) + 1;
                if j >= toks.len() {
                    return false;
                }
            }
            Some(s) => return s == "JOIN" || s == "APPLY",
            None => return false,
        }
    }
}

fn format_inner(sql: &str, opts: &FormatOptions) -> Option<String> {
    let toks = lex(sql)?;
    if toks.is_empty() || unsafe_to_format(&toks, sql) {
        return None;
    }
    let mut wr = Writer {
        out: String::with_capacity(sql.len() + sql.len() / 4),
        width: opts.indent,
        pending: None,
        line_indent: 0,
    };
    let mut frames = vec![Frame::new(FrameKind::Top, 0)];
    let mut stmt_start = false;
    let mut prev_word: Option<String> = None;

    for (i, t) in toks.iter().enumerate() {
        if t.is_comment() {
            if stmt_start {
                if !t.nl {
                    // trailing comment on the line of the `;`
                    wr.put(t.text, true);
                    continue;
                }
                stmt_start = false;
                wr.pending = None;
                wr.out.push_str("\n\n");
                wr.line_indent = 0;
            }
            // A trailing comment stays on its line; the layout break that was
            // waiting moves after it.
            let saved = if t.nl { None } else { wr.pending.take() };
            if t.nl && wr.pending.is_none() {
                wr.pending = Some(wr.line_indent);
            }
            wr.put(t.text, t.sp);
            if t.kind == K::LineComment {
                wr.pending = Some(saved.unwrap_or(wr.line_indent));
            } else if saved.is_some() {
                wr.pending = saved;
            }
            continue;
        }

        if stmt_start {
            stmt_start = false;
            wr.pending = None;
            wr.out.push_str("\n\n");
            wr.line_indent = 0;
        }

        let fi = frames.len() - 1;
        let laid_out = frames[fi].kind != FrameKind::Plain;
        let base = frames[fi].base;

        // The first thing after `SELECT [DISTINCT]` starts the item list.
        if laid_out && frames[fi].in_modifiers {
            let modifier = matches!(t.up().as_deref(), Some("DISTINCT" | "ALL" | "UNIQUE"));
            if !modifier {
                frames[fi].in_modifiers = false;
                wr.pending = Some(base + 1);
            }
        }

        let prev_dot =
            prev_sig(&toks, i).is_some_and(|p| (p.is('.') || p.is(':') || p.is('@')) && !t.sp);
        let next_dot = toks.get(i + 1).is_some_and(|n| n.is('.') && !n.sp);
        let qualified = prev_dot || next_dot;
        let word = t.up();
        let mut text = t.text.to_string();

        if t.is('(') {
            let sub = matches!(
                next_sig(&toks, i).and_then(|n| n.up()).as_deref(),
                Some("SELECT" | "WITH")
            );
            wr.put("(", t.sp);
            if sub {
                let k = wr.line_indent;
                frames.push(Frame::new(FrameKind::Sub, k + 1));
                wr.pending = Some(k + 1);
            } else {
                frames.push(Frame::new(FrameKind::Plain, 0));
            }
            frames[fi].count += 1;
            prev_word = None;
            continue;
        }
        if t.is(')') {
            let f = frames.pop()?;
            if f.kind == FrameKind::Sub {
                wr.pending = Some(f.base - 1);
            }
            wr.put(")", t.sp);
            let fi = frames.len() - 1;
            frames[fi].count += 1;
            prev_word = None;
            continue;
        }
        if t.is(';') {
            wr.put(";", t.sp);
            frames.truncate(1);
            frames[0] = Frame::new(FrameKind::Top, 0);
            stmt_start = true;
            prev_word = None;
            continue;
        }
        if t.is(',') {
            wr.put(",", t.sp);
            let f = &frames[fi];
            if laid_out && f.clause == Clause::Select && f.multi {
                wr.pending = Some(base + 1);
            }
            frames[fi].count += 1;
            prev_word = None;
            continue;
        }

        if let (Some(w), true) = (word.as_deref(), laid_out && !qualified) {
            let first = frames[fi].count == 0;
            let f = &mut frames[fi];
            let mut brk: Option<usize> = None;
            match w {
                "SELECT" => {
                    brk = Some(base);
                    f.clause = Clause::Select;
                    f.multi = select_has_comma(&toks, i);
                    f.in_modifiers = f.multi;
                }
                "FROM" => {
                    if prev_word.as_deref() != Some("DISTINCT") {
                        brk = Some(base);
                    }
                    f.clause = Clause::Other;
                }
                "WHERE" => {
                    brk = Some(base);
                    f.clause = Clause::Where;
                }
                "HAVING" | "LIMIT" | "UNION" | "INTERSECT" | "EXCEPT" | "MINUS" | "VALUES"
                | "SET" => {
                    brk = Some(base);
                    f.clause = Clause::Other;
                }
                "GROUP" | "ORDER"
                    if next_sig(&toks, i).and_then(|n| n.up()).as_deref() == Some("BY") =>
                {
                    brk = Some(base);
                    f.clause = Clause::Other;
                }
                "ON" => f.clause = Clause::On,
                "BETWEEN" => f.between = true,
                "CASE" => f.case_depth += 1,
                "END" => f.case_depth = f.case_depth.saturating_sub(1),
                "AND" | "OR" => {
                    if w == "AND" && f.between {
                        f.between = false;
                    } else if matches!(f.clause, Clause::Where | Clause::On) && f.case_depth == 0 {
                        brk = Some(base + 1);
                    }
                }
                _ => {}
            }
            if brk.is_none() && join_starts(&toks, i, w) {
                brk = Some(base);
                frames[fi].clause = Clause::Other;
            }
            if let Some(ind) = brk
                && !first
            {
                wr.pending = Some(ind);
            }
        }

        if opts.uppercase_keywords
            && let Some(w) = word.as_deref()
            && KEYWORDS.contains(&w)
            && !qualified
        {
            text = w.to_string();
        }
        wr.put(&text, t.sp);
        frames[fi].count += 1;
        prev_word = word;
    }
    Some(wr.out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whitespace outside literals and comments removed, ASCII case folded
    /// outside literals and comments, so the token sequence can be compared.
    fn strip(sql: &str) -> String {
        let b = sql.as_bytes();
        let n = b.len();
        let mut out = String::new();
        let mut i = 0;
        while i < n {
            let c = b[i];
            let end = if c.is_ascii_whitespace() {
                i += 1;
                continue;
            } else if c == b'-' && b.get(i + 1) == Some(&b'-') || c == b'#' {
                sql[i..].find(['\n', '\r']).map_or(n, |p| i + p)
            } else if c == b'[' {
                i + sql[i..].find(']').unwrap() + 1
            } else if c == b'q' && b.get(i + 1) == Some(&b'\'') {
                let close = match b[i + 2] {
                    b'[' => b']',
                    o => o,
                };
                i + 3
                    + b[i + 3..]
                        .windows(2)
                        .position(|x| x == [close, b'\''])
                        .unwrap()
                    + 2
            } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
                i + 2 + sql[i + 2..].find("*/").unwrap() + 2
            } else if c == b'\'' || c == b'"' || c == b'`' {
                quote_end(b, i, false).unwrap()
            } else if c == b'$'
                && sql[i + 1..].find('$').is_some_and(|p| {
                    sql[i + 1..i + 1 + p]
                        .bytes()
                        .all(|x| x.is_ascii_alphanumeric() || x == b'_')
                })
            {
                let j = i + 1 + sql[i + 1..].find('$').unwrap();
                let needle = &sql[i..=j];
                j + 1 + sql[j + 1..].find(needle).unwrap() + needle.len()
            } else {
                out.push(c.to_ascii_uppercase() as char);
                i += 1;
                if c >= 0x80 {
                    // copy the whole multibyte char (it is not whitespace)
                    out.pop();
                    let ch = sql[i - 1..].chars().next().unwrap();
                    out.push(ch);
                    i = i - 1 + ch.len_utf8();
                }
                continue;
            };
            out.push_str(&sql[i..end]);
            i = end;
        }
        out
    }

    fn opts() -> FormatOptions {
        FormatOptions {
            uppercase_keywords: true,
            indent: 4,
        }
    }

    /// Formats with both settings and checks token sequence and idempotency.
    fn fmt_with(sql: &str, o: &FormatOptions) -> String {
        let out = format_sql(sql, o);
        assert_eq!(strip(sql), strip(&out), "tokens changed:\n{sql}\n->\n{out}");
        assert_eq!(format_sql(&out, o), out, "not idempotent:\n{out}");
        out
    }

    fn fmt(sql: &str) -> String {
        let lower = FormatOptions {
            uppercase_keywords: false,
            indent: 4,
        };
        fmt_with(sql, &lower);
        fmt_with(
            sql,
            &FormatOptions {
                uppercase_keywords: true,
                indent: 2,
            },
        );
        fmt_with(
            sql,
            &FormatOptions {
                uppercase_keywords: false,
                indent: 0,
            },
        );
        fmt_with(sql, &opts())
    }

    #[test]
    fn simple_select() {
        assert_eq!(
            fmt("select a from   t where a=1"),
            "SELECT a\nFROM t\nWHERE a=1"
        );
    }

    #[test]
    fn select_list_one_per_line() {
        assert_eq!(
            fmt("select a, b,c from t"),
            "SELECT\n    a,\n    b,\n    c\nFROM t"
        );
        assert_eq!(
            fmt("select distinct a, b from t"),
            "SELECT DISTINCT\n    a,\n    b\nFROM t"
        );
    }

    #[test]
    fn function_commas_stay_inline() {
        assert_eq!(
            fmt("select coalesce(a, b) from t"),
            "SELECT coalesce(a, b)\nFROM t"
        );
        assert_eq!(
            fmt("select count(*), max(x) from t"),
            "SELECT\n    count(*),\n    max(x)\nFROM t"
        );
    }

    #[test]
    fn and_or_lines() {
        assert_eq!(
            fmt("select * from t where a = 1 and b = 2 or c = 3"),
            "SELECT *\nFROM t\nWHERE a = 1\n    AND b = 2\n    OR c = 3"
        );
    }

    #[test]
    fn between_and_case_keep_their_and() {
        let out = fmt("select * from t where a between 1 and 2 and b = 3");
        assert_eq!(
            out,
            "SELECT *\nFROM t\nWHERE a BETWEEN 1 AND 2\n    AND b = 3"
        );
        let out =
            fmt("select * from t where case when a = 1 and b = 2 then 1 else 0 end = 1 and c = 2");
        assert_eq!(
            out,
            "SELECT *\nFROM t\nWHERE CASE WHEN a = 1 AND b = 2 THEN 1 ELSE 0 END = 1\n    AND c = 2"
        );
    }

    #[test]
    fn joins() {
        let out = fmt(
            "select * from a inner join b on a.id = b.id and a.x = 1 left outer join c on c.id = a.id cross join d join e using (id) where 1=1",
        );
        assert_eq!(
            out,
            "SELECT *\nFROM a\nINNER JOIN b ON a.id = b.id\n    AND a.x = 1\nLEFT OUTER JOIN c ON c.id = a.id\nCROSS JOIN d\nJOIN e USING (id)\nWHERE 1=1"
        );
    }

    #[test]
    fn left_function_is_not_a_join() {
        assert_eq!(fmt("select left(a, 2) from t"), "SELECT LEFT(a, 2)\nFROM t");
    }

    #[test]
    fn subquery_is_indented() {
        let out =
            fmt("select * from t where id in (select id from u where x = 1 and y = 2) and z = 1");
        assert_eq!(
            out,
            "SELECT *\nFROM t\nWHERE id IN (\n    SELECT id\n    FROM u\n    WHERE x = 1\n        AND y = 2\n)\n    AND z = 1"
        );
    }

    #[test]
    fn nested_subqueries() {
        fmt("select * from (select a from (select 1 a) x) y");
        fmt("select (select max(a) from t), b from u");
    }

    #[test]
    fn union_and_order() {
        let out = fmt("select a from t union all select a from u order by a desc limit 5");
        assert_eq!(
            out,
            "SELECT a\nFROM t\nUNION ALL\nSELECT a\nFROM u\nORDER BY a DESC\nLIMIT 5"
        );
    }

    #[test]
    fn group_by_having() {
        assert_eq!(
            fmt("select a, count(*) from t group by a having count(*) > 1"),
            "SELECT\n    a,\n    count(*)\nFROM t\nGROUP BY a\nHAVING count(*) > 1"
        );
    }

    #[test]
    fn window_order_by_stays_inline() {
        assert_eq!(
            fmt("select row_number() over (partition by a order by b) from t"),
            "SELECT row_number() OVER (PARTITION BY a ORDER BY b)\nFROM t"
        );
    }

    #[test]
    fn dml() {
        assert_eq!(
            fmt("insert into t (a, b) values (1, 2), (3, 4)"),
            "INSERT INTO t (a, b)\nVALUES (1, 2), (3, 4)"
        );
        assert_eq!(
            fmt("update t set a = 1, b = 2 where id = 3"),
            "UPDATE t\nSET a = 1, b = 2\nWHERE id = 3"
        );
        assert_eq!(
            fmt("delete from t where id = 3"),
            "DELETE\nFROM t\nWHERE id = 3"
        );
        fmt("insert into t select * from u where a = 1");
    }

    #[test]
    fn cte() {
        let out = fmt("with a as (select 1 x), b as (select 2 y) select * from a, b");
        assert_eq!(
            out,
            "WITH a AS (\n    SELECT 1 x\n), b AS (\n    SELECT 2 y\n)\nSELECT *\nFROM a, b"
        );
    }

    #[test]
    fn literals_are_copied() {
        let sql = "select 'a  b\nc', \"Q  x\", `b  t`, [x  y], $$ a   b $$, $t$ x  $$ y $t$, q'[a  'b]' from t";
        let out = fmt(sql);
        for lit in [
            "'a  b\nc'",
            "\"Q  x\"",
            "`b  t`",
            "[x  y]",
            "$$ a   b $$",
            "$t$ x  $$ y $t$",
            "q'[a  'b]'",
        ] {
            assert!(out.contains(lit), "{lit} in {out}");
        }
    }

    #[test]
    fn comments_are_copied() {
        let sql = "select a, -- first   one\n b /* two   words */ from t -- tail\nwhere x = 1";
        let out = fmt(sql);
        assert!(out.contains("-- first   one"));
        assert!(out.contains("/* two   words */"));
        assert!(out.contains("-- tail"));
        assert_eq!(
            out,
            "SELECT\n    a, -- first   one\n    b /* two   words */\nFROM t -- tail\nWHERE x = 1"
        );
    }

    #[test]
    fn leading_and_own_line_comments() {
        let out = fmt("-- head\nselect 1\n-- mid\nfrom dual");
        assert!(out.starts_with("-- head\nSELECT 1"));
        assert!(out.contains("-- mid"));
        fmt("/* a */ select /* b */ 1 /* c */");
        fmt("# mysql comment\nselect 1");
    }

    #[test]
    fn multiple_statements() {
        let out = fmt("select 1;select 2 ; -- end\nupdate t set a=1;");
        assert_eq!(out, "SELECT 1;\n\nSELECT 2 ; -- end\n\nUPDATE t\nSET a=1;");
    }

    #[test]
    fn uppercase_option_off_keeps_case() {
        let o = FormatOptions {
            uppercase_keywords: false,
            indent: 4,
        };
        assert_eq!(fmt_with("Select a From T", &o), "Select a\nFrom T");
    }

    #[test]
    fn uppercase_skips_qualified_and_quoted_names() {
        assert_eq!(
            fmt("select t.limit, :order, \"from\" from t"),
            "SELECT\n    t.limit,\n    :order,\n    \"from\"\nFROM t"
        );
    }

    #[test]
    fn operators_are_not_split() {
        let out = fmt("select a<=b, c<>d, e||f, g::int, h->>'x' from t where a>=1 and b!=2");
        assert!(
            out.contains("a<=b")
                && out.contains("c<>d")
                && out.contains("e||f")
                && out.contains("g::int")
                && out.contains("h->>'x'")
        );
        assert!(out.contains("a>=1") && out.contains("b!=2"));
    }

    #[test]
    fn is_distinct_from_stays_on_the_line() {
        assert_eq!(
            fmt("select * from t where a is distinct from b"),
            "SELECT *\nFROM t\nWHERE a IS DISTINCT FROM b"
        );
    }

    #[test]
    fn indent_option() {
        let o = FormatOptions {
            uppercase_keywords: true,
            indent: 2,
        };
        assert_eq!(
            fmt_with("select a, b from t where x = 1 and y = 2", &o),
            "SELECT\n  a,\n  b\nFROM t\nWHERE x = 1\n  AND y = 2"
        );
    }

    #[test]
    fn multibyte() {
        assert_eq!(
            fmt("select 名前, 'é' from 表 where 列 = 1"),
            "SELECT\n    名前,\n    'é'\nFROM 表\nWHERE 列 = 1"
        );
    }

    #[test]
    fn crlf_input() {
        assert_eq!(
            fmt("select a\r\nfrom t -- c\r\nwhere a = 1"),
            "SELECT a\nFROM t -- c\nWHERE a = 1"
        );
    }

    #[test]
    fn tsql_and_oracle_samples() {
        fmt("select top 5 * from [dbo].[t] with (nolock) where a = 1");
        fmt("select * from #tmp where a = 1");
        fmt("select * from emp where rownum <= 5 order by sal desc");
        fmt("select a from t cross apply f(t.x) outer apply g(t.y)");
        fmt("select * from t where a = :a and b = @b and c = $1");
    }

    #[test]
    fn unparsable_input_is_unchanged() {
        for sql in [
            "create procedure p() begin select 1; end",
            "BEGIN\n  delete from t;\nEND;",
            "DELIMITER //\nselect 1//\nDELIMITER ;",
            "select 1\nGO\nselect 2",
            "select 1\n/\nselect 2",
            "select 'unterminated",
            "select 1 /* open",
            "select ((1)",
            "select 1)",
            "select 'a\\'' from   t",
            "select $$ open",
            "select 1 /* a /* b */ c */",
            "CREATE FUNCTION f() RETURNS int RETURN 1",
            "DECLARE x int; SELECT   1",
        ] {
            assert_eq!(format_sql(sql, &opts()), sql, "{sql}");
        }
    }

    #[test]
    fn transaction_begin_is_formatted() {
        assert_eq!(fmt("begin; select  1"), "begin;\n\nSELECT 1");
    }

    #[test]
    fn empty_and_blank() {
        assert_eq!(format_sql("", &opts()), "");
        assert_eq!(format_sql("  \n ", &opts()), "  \n ");
    }

    #[test]
    fn already_formatted_is_stable() {
        let s = "SELECT\n    a,\n    b\nFROM t\nWHERE x = 1\n    AND y = 2";
        assert_eq!(fmt(s), s);
    }

    #[test]
    fn comment_before_closing_paren() {
        fmt("select * from t where a in (select 1 -- one\n) and b = 2");
        fmt("select f(a, -- x\n b) from t");
    }

    #[test]
    fn insert_on_duplicate_and_returning() {
        fmt("insert into t (a) values (1) on duplicate key update a = values(a)");
        fmt(
            "insert into t (a) values (1) on conflict (a) do update set a = excluded.a returning *",
        );
    }

    #[test]
    fn hash_comment_forces_following_newline() {
        let out = fmt("select * from t where a = 1 # why\nand b = 2");
        assert!(out.contains("# why\n"));
    }
}
