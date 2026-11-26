//! Client-side variables, after DBeaver's "client side commands":
//!
//! | Syntax | Meaning |
//! |---|---|
//! | `@set name = value` | defines `name` (the rest of the line, trimmed) |
//! | `@unset name` | removes it |
//! | `@echo text` | prints `text` to the Messages tab |
//! | `${name}` | replaced by the value |
//!
//! **A variable is text substitution**, done on the client before the
//! statement is classified, audited or sent: `${t}` becomes the characters of
//! `t`, so it can be a table name, a keyword or a whole clause. That is the
//! opposite of a parameter (`:name`, `@name`, see `params`), which is bound as
//! a value by the driver and can never change the statement. Because the
//! substitution happens first, the Production guard and the audit log see the
//! statement as it is sent, and a variable cannot hide a `DROP` from them.
//!
//! Command lines are never sent to the database. `@include` is not supported.

use std::collections::BTreeMap;

/// Variables by name, kept in name order.
pub type Vars = BTreeMap<String, String>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cmd {
    Set { name: String, value: String },
    Unset(String),
    Echo(String),
}

fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The command on one `@set` / `@unset` / `@echo` line. `None` when the line is
/// not a command, `Some(Err(line))` when it is one but malformed (no name).
pub fn parse_command(line: &str) -> Option<Result<Cmd, String>> {
    let t = line.trim();
    let rest = t.strip_prefix('@')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let (cmd, args) = (&rest[..end], rest[end..].trim());
    let name_of = |args: &str| -> Option<(String, String)> {
        let n: String = args.chars().take_while(|c| word(*c)).collect();
        (!n.is_empty()).then(|| (n.clone(), args[n.len()..].to_string()))
    };
    let bad = || Some(Err(t.to_string()));
    match cmd.to_ascii_lowercase().as_str() {
        "set" => {
            let Some((name, tail)) = name_of(args) else {
                return bad();
            };
            let tail = tail.trim_start();
            let value = tail
                .strip_prefix(":=")
                .or_else(|| tail.strip_prefix('='))
                .unwrap_or(tail)
                .trim();
            // A habitual closing `;` is not part of the value.
            let value = value.strip_suffix(';').unwrap_or(value).trim_end();
            Some(Ok(Cmd::Set {
                name,
                value: value.to_string(),
            }))
        }
        "unset" => match name_of(args) {
            Some((name, _)) => Some(Ok(Cmd::Unset(name))),
            None => bad(),
        },
        "echo" => Some(Ok(Cmd::Echo(args.to_string()))),
        _ => None,
    }
}

/// `${name}` references of `text` (outside `--` and `/* */` comments) as
/// (byte range, name), in order.
pub fn refs(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'-' if b.get(i + 1) == Some(&b'-') => {
                i = b[i..]
                    .iter()
                    .position(|x| *x == b'\n')
                    .map_or(b.len(), |p| i + p + 1);
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..].find("*/").map_or(b.len(), |p| i + 2 + p + 2);
            }
            b'$' if b.get(i + 1) == Some(&b'{') => {
                let name: String = text[i + 2..].chars().take_while(|c| word(*c)).collect();
                let end = i + 2 + name.len();
                if !name.is_empty() && b.get(end) == Some(&b'}') {
                    out.push((i..end + 1, name));
                    i = end + 1;
                } else {
                    i += 2;
                }
            }
            _ => i += text[i..].chars().next().map_or(1, char::len_utf8),
        }
    }
    out
}

/// `text` with every defined `${name}` replaced; the names without a value, in
/// order and distinct, are returned and stay in the text as written.
pub fn substitute(text: &str, vars: &Vars) -> (String, Vec<String>) {
    let mut out = String::with_capacity(text.len());
    let mut undefined: Vec<String> = Vec::new();
    let mut at = 0;
    for (r, name) in refs(text) {
        out.push_str(&text[at..r.start]);
        match vars.get(&name) {
            Some(v) => out.push_str(v),
            None => {
                out.push_str(&text[r.clone()]);
                if !undefined.contains(&name) {
                    undefined.push(name);
                }
            }
        }
        at = r.end;
    }
    out.push_str(&text[at..]);
    (out, undefined)
}

/// The text typed after an open `${` when the cursor is inside it (only name
/// characters since the `${`), for completing variable names.
pub fn completion_prefix(before: &str) -> Option<&str> {
    let open = before.rfind("${")?;
    let typed = &before[open + 2..];
    typed.chars().all(word).then_some(typed)
}

/// Names defined by `@set` lines in `text`, in order of first appearance.
pub fn defined_names(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in db_core::sql::split_statements(text) {
        if let Some(Ok(Cmd::Set { name, .. })) = parse_command(&text[r])
            && !out.contains(&name)
        {
            out.push(name);
        }
    }
    out
}

/// A script after its client commands ran and its variables were substituted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prepared {
    /// What is classified, audited and sent: no command lines, no `${}` left
    /// except the undefined ones.
    pub sql: String,
    /// The variables after the commands ran (to keep for the tab).
    pub vars: Vars,
    /// Lines of `@echo`, substituted.
    pub echoes: Vec<String>,
    /// `${name}` without a value (the Run dialog asks for them).
    pub undefined: Vec<String>,
    /// Malformed command lines.
    pub errors: Vec<String>,
    /// The script had at least one command line.
    pub had_commands: bool,
}

/// Runs the command lines of `sql` in order against `vars` and substitutes the
/// rest. A variable defined by `@set` is visible to everything after it and,
/// through `Prepared::vars`, to later runs of the tab.
pub fn prepare(sql: &str, vars: &Vars) -> Prepared {
    let mut p = Prepared {
        vars: vars.clone(),
        ..Default::default()
    };
    let mut at = 0;
    let mut text = String::new();
    let mut undefined: Vec<String> = Vec::new();
    let mut note = |names: Vec<String>| {
        for n in names {
            if !undefined.contains(&n) {
                undefined.push(n);
            }
        }
    };
    for r in db_core::sql::split_statements(sql) {
        let Some(cmd) = parse_command(&sql[r.clone()]) else {
            continue;
        };
        p.had_commands = true;
        let (chunk, und) = substitute(&sql[at..r.start], &p.vars);
        text.push_str(&chunk);
        note(und);
        at = r.end;
        match cmd {
            Err(line) => p.errors.push(line),
            Ok(Cmd::Set { name, value }) => {
                let (value, und) = substitute(&value, &p.vars);
                note(und);
                p.vars.insert(name, value);
            }
            Ok(Cmd::Unset(name)) => {
                p.vars.remove(&name);
            }
            Ok(Cmd::Echo(msg)) => {
                let (msg, und) = substitute(&msg, &p.vars);
                note(und);
                p.echoes.push(msg);
            }
        }
    }
    let (chunk, und) = substitute(&sql[at..], &p.vars);
    text.push_str(&chunk);
    note(und);
    p.sql = if p.had_commands {
        text.trim().to_string()
    } else {
        text
    };
    p.undefined = undefined;
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::{Decision, decide};
    use crate::store::Env;
    use db_core::classify::classify_script;

    fn vars(kv: &[(&str, &str)]) -> Vars {
        kv.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parse_set_forms() {
        let set = |l: &str| parse_command(l);
        assert_eq!(
            set("@set a = 1"),
            Some(Ok(Cmd::Set {
                name: "a".into(),
                value: "1".into()
            }))
        );
        assert_eq!(
            set("  @SET  tbl=booking_rec  "),
            Some(Ok(Cmd::Set {
                name: "tbl".into(),
                value: "booking_rec".into()
            }))
        );
        // No `=`: the rest of the line is the value; a closing `;` is dropped.
        assert_eq!(
            set("@set w where id > 1;"),
            Some(Ok(Cmd::Set {
                name: "w".into(),
                value: "where id > 1".into()
            }))
        );
        // The value is the rest of the line, including `=` and quotes.
        assert_eq!(
            set("@set c = a = 'x y'"),
            Some(Ok(Cmd::Set {
                name: "c".into(),
                value: "a = 'x y'".into()
            }))
        );
        assert_eq!(
            set("@set e ="),
            Some(Ok(Cmd::Set {
                name: "e".into(),
                value: String::new()
            }))
        );
        assert!(matches!(set("@set"), Some(Err(_))));
        assert!(matches!(set("@set = 1"), Some(Err(_))));
        assert_eq!(set("@unset a"), Some(Ok(Cmd::Unset("a".into()))));
        assert_eq!(
            set("@echo hi  there"),
            Some(Ok(Cmd::Echo("hi  there".into())))
        );
        assert_eq!(set("@include x.sql"), None);
        assert_eq!(set("select 1"), None);
    }

    #[test]
    fn refs_skip_comments_and_bad_forms() {
        let t = "select ${a}, '${b}' -- ${c}\n, /* ${d} */ ${e}, ${}, ${ f}, ${g";
        let names: Vec<String> = refs(t).into_iter().map(|x| x.1).collect();
        assert_eq!(names, ["a", "b", "e"]);
    }

    #[test]
    fn substitute_is_plain_text() {
        let v = vars(&[("t", "booking_rec"), ("w", "id = 1 or 1 = 1")]);
        let (s, und) = substitute(
            "select * from ${t} where ${w} and x = ${nope} or ${nope}",
            &v,
        );
        assert_eq!(
            s,
            "select * from booking_rec where id = 1 or 1 = 1 and x = ${nope} or ${nope}"
        );
        assert_eq!(und, ["nope"]);
        // Unicode names and values.
        let (s, _) = substitute("select ${名稱}", &vars(&[("名稱", "'測試'")]));
        assert_eq!(s, "select '測試'");
    }

    #[test]
    fn prepare_runs_commands_in_order() {
        let sql = "@set tbl = booking_rec\n@set col = id\nselect ${col} from ${tbl} where id = :id";
        let p = prepare(sql, &Vars::new());
        assert_eq!(p.sql, "select id from booking_rec where id = :id");
        assert_eq!(p.vars, vars(&[("col", "id"), ("tbl", "booking_rec")]));
        assert!(p.undefined.is_empty() && p.errors.is_empty() && p.had_commands);
    }

    #[test]
    fn prepare_values_can_reference_variables() {
        let p = prepare(
            "@set a = people\n@set b = ${a}_old\nselect * from ${b}",
            &Vars::new(),
        );
        assert_eq!(p.sql, "select * from people_old");
        assert_eq!(p.vars["b"], "people_old");
    }

    #[test]
    fn prepare_uses_kept_variables_and_unset_removes() {
        let kept = vars(&[("t", "x")]);
        let p = prepare("select * from ${t}", &kept);
        assert_eq!(p.sql, "select * from x");
        assert!(!p.had_commands);
        let p = prepare("select ${t};\n@unset t\nselect ${t}", &kept);
        assert_eq!(
            p.sql,
            "select x;

select ${t}"
        );
        assert_eq!(p.undefined, ["t"]);
        assert!(p.vars.is_empty());
    }

    #[test]
    fn prepare_echo_and_errors_and_no_command_text() {
        let p = prepare("@set n = 5\n@echo n is ${n}\n@set\nselect 1", &Vars::new());
        assert_eq!(p.echoes, ["n is 5"]);
        assert_eq!(p.errors, ["@set"]);
        assert_eq!(p.sql, "select 1");
        // The whole script was commands: nothing left to send.
        assert_eq!(prepare("@set a = 1", &Vars::new()).sql, "");
        // Commands inside a statement are not commands.
        let p = prepare("select 1\n@set a = 1", &Vars::new());
        assert!(!p.had_commands);
        assert_eq!(p.sql, "select 1\n@set a = 1");
    }

    #[test]
    fn prepare_undefined_are_listed_once_in_order() {
        let p = prepare("select ${b}, ${a}, ${b} from ${c}", &Vars::new());
        assert_eq!(p.undefined, ["b", "a", "c"]);
        assert_eq!(p.sql, "select ${b}, ${a}, ${b} from ${c}");
    }

    #[test]
    fn no_substitution_in_comments() {
        let p = prepare("-- ${a}\nselect 1 /* ${b} */", &Vars::new());
        assert!(p.undefined.is_empty());
    }

    #[test]
    fn completion_helpers() {
        assert_eq!(completion_prefix("select * from ${"), Some(""));
        assert_eq!(completion_prefix("select * from ${ta"), Some("ta"));
        assert_eq!(completion_prefix("select ${a} from ${b_"), Some("b_"));
        assert_eq!(completion_prefix("select ${a} from"), None);
        assert_eq!(completion_prefix("select ${a.b"), None);
        assert_eq!(completion_prefix("select 1"), None);
        assert_eq!(
            defined_names("@set b = 1\n@set a = 2\nselect 1;\n@set b = 3\n@unset a"),
            ["b", "a"]
        );
    }
    #[test]
    fn variables_do_not_hide_a_drop_from_the_production_guard() {
        // The unsubstituted text does not say DROP ...
        let raw = "@set stmt = drop table customers\n${stmt}";
        let raw_class = classify_script("${stmt}");
        let kw = |c: &[db_core::classify::Classified]| {
            crate::guard::WriteInfo::of(c)
                .map(|w| w.keywords)
                .unwrap_or_default()
        };
        assert!(!kw(&raw_class).iter().any(|k| k == "DROP"));
        // ... the prepared statement (what the gate, the audit and the driver
        // get) is a DROP, so a locked Production connection blocks it.
        let p = prepare(raw, &Vars::new());
        assert_eq!(p.sql, "drop table customers");
        let classes = classify_script(&p.sql);
        assert!(kw(&classes).iter().any(|k| k == "DROP"));
        assert!(matches!(
            decide(Env::Prod, true, &classes, true),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            decide(Env::Prod, false, &classes, true),
            Decision::Confirm { .. }
        ));
        // Also when the variable is only part of the statement, or kept from an earlier run.
        let kept = vars(&[("what", "table"), ("tbl", "customers")]);
        let p = prepare("drop ${what} ${tbl}", &kept);
        assert!(matches!(
            decide(Env::Prod, true, &classify_script(&p.sql), true),
            Decision::Blocked(_)
        ));
    }
}
