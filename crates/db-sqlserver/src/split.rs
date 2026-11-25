//! Script text helpers for T-SQL: `GO` batch separators and `?` placeholders.

/// Splits a script into batches on lines that hold only `GO` (any case,
/// optional trailing `;` / spaces), the way SSMS and sqlcmd do. `GO` is a
/// client-side separator; the server never sees it. Lines inside string
/// literals or block comments are not separators. Empty batches are dropped.
pub(crate) fn batches(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut state = State::Code;
    for line in sql.split_inclusive('\n') {
        if state == State::Code && is_go(line) {
            push(&mut out, &mut cur);
            continue;
        }
        state = scan(line, state);
        cur.push_str(line);
    }
    push(&mut out, &mut cur);
    out
}

fn push(out: &mut Vec<String>, cur: &mut String) {
    if !cur.trim().is_empty() {
        out.push(std::mem::take(cur));
    } else {
        cur.clear();
    }
}

fn is_go(line: &str) -> bool {
    let t = line.trim().trim_end_matches(';').trim_end();
    t.eq_ignore_ascii_case("go")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Code,
    Str,
    Block(u32),
}

/// State at the end of `line`, starting in `state`.
fn scan(line: &str, mut state: State) -> State {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match state {
            State::Code => match b[i] {
                b'\'' => state = State::Str,
                b'-' if b.get(i + 1) == Some(&b'-') => return State::Code,
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    state = State::Block(1);
                    i += 1;
                }
                _ => {}
            },
            State::Str => {
                if b[i] == b'\'' {
                    if b.get(i + 1) == Some(&b'\'') {
                        i += 1;
                    } else {
                        state = State::Code;
                    }
                }
            }
            State::Block(n) => {
                if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    state = if n == 1 {
                        State::Code
                    } else {
                        State::Block(n - 1)
                    };
                    i += 1;
                } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    state = State::Block(n + 1);
                    i += 1;
                }
            }
        }
        i += 1;
    }
    state
}

/// Rewrites `?` placeholders (outside strings, `[names]`, `"names"` and
/// comments) to `@P1`, `@P2`, ... in order.
pub(crate) fn bind_placeholders(sql: &str) -> String {
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len() + 8);
    let mut n = 0;
    let mut i = 0;
    let mut last = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < b.len() {
                    if b[i] == q {
                        if b.get(i + 1) == Some(&q) {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    i += 1;
                }
            }
            b'[' => {
                while i < b.len() && b[i] != b']' {
                    i += 1;
                }
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 0;
                while i < b.len() {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 1;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    i += 1;
                }
            }
            b'?' => {
                n += 1;
                out.push_str(&sql[last..i]);
                out.push_str(&format!("@P{n}"));
                last = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push_str(&sql[last.min(sql.len())..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_splits_batches() {
        let b = batches("select 1\nGO\nselect 2\n go ; \nselect 3");
        assert_eq!(b, ["select 1\n", "select 2\n", "select 3"]);
        assert_eq!(batches("select 'a\ngo\nb'"), ["select 'a\ngo\nb'"]);
        assert_eq!(batches("/* x\ngo\n*/ select 1"), ["/* x\ngo\n*/ select 1"]);
        assert_eq!(batches("select 1 -- c\ngo\n"), ["select 1 -- c\n"]);
        assert!(batches("go\n\ngo").is_empty());
        assert_eq!(batches("select gone\n"), ["select gone\n"]);
    }

    #[test]
    fn placeholders_become_numbered() {
        assert_eq!(
            bind_placeholders("update t set a = ?, [b?] = '?' where c = ? -- ?\n"),
            "update t set a = @P1, [b?] = '?' where c = @P2 -- ?\n"
        );
        assert_eq!(
            bind_placeholders("select '?''?' , ? /* ? */"),
            "select '?''?' , @P1 /* ? */"
        );
    }
}
