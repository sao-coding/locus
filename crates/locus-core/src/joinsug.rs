//! JOIN completion from foreign keys (DataGrip style): after `JOIN` suggest
//! the tables that are related by a key to the tables already in the
//! statement, with the whole `ON a.x = b.y` (composite keys too); after
//! `ON` suggest the condition for the table just joined. Pure text: the
//! completion provider supplies the catalog keys and the statement before
//! the cursor.

use db_core::ForeignKey;

/// A foreign key with the schema its `table` lives in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rel {
    pub schema: String,
    pub fk: ForeignKey,
}

/// A table of the statement: alias (or the table name), schema, table.
pub type Ref = (String, Option<String>, String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinSuggestion {
    pub schema: String,
    pub table: String,
    pub alias: String,
    /// `a.x = b.y AND ...`
    pub on: String,
    /// Name of the key, for the item detail.
    pub key: String,
}

impl JoinSuggestion {
    /// Text that replaces the typed prefix after `JOIN`.
    pub fn insert_with_on(&self, q: char) -> String {
        format!("{} {} ON {}", quote(&self.table, q), self.alias, self.on)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ctx {
    /// The cursor is on the table name after `JOIN`.
    AfterJoin,
    /// The cursor is on the condition after `ON`.
    AfterOn,
}

fn simple(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_alphabetic() || f == '_')
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

fn quote(s: &str, q: char) -> String {
    if simple(s) {
        s.to_string()
    } else {
        format!("{q}{}{q}", s.replace(q, &format!("{q}{q}")))
    }
}

/// What the cursor is right after: the keyword before the word being typed
/// (`before` is the statement text up to the cursor, `prefix` the word).
pub fn context(before: &str, prefix: &str) -> Option<Ctx> {
    let head = before.strip_suffix(prefix).unwrap_or(before);
    // The text must end in whitespace: `JOIN ` / `ON `, not `JOINx`.
    if !head.ends_with(|c: char| c.is_whitespace()) {
        return None;
    }
    let last = head.split_whitespace().next_back()?.to_ascii_uppercase();
    match last.as_str() {
        "JOIN" => Some(Ctx::AfterJoin),
        "ON" => Some(Ctx::AfterOn),
        _ => None,
    }
}

/// Short alias of a table: initials of its `_` words (`order_items` -> `oi`),
/// made unique against `taken` with a number.
pub fn alias_for(table: &str, taken: &[String]) -> String {
    let mut a: String = table
        .split(['_', ' '])
        .filter_map(|w| w.chars().next())
        .flat_map(char::to_lowercase)
        .collect();
    if a.is_empty() || !simple(&a) {
        a = "t".into();
    }
    // Avoid SQL keywords commonly hit by two letters.
    if matches!(
        a.as_str(),
        "as" | "by" | "in" | "is" | "on" | "or" | "to" | "do" | "if" | "no" | "of"
    ) {
        a.push('1');
    }
    let clash = |x: &str| taken.iter().any(|t| t.eq_ignore_ascii_case(x));
    if !clash(&a) {
        return a;
    }
    (2..)
        .map(|n| format!("{a}{n}"))
        .find(|c| !clash(c))
        .unwrap()
}

fn cond(new: &str, new_cols: &[String], old: &str, old_cols: &[String], q: char) -> String {
    new_cols
        .iter()
        .zip(old_cols)
        .map(|(n, o)| format!("{new}.{} = {old}.{}", quote(n, q), quote(o, q)))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Schema match; an unknown schema (empty) matches any.
fn sch(a: &str, b: &str) -> bool {
    b.is_empty() || a.eq_ignore_ascii_case(b)
}

fn is(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Tables related by a foreign key to a table of `refs`, one suggestion per
/// key and direction. `current` is the connection's current schema (the
/// schema of an unqualified table).
pub fn suggest_tables(
    rels: &[Rel],
    refs: &[Ref],
    current: Option<&str>,
    q: char,
) -> Vec<JoinSuggestion> {
    let schema_of = |r: &Ref| {
        r.1.clone()
            .or_else(|| current.map(str::to_string))
            .unwrap_or_default()
    };
    let mut taken: Vec<String> = refs.iter().map(|r| r.0.clone()).collect();
    let mut out: Vec<JoinSuggestion> = Vec::new();
    for r in refs {
        let rs = schema_of(r);
        for rel in rels {
            let fk = &rel.fk;
            // This table holds the key: join the table it points at.
            if sch(&rel.schema, &rs) && is(&fk.table, &r.2) {
                push(
                    &mut out,
                    &mut taken,
                    rel,
                    &fk.ref_schema,
                    &fk.ref_table,
                    &fk.ref_columns,
                    &fk.columns,
                    &r.0,
                    q,
                );
            }
            // This table is pointed at: join the table that holds the key.
            if sch(&fk.ref_schema, &rs) && is(&fk.ref_table, &r.2) {
                push(
                    &mut out,
                    &mut taken,
                    rel,
                    &rel.schema,
                    &fk.table,
                    &fk.columns,
                    &fk.ref_columns,
                    &r.0,
                    q,
                );
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn push(
    out: &mut Vec<JoinSuggestion>,
    taken: &mut Vec<String>,
    rel: &Rel,
    schema: &str,
    table: &str,
    new_cols: &[String],
    old_cols: &[String],
    old_alias: &str,
    q: char,
) {
    let alias = alias_for(table, taken);
    let on = cond(&alias, new_cols, old_alias, old_cols, q);
    // The same table through the same key from the same side twice is one hint.
    if out.iter().any(|s| {
        is(&s.table, table)
            && is(&s.schema, schema)
            && s.key == rel.fk.name
            && s.on.ends_with(&format!(
                " = {old_alias}.{}",
                quote(&old_cols[old_cols.len() - 1], q)
            ))
    }) {
        return;
    }
    taken.push(alias.clone());
    out.push(JoinSuggestion {
        schema: schema.to_string(),
        table: table.to_string(),
        alias,
        on,
        key: rel.fk.name.clone(),
    });
}

/// Conditions for the last table of `refs` (the one just joined) against each
/// earlier table it is related to, through a key in either direction.
pub fn suggest_on(rels: &[Rel], refs: &[Ref], current: Option<&str>, q: char) -> Vec<String> {
    let Some((new, others)) = refs.split_last() else {
        return Vec::new();
    };
    let schema_of = |r: &Ref| {
        r.1.clone()
            .or_else(|| current.map(str::to_string))
            .unwrap_or_default()
    };
    let ns = schema_of(new);
    let mut out = Vec::new();
    for o in others.iter().rev() {
        let os = schema_of(o);
        for rel in rels {
            let fk = &rel.fk;
            let c = if sch(&rel.schema, &ns)
                && is(&fk.table, &new.2)
                && sch(&fk.ref_schema, &os)
                && is(&fk.ref_table, &o.2)
            {
                Some(cond(&new.0, &fk.columns, &o.0, &fk.ref_columns, q))
            } else if sch(&rel.schema, &os)
                && is(&fk.table, &o.2)
                && sch(&fk.ref_schema, &ns)
                && is(&fk.ref_table, &new.2)
            {
                Some(cond(&new.0, &fk.ref_columns, &o.0, &fk.columns, q))
            } else {
                None
            };
            if let Some(c) = c
                && !out.contains(&c)
            {
                out.push(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fk(name: &str, table: &str, cols: &[&str], rt: &str, rcols: &[&str]) -> Rel {
        Rel {
            schema: "app".into(),
            fk: ForeignKey {
                name: name.into(),
                table: table.into(),
                columns: cols.iter().map(|s| s.to_string()).collect(),
                ref_schema: "app".into(),
                ref_table: rt.into(),
                ref_columns: rcols.iter().map(|s| s.to_string()).collect(),
            },
        }
    }

    fn r(alias: &str, table: &str) -> Ref {
        (alias.into(), None, table.into())
    }

    fn rels() -> Vec<Rel> {
        vec![
            fk("fk_o_u", "orders", &["user_id"], "users", &["id"]),
            fk("fk_oi_o", "order_items", &["order_id"], "orders", &["id"]),
            fk(
                "fk_oi_p",
                "order_items",
                &["order_id", "line"],
                "lines",
                &["order_id", "no"],
            ),
            fk("fk_u_u", "users", &["manager_id"], "users", &["id"]),
        ]
    }

    #[test]
    fn contexts() {
        assert_eq!(context("select * from a join ", ""), Some(Ctx::AfterJoin));
        assert_eq!(
            context("select * from a left JOIN or", "or"),
            Some(Ctx::AfterJoin)
        );
        assert_eq!(
            context("select * from a join b on ", ""),
            Some(Ctx::AfterOn)
        );
        assert_eq!(
            context("select * from a join b ON u", "u"),
            Some(Ctx::AfterOn)
        );
        assert_eq!(context("select * from a where ", ""), None);
        assert_eq!(context("select * from a join", ""), None);
    }

    #[test]
    fn outgoing_and_incoming() {
        let s = suggest_tables(&rels(), &[r("u", "users")], None, '`');
        let tables: Vec<_> = s
            .iter()
            .map(|x| (x.table.as_str(), x.on.as_str()))
            .collect();
        assert!(tables.contains(&("orders", "o.user_id = u.id")));
        // The self reference is offered both ways with a fresh alias.
        assert!(
            s.iter()
                .any(|x| x.table == "users" && x.alias == "u2" && x.on == "u2.id = u.manager_id")
        );
        assert!(
            s.iter()
                .any(|x| x.table == "users" && x.on == "u3.manager_id = u.id")
        );
    }

    #[test]
    fn composite_key() {
        let s = suggest_tables(&rels(), &[r("oi", "order_items")], None, '`');
        let l = s.iter().find(|x| x.table == "lines").unwrap();
        assert_eq!(l.on, "l.order_id = oi.order_id AND l.no = oi.line");
        assert_eq!(
            l.insert_with_on('`'),
            "lines l ON l.order_id = oi.order_id AND l.no = oi.line"
        );
    }

    #[test]
    fn schema_must_match() {
        let refs: Vec<Ref> = vec![("u".into(), Some("other".into()), "users".into())];
        assert!(suggest_tables(&rels(), &refs, Some("app"), '`').is_empty());
        let refs: Vec<Ref> = vec![("u".into(), None, "USERS".into())];
        assert!(!suggest_tables(&rels(), &refs, Some("APP"), '`').is_empty());
    }

    #[test]
    fn on_condition_for_last_table() {
        let refs = vec![r("u", "users"), r("o", "orders")];
        assert_eq!(
            suggest_on(&rels(), &refs, None, '`'),
            vec!["o.user_id = u.id"]
        );
        let refs = vec![r("o", "orders"), r("oi", "order_items")];
        assert_eq!(
            suggest_on(&rels(), &refs, None, '`'),
            vec!["oi.order_id = o.id"]
        );
        assert!(suggest_on(&rels(), &[r("u", "users")], None, '`').is_empty());
        assert!(suggest_on(&rels(), &[r("u", "users"), r("l", "lines")], None, '`').is_empty());
    }

    #[test]
    fn aliases() {
        assert_eq!(alias_for("order_items", &[]), "oi");
        assert_eq!(alias_for("orders", &["o".into()]), "o2");
        assert_eq!(alias_for("a", &[]), "a");
        assert_eq!(alias_for("order_new", &[]), "on1");
        assert_eq!(alias_for("訂單", &[]), "訂");
    }

    #[test]
    fn odd_names_are_quoted() {
        let rels = vec![fk("k", "my orders", &["user id"], "users", &["id"])];
        let s = suggest_tables(&rels, &[r("u", "users")], None, '`');
        assert_eq!(s[0].on, "mo.`user id` = u.id");
        assert_eq!(
            s[0].insert_with_on('`'),
            "`my orders` mo ON mo.`user id` = u.id"
        );
    }
}
