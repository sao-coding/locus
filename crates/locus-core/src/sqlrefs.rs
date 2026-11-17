//! Table references of a SQL statement (autocomplete's `alias.` lookup).

pub fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

pub fn unquote(s: &str) -> String {
    s.trim_matches('`').trim_matches('"').to_string()
}

/// `(alias or table) -> (schema, table)` from `FROM a [AS] x, b y JOIN c z` in `stmt`.
pub fn table_refs(stmt: &str) -> Vec<(String, Option<String>, String)> {
    let toks: Vec<String> = stmt
        .split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')' || c == ';')
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    let mut out = Vec::new();
    let stop = |t: &str| {
        matches!(
            t.to_ascii_uppercase().as_str(),
            "WHERE"
                | "ON"
                | "USING"
                | "GROUP"
                | "ORDER"
                | "HAVING"
                | "LIMIT"
                | "JOIN"
                | "LEFT"
                | "RIGHT"
                | "INNER"
                | "OUTER"
                | "CROSS"
                | "UNION"
                | "SET"
                | "FROM"
                | "STRAIGHT_JOIN"
        )
    };
    let mut i = 0;
    while i < toks.len() {
        let up = toks[i].to_ascii_uppercase();
        if up == "FROM" || up == "JOIN" || up == "UPDATE" || up == "INTO" {
            let mut j = i + 1;
            // Comma-separated list after FROM; a single table after JOIN.
            while let Some(name) = toks.get(j) {
                if stop(name) || name.starts_with("SELECT") || name.eq_ignore_ascii_case("select") {
                    break;
                }
                let (schema, table) = match name.split_once('.') {
                    Some((s, t)) => (Some(unquote(s)), unquote(t)),
                    None => (None, unquote(name)),
                };
                let mut alias = table.clone();
                let mut k = j + 1;
                if toks.get(k).is_some_and(|t| t.eq_ignore_ascii_case("AS")) {
                    k += 1;
                }
                if let Some(a) = toks.get(k)
                    && !stop(a)
                    && a.chars().all(|c| is_word(c) || c == '`')
                {
                    alias = unquote(a);
                    k += 1;
                }
                out.push((alias, schema, table));
                // Next item of a FROM list only when the source had a comma.
                let after = stmt.contains(',');
                if up == "FROM" && after && toks.get(k).is_some_and(|t| !stop(t)) {
                    j = k;
                    continue;
                }
                break;
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn aliases() {
        let r = table_refs(
            "SELECT s.x FROM bench.student s JOIN wb_t AS t ON s.id = t.id, other o WHERE 1",
        );
        assert!(r.contains(&("s".into(), Some("bench".into()), "student".into())));
        assert!(r.contains(&("t".into(), None, "wb_t".into())));
        let r = table_refs("select * from `a` where");
        assert_eq!(r, vec![("a".into(), None, "a".into())]);
    }
}
