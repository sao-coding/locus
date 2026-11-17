//! PostgreSQL-aware text scanning: script splitting and `?` placeholders.
//!
//! `db_core::sql::split_statements` follows MySQL rules (`#` comments,
//! backslash escapes, no dollar quoting), which break PostgreSQL scripts
//! (`jsonb #> '{a}'`, function bodies in `$$ ... $$`). This scanner knows:
//! `'..'` with `''` (and backslash escapes only in `E'..'`), `".."`,
//! dollar quotes `$tag$ .. $tag$`, `--` comments and nested `/* */`.

use std::ops::Range;

/// Calls `f(i)` for every byte outside strings, identifiers and comments.
fn scan(sql: &str, mut f: impl FnMut(usize)) {
    let b = sql.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        let c = b[i];
        match c {
            b'\'' => {
                let escapes =
                    i > 0 && matches!(b[i - 1], b'e' | b'E') && (i < 2 || !is_word(b[i - 2]));
                i = quoted(b, i, b'\'', escapes);
            }
            b'"' => i = quoted(b, i, b'"', false),
            b'-' if b.get(i + 1) == Some(&b'-') => i = sql[i..].find('\n').map_or(n, |p| i + p),
            b'/' if b.get(i + 1) == Some(&b'*') => i = block_comment(b, i),
            b'$' => {
                // $tag$ ... $tag$ (tag may be empty); `$1` is a parameter.
                let tag_end = b[i + 1..]
                    .iter()
                    .position(|x| !is_word(*x))
                    .map(|p| i + 1 + p)
                    .unwrap_or(n);
                let first_digit = b.get(i + 1).is_some_and(u8::is_ascii_digit);
                if b.get(tag_end) == Some(&b'$') && !first_digit && !(i > 0 && is_word(b[i - 1])) {
                    let tag = &sql[i..=tag_end];
                    i = sql[tag_end + 1..]
                        .find(tag)
                        .map_or(n, |p| tag_end + 1 + p + tag.len());
                } else {
                    f(i);
                    i += 1;
                }
            }
            _ => {
                f(i);
                i += 1;
            }
        }
    }
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

fn quoted(b: &[u8], start: usize, q: u8, backslash: bool) -> usize {
    let mut j = start + 1;
    while j < b.len() {
        if backslash && b[j] == b'\\' {
            j += 2;
        } else if b[j] == q {
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

fn block_comment(b: &[u8], start: usize) -> usize {
    let (mut depth, mut j) = (0, start);
    while j < b.len() {
        if b[j] == b'/' && b.get(j + 1) == Some(&b'*') {
            depth += 1;
            j += 2;
        } else if b[j] == b'*' && b.get(j + 1) == Some(&b'/') {
            depth -= 1;
            j += 2;
            if depth == 0 {
                return j;
            }
        } else {
            j += 1;
        }
    }
    b.len()
}

/// Byte ranges of the statements of a script (no terminating `;`, trimmed,
/// comment-only chunks dropped).
pub fn split(sql: &str) -> Vec<Range<usize>> {
    let mut cuts = Vec::new();
    scan(sql, |i| {
        if sql.as_bytes()[i] == b';' {
            cuts.push(i);
        }
    });
    cuts.push(sql.len());
    let mut out = Vec::new();
    let mut start = 0;
    for end in cuts {
        let chunk = &sql[start..end];
        if has_code(chunk) {
            let lead = chunk.len() - chunk.trim_start().len();
            out.push(start + lead..start + chunk.trim_end().len());
        }
        start = end + 1;
    }
    out
}

/// True when the chunk holds more than blanks and comments.
fn has_code(chunk: &str) -> bool {
    let mut code = false;
    let b = chunk.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            c if c.is_ascii_whitespace() => i += 1,
            b'-' if b.get(i + 1) == Some(&b'-') => {
                i = chunk[i..].find('\n').map_or(b.len(), |p| i + p)
            }
            b'/' if b.get(i + 1) == Some(&b'*') => i = block_comment(b, i),
            _ => {
                code = true;
                break;
            }
        }
    }
    code
}

/// Rewrites the host's `?` placeholders into `$1`, `$2`, ... outside strings,
/// quoted identifiers, dollar quotes and comments.
pub fn bind_placeholders(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len() + 8);
    let (mut last, mut n) = (0, 0);
    scan(sql, |i| {
        if sql.as_bytes()[i] == b'?' {
            n += 1;
            out.push_str(&sql[last..i]);
            out.push_str(&format!("${n}"));
            last = i + 1;
        }
    });
    out.push_str(&sql[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(sql: &str) -> Vec<&str> {
        split(sql).into_iter().map(|r| &sql[r]).collect()
    }

    #[test]
    fn splits_like_postgres() {
        assert_eq!(
            parts("select 1; select 2;\n select 3"),
            ["select 1", "select 2", "select 3"]
        );
        assert_eq!(
            parts("select 'a;b', \"c;d\", E'x\\';y'; select 2"),
            ["select 'a;b', \"c;d\", E'x\\';y'", "select 2"]
        );
        // a backslash is not an escape in a plain string
        assert_eq!(
            parts("select 'a\\'; select 2"),
            ["select 'a\\'", "select 2"]
        );
        assert_eq!(
            parts("select '{}'::jsonb #> '{a}'; select 2"),
            ["select '{}'::jsonb #> '{a}'", "select 2"]
        );
        let f = "create function f() returns int as $$ begin select 1; return 1; end $$ language plpgsql; select 2";
        assert_eq!(parts(f).len(), 2);
        assert!(parts(f)[0].ends_with("plpgsql"));
        assert_eq!(
            parts("select $t$ a;b $t$; select $1"),
            ["select $t$ a;b $t$", "select $1"]
        );
        assert_eq!(
            parts("-- c;\nselect 1; /* a /* nested ; */ ; */ select 2 -- t;"),
            ["-- c;\nselect 1", "/* a /* nested ; */ ; */ select 2 -- t;"]
        );
        assert_eq!(parts("  ;; -- nothing\n"), Vec::<&str>::new());
        assert_eq!(
            parts("select '終;' ; select 2"),
            ["select '終;'", "select 2"]
        );
    }

    #[test]
    fn placeholders() {
        assert_eq!(
            bind_placeholders("update t set a = ? where b = ?"),
            "update t set a = $1 where b = $2"
        );
        assert_eq!(
            bind_placeholders(
                "select '?', \"c?\", $$?$$, $q$?$q$ -- ?\n from t where x = ? /* ? */"
            ),
            "select '?', \"c?\", $$?$$, $q$?$q$ -- ?\n from t where x = $1 /* ? */"
        );
        assert_eq!(bind_placeholders("select $1, ?"), "select $1, $1");
    }
}
