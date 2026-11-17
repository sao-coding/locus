//! The result strip's row filter: `column op value` (`=`, `!=`, `>`, `<`,
//! `>=`, `<=`, `LIKE`, `IS [NOT] NULL`) or free text matched against every
//! column. Pure: the grid feeds it cells, nothing here touches the UI.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    Like,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Filter {
    Cmp {
        col: usize,
        op: Op,
        value: String,
        num: Option<f64>,
        like: Vec<char>,
    },
    IsNull {
        col: usize,
        not: bool,
    },
    /// Case-insensitive substring in any column.
    Text {
        needle: String,
    },
}

/// What a typed filter turned into (shown under the input as a hint).
#[derive(Clone, Debug, PartialEq)]
pub enum Parsed {
    Empty,
    Filter(Filter),
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || c == '.'
}

/// Reads a column name at the start of `s`: bare, `` `quoted` `` or `"quoted"`.
fn take_column(s: &str) -> Option<(String, &str)> {
    let mut it = s.char_indices();
    let (_, first) = it.next()?;
    if first == '`' || first == '"' {
        let rest = &s[1..];
        let end = rest.find(first)?;
        return Some((rest[..end].to_string(), &rest[end + 1..]));
    }
    let end = s
        .char_indices()
        .find(|&(_, c)| !is_ident(c))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    Some((s[..end].to_string(), &s[end..]))
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    for q in ['\'', '"'] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return v[1..v.len() - 1].replace(&format!("{q}{q}"), &q.to_string());
        }
    }
    v.to_string()
}

fn find_col(columns: &[&str], name: &str) -> Option<usize> {
    columns
        .iter()
        .position(|c| c.eq_ignore_ascii_case(name))
        .or_else(|| {
            columns
                .iter()
                .position(|c| c.to_lowercase() == name.to_lowercase())
        })
}

/// Parses `input` against the result's column names. Anything that is not
/// `column op value` with a known column is free text.
pub fn parse(input: &str, columns: &[&str]) -> Parsed {
    let input = input.trim();
    if input.is_empty() {
        return Parsed::Empty;
    }
    if let Some((name, rest)) = take_column(input)
        && let Some(col) = find_col(columns, &name)
    {
        let rest = rest.trim_start();
        let upper = rest.to_ascii_uppercase();
        // IS [NOT] NULL
        if let Some(after) = upper.strip_prefix("IS ") {
            let after = after.trim();
            if after == "NULL" {
                return Parsed::Filter(Filter::IsNull { col, not: false });
            }
            if after == "NOT NULL" {
                return Parsed::Filter(Filter::IsNull { col, not: true });
            }
        }
        let ops: [(&str, Op); 8] = [
            ("!=", Op::Ne),
            ("<>", Op::Ne),
            (">=", Op::Ge),
            ("<=", Op::Le),
            ("=", Op::Eq),
            (">", Op::Gt),
            ("<", Op::Lt),
            ("", Op::Like),
        ];
        for (sym, op) in ops {
            if op == Op::Like {
                // `LIKE` needs a space or quote after it.
                if upper.starts_with("LIKE")
                    && rest.len() > 4
                    && rest[4..].starts_with(|c: char| c.is_whitespace() || c == '\'' || c == '"')
                {
                    return Parsed::Filter(cmp(col, Op::Like, unquote(&rest[4..])));
                }
                break;
            }
            if let Some(v) = rest.strip_prefix(sym) {
                return Parsed::Filter(cmp(col, op, unquote(v)));
            }
        }
    }
    Parsed::Filter(Filter::Text {
        needle: input.to_lowercase(),
    })
}

fn cmp(col: usize, op: Op, value: String) -> Filter {
    let num = value.trim().parse::<f64>().ok().filter(|n| n.is_finite());
    let like = if op == Op::Like {
        value.to_lowercase().chars().collect()
    } else {
        Vec::new()
    };
    Filter::Cmp {
        col,
        op,
        value,
        num,
        like,
    }
}

impl Filter {
    /// `get(c)` is column `c` of the row: `None` = NULL.
    pub fn matches<'a>(&self, columns: usize, get: impl Fn(usize) -> Option<&'a str>) -> bool {
        match self {
            Filter::IsNull { col, not } => get(*col).is_none() != *not,
            Filter::Text { needle } => {
                (0..columns).any(|c| get(c).is_some_and(|s| contains_ci(s, needle)))
            }
            Filter::Cmp {
                col,
                op,
                value,
                num,
                like,
            } => {
                let Some(cell) = get(*col) else { return false };
                match op {
                    Op::Like => like_match(like, cell),
                    _ => {
                        let ord = match (num, cell.trim().parse::<f64>().ok()) {
                            (Some(a), Some(b)) => b.partial_cmp(a),
                            _ => Some(cell.cmp(value.as_str())),
                        };
                        let Some(ord) = ord else { return false };
                        use std::cmp::Ordering::*;
                        match op {
                            Op::Eq => {
                                ord == Equal || (num.is_none() && cell.eq_ignore_ascii_case(value))
                            }
                            Op::Ne => {
                                ord != Equal && !(num.is_none() && cell.eq_ignore_ascii_case(value))
                            }
                            Op::Gt => ord == Greater,
                            Op::Lt => ord == Less,
                            Op::Ge => ord != Less,
                            Op::Le => ord != Greater,
                            Op::Like => unreachable!(),
                        }
                    }
                }
            }
        }
    }
}

/// `needle` is already lower case.
fn contains_ci(hay: &str, needle: &str) -> bool {
    if needle.is_ascii() {
        let (h, n) = (hay.as_bytes(), needle.as_bytes());
        if n.is_empty() {
            return true;
        }
        if h.len() < n.len() {
            return false;
        }
        let first = n[0];
        for i in 0..=h.len() - n.len() {
            if h[i].to_ascii_lowercase() == first
                && h[i..i + n.len()]
                    .iter()
                    .zip(n)
                    .all(|(a, b)| a.to_ascii_lowercase() == *b)
            {
                return true;
            }
        }
        false
    } else {
        hay.to_lowercase().contains(needle)
    }
}

/// SQL `LIKE` with `%`, `_` and `\` escapes; `pat` is lower case, matching ignores case.
fn like_match(pat: &[char], text: &str) -> bool {
    let t: Vec<char> = text.chars().flat_map(|c| c.to_lowercase()).collect();
    // Iterative wildcard match with one backtrack point.
    let (mut p, mut s) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while s < t.len() {
        if p < pat.len() {
            match pat[p] {
                '%' => {
                    star = Some((p, s));
                    p += 1;
                    continue;
                }
                '_' => {
                    p += 1;
                    s += 1;
                    continue;
                }
                '\\' if p + 1 < pat.len() => {
                    if pat[p + 1] == t[s] {
                        p += 2;
                        s += 1;
                        continue;
                    }
                }
                c if c == t[s] => {
                    p += 1;
                    s += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((sp, ss)) => {
                p = sp + 1;
                s = ss + 1;
                star = Some((sp, ss + 1));
            }
            None => return false,
        }
    }
    while p < pat.len() && pat[p] == '%' {
        p += 1;
    }
    p == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLS: [&str; 4] = ["id", "name", "status", "note"];

    fn f(input: &str) -> Filter {
        match parse(input, &COLS) {
            Parsed::Filter(f) => f,
            Parsed::Empty => panic!("empty"),
        }
    }

    fn row<'a>(v: &'a [Option<&'a str>]) -> impl Fn(usize) -> Option<&'a str> {
        move |c| v[c]
    }

    #[::core::prelude::v1::test]
    fn empty_is_empty() {
        assert_eq!(parse("   ", &COLS), Parsed::Empty);
    }

    #[::core::prelude::v1::test]
    fn operators_parse() {
        for (text, op) in [
            ("id = 5", Op::Eq),
            ("id=5", Op::Eq),
            ("id != 5", Op::Ne),
            ("id <> 5", Op::Ne),
            ("id > 5", Op::Gt),
            ("id < 5", Op::Lt),
            ("id >= 5", Op::Ge),
            ("id <= 5", Op::Le),
            ("name LIKE 'a%'", Op::Like),
            ("name like a%", Op::Like),
        ] {
            match f(text) {
                Filter::Cmp { op: o, .. } => assert_eq!(o, op, "{text}"),
                other => panic!("{text}: {other:?}"),
            }
        }
    }

    #[::core::prelude::v1::test]
    fn unknown_column_and_plain_words_are_free_text() {
        assert_eq!(
            f("zzz = 5"),
            Filter::Text {
                needle: "zzz = 5".into()
            }
        );
        assert_eq!(
            f("Paid"),
            Filter::Text {
                needle: "paid".into()
            }
        );
        // A column name alone is text too.
        assert_eq!(
            f("status"),
            Filter::Text {
                needle: "status".into()
            }
        );
    }

    #[::core::prelude::v1::test]
    fn compare_numbers_as_numbers() {
        let r = [Some("10"), None, None, None];
        assert!(f("id > 9").matches(4, row(&r)));
        assert!(!f("id > 10").matches(4, row(&r)));
        assert!(f("id >= 10").matches(4, row(&r)));
        assert!(f("id = 10.0").matches(4, row(&r)));
        assert!(f("id != 11").matches(4, row(&r)));
        assert!(f("id < '11'").matches(4, row(&r)));
    }

    #[::core::prelude::v1::test]
    fn compare_text_and_quotes() {
        let r = [None, Some("Ann Lee"), Some("paid"), Some("it's")];
        assert!(f("status = 'paid'").matches(4, row(&r)));
        assert!(f("STATUS = PAID").matches(4, row(&r)));
        assert!(f("status != 'open'").matches(4, row(&r)));
        assert!(f("name = \"Ann Lee\"").matches(4, row(&r)));
        assert!(f("note = 'it''s'").matches(4, row(&r)));
        assert!(f("`name` = 'Ann Lee'").matches(4, row(&r)));
    }

    #[::core::prelude::v1::test]
    fn like_wildcards() {
        let r = [None, Some("Ann Lee"), None, Some("100%")];
        assert!(f("name like 'ann%'").matches(4, row(&r)));
        assert!(f("name like '%LEE'").matches(4, row(&r)));
        assert!(f("name like 'A_n%'").matches(4, row(&r)));
        assert!(f("name like '%n L%'").matches(4, row(&r)));
        assert!(!f("name like 'Bob%'").matches(4, row(&r)));
        assert!(!f("name like 'Ann'").matches(4, row(&r)));
        assert!(f("note like '100\\%'").matches(4, row(&r)));
        assert!(f("name like '%'").matches(4, row(&r)));
    }

    #[::core::prelude::v1::test]
    fn null_handling() {
        let r = [Some("1"), None, None, None];
        // SQL semantics: a comparison with NULL is never true.
        assert!(!f("name = 'x'").matches(4, row(&r)));
        assert!(!f("name != 'x'").matches(4, row(&r)));
        assert!(f("name is null").matches(4, row(&r)));
        assert!(!f("id IS NULL").matches(4, row(&r)));
        assert!(f("id is not null").matches(4, row(&r)));
    }

    #[::core::prelude::v1::test]
    fn free_text_any_column_case_insensitive() {
        let r = [Some("7"), Some("Zoë"), Some("Paid"), None];
        assert!(f("paid").matches(4, row(&r)));
        assert!(f("PAI").matches(4, row(&r)));
        assert!(f("zoë").matches(4, row(&r)));
        assert!(f("ZOË").matches(4, row(&r)));
        assert!(!f("open").matches(4, row(&r)));
        assert!(f("7").matches(4, row(&r)));
    }

    #[::core::prelude::v1::test]
    fn chinese_and_fullwidth_text() {
        let r = [Some("1"), Some("王小明"), Some("已付款"), Some("ＡＢＣ")];
        assert!(f("小明").matches(4, row(&r)));
        assert!(f("已付").matches(4, row(&r)));
        assert!(!f("李").matches(4, row(&r)));
        assert!(f("ａｂｃ").matches(4, row(&r))); // full-width, case-folded
        assert!(f("name = 王小明").matches(4, row(&r)));
        assert!(f("name like '王%'").matches(4, row(&r)));
    }

    #[::core::prelude::v1::test]
    fn empty_string_is_not_null() {
        let r = [Some("1"), Some(""), None, None];
        assert!(f("name = ''").matches(4, row(&r)));
        assert!(!f("name is null").matches(4, row(&r)));
        assert!(f("name is not null").matches(4, row(&r)));
        assert!(!f("status = ''").matches(4, row(&r))); // NULL never equals
    }

    #[::core::prelude::v1::test]
    fn free_text_spans_columns_and_skips_null() {
        // Free text hits when any single column contains it.
        let r = [Some("12"), Some("Ann"), None, Some("late")];
        assert!(f("ann").matches(4, row(&r)));
        assert!(f("late").matches(4, row(&r)));
        assert!(!f("ann late").matches(4, row(&r)));
    }

    #[::core::prelude::v1::test]
    fn index_count_on_100k_rows() {
        let filter = f("status = 'paid'");
        let hits: Vec<u32> = (0..100_000u32)
            .filter(|i| {
                let id = i.to_string();
                let st = if i % 4 == 0 { "PAID" } else { "open" };
                filter.matches(4, |c| match c {
                    0 => Some(id.as_str()),
                    2 => Some(st),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(hits.len(), 25_000);
        assert_eq!(hits[1], 4);
    }
}
