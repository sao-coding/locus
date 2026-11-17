//! Table structure for the read-only Structure tab: columns with defaults and
//! comments, indexes, foreign keys in both directions, checks and triggers.
//! The SQL per engine lives in [`crate::dictionary`]; this file has the types
//! and the text helpers that need no server (unit tested).

use crate::ObjectEntry;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructColumn {
    pub name: String,
    /// Full type as the server writes it, e.g. `varchar(191)`.
    pub type_name: String,
    pub nullable: bool,
    /// Default expression as the server stores it; `None` = no default.
    pub default: Option<String>,
    pub primary_key: bool,
    /// AUTO_INCREMENT / identity / serial (a sequence feeds the column).
    pub auto_increment: bool,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructIndex {
    pub name: String,
    /// Key columns in index order (`<expr>` for an expression part).
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
    /// BTREE, HASH, FULLTEXT, NORMAL, ...; empty when the engine has none.
    pub kind: String,
}

/// One foreign key of `schema.table` (the table that holds the columns).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructForeignKey {
    pub name: String,
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    /// `CASCADE`, `SET NULL`, `NO ACTION`, `RESTRICT`, `SET DEFAULT`.
    pub on_delete: String,
    pub on_update: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructCheck {
    pub name: String,
    pub expression: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableStructure {
    pub columns: Vec<StructColumn>,
    pub indexes: Vec<StructIndex>,
    /// Foreign keys this table declares.
    pub foreign_keys: Vec<StructForeignKey>,
    /// Foreign keys of other tables that point at this one.
    pub referenced_by: Vec<StructForeignKey>,
    pub checks: Vec<StructCheck>,
    /// `detail` = timing and event where the engine tells.
    pub triggers: Vec<ObjectEntry>,
}

/// `CHECK ((a > 0))` -> `a > 0`: the bare expression (PostgreSQL writes the
/// whole clause, MySQL / MariaDB and Oracle only the expression).
pub fn bare_check(text: &str) -> String {
    let mut s = text.trim();
    if s.len() >= 5
        && s.is_char_boundary(5)
        && s[..5].eq_ignore_ascii_case("check")
        && s[5..].trim_start().starts_with('(')
    {
        s = s[5..].trim_start();
    }
    // Remove redundant outer parentheses only while they enclose the whole text.
    while s.starts_with('(') && s.ends_with(')') && encloses(s) {
        s = s[1..s.len() - 1].trim();
    }
    s.to_string()
}

/// The first `(` closes at the very end.
fn encloses(s: &str) -> bool {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' | '`' => quote = Some(c),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return i + 1 == s.len();
                    }
                }
                _ => {}
            },
        }
    }
    false
}

/// Oracle lists every NOT NULL as a check `"COL" IS NOT NULL`; that is the
/// column's nullability, not a constraint worth a row.
pub fn is_not_null_check(expr: &str) -> bool {
    let e = expr.trim();
    let Some(head) = e.strip_suffix("IS NOT NULL").map(str::trim_end) else {
        return false;
    };
    head.len() >= 2
        && head.starts_with('"')
        && head.ends_with('"')
        && !head[1..head.len() - 1].contains('"')
}

/// CHECK constraints in a SQLite `CREATE TABLE` text: `(name, expression)`;
/// the name is empty for an unnamed one. Strings, quoted names and comments
/// are skipped, so a `CHECK` inside them is not taken.
pub fn sqlite_checks(sql: &str) -> Vec<(String, String)> {
    let b: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    // The last `CONSTRAINT <name>` seen at the table level, cleared by a comma.
    let mut pending: Option<String> = None;
    let mut depth = 0i32;
    let word_at = |i: usize, w: &str| -> bool {
        let n = w.chars().count();
        i + n <= b.len()
            && b[i..i + n]
                .iter()
                .collect::<String>()
                .eq_ignore_ascii_case(w)
            && (i == 0 || !(b[i - 1].is_alphanumeric() || b[i - 1] == '_'))
            && (i + n == b.len() || !(b[i + n].is_alphanumeric() || b[i + n] == '_'))
    };
    while i < b.len() {
        let c = b[i];
        match c {
            '\'' | '"' | '`' | '[' => {
                let close = if c == '[' { ']' } else { c };
                i += 1;
                while i < b.len() {
                    if b[i] == close {
                        if i + 1 < b.len() && b[i + 1] == close && c != '[' {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            '-' if i + 1 < b.len() && b[i + 1] == '-' => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < b.len() && b[i + 1] == '*' => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            '(' => {
                depth += 1;
                i += 1;
            }
            ')' => {
                depth -= 1;
                i += 1;
            }
            ',' => {
                if depth == 1 {
                    pending = None;
                }
                i += 1;
            }
            _ if depth == 1 && word_at(i, "constraint") => {
                i += "constraint".len();
                while i < b.len() && b[i].is_whitespace() {
                    i += 1;
                }
                let (name, next) = read_name(&b, i);
                pending = Some(name);
                i = next;
            }
            _ if depth == 1 && word_at(i, "check") => {
                let mut j = i + "check".len();
                while j < b.len() && b[j].is_whitespace() {
                    j += 1;
                }
                if j < b.len() && b[j] == '(' {
                    let from = j + 1;
                    let mut d = 1;
                    let mut k = from;
                    let mut quote: Option<char> = None;
                    while k < b.len() {
                        match (quote, b[k]) {
                            (Some(q), ch) if ch == q => quote = None,
                            (Some(_), _) => {}
                            (None, '\'' | '"' | '`') => quote = Some(b[k]),
                            (None, '(') => d += 1,
                            (None, ')') => {
                                d -= 1;
                                if d == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    let expr: String = b[from..k.min(b.len())].iter().collect();
                    out.push((pending.take().unwrap_or_default(), expr.trim().to_string()));
                    i = (k + 1).min(b.len());
                } else {
                    i += "check".len();
                }
            }
            _ => i += 1,
        }
    }
    out
}

fn read_name(b: &[char], i: usize) -> (String, usize) {
    if i >= b.len() {
        return (String::new(), i);
    }
    let close = match b[i] {
        '"' => Some('"'),
        '`' => Some('`'),
        '[' => Some(']'),
        '\'' => Some('\''),
        _ => None,
    };
    match close {
        Some(q) => {
            let mut j = i + 1;
            while j < b.len() && b[j] != q {
                j += 1;
            }
            (
                b[i + 1..j.min(b.len())].iter().collect(),
                (j + 1).min(b.len()),
            )
        }
        None => {
            let mut j = i;
            while j < b.len() && (b[j].is_alphanumeric() || b[j] == '_' || b[j] == '$') {
                j += 1;
            }
            (b[i..j].iter().collect(), j)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_check_strips_clause_and_parens() {
        assert_eq!(bare_check("CHECK ((price > 0))"), "price > 0");
        assert_eq!(bare_check("(`a` > 0)"), "`a` > 0");
        assert_eq!(bare_check("(a > 0) AND (b < 1)"), "(a > 0) AND (b < 1)");
        assert_eq!(
            bare_check("CHECK ((a > 0) AND (b < 1))"),
            "(a > 0) AND (b < 1)"
        );
        assert_eq!(bare_check("x IN ('(', ')')"), "x IN ('(', ')')");
    }

    #[test]
    fn oracle_not_null_checks_are_recognised() {
        assert!(is_not_null_check("\"ID\" IS NOT NULL"));
        assert!(!is_not_null_check("QTY > 0"));
        assert!(!is_not_null_check(
            "\"A\" IS NOT NULL AND \"B\" IS NOT NULL"
        ));
    }

    #[test]
    fn sqlite_checks_are_parsed() {
        let sql = "CREATE TABLE t (\n id INTEGER PRIMARY KEY,\n qty INT CHECK (qty > 0),\n note TEXT DEFAULT 'CHECK (x)',\n \
                   -- CHECK (nope)\n CONSTRAINT price_ok CHECK (price >= 0 AND price < (10)),\n CHECK (length(note) < 'a)b'))";
        let c = sqlite_checks(sql);
        assert_eq!(
            c,
            vec![
                (String::new(), "qty > 0".to_string()),
                (
                    "price_ok".to_string(),
                    "price >= 0 AND price < (10)".to_string()
                ),
                (String::new(), "length(note) < 'a)b'".to_string()),
            ]
        );
        assert!(sqlite_checks("CREATE TABLE t (checked INT)").is_empty());
    }
}
