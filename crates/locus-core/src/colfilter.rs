//! Column filters and orders of a result (the header menu): conditions and
//! `ORDER BY` terms as data, and the SQL they become. The query is re-run on
//! the server as `SELECT * FROM (<query>) wb_view WHERE .. ORDER BY ..`
//! (see `db_core::sql::view_wrapper`). Pure text; no I/O (`colfilter_store`
//! keeps the saved ones).

use crate::copyfmt::{Dialect, Value, sql_literal};
use serde::{Deserialize, Serialize};

/// Comparison of a column with one value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Lt,
    Like,
    /// Case-insensitive `LIKE`: `ILIKE` on PostgreSQL, `LOWER(..) LIKE LOWER(..)` elsewhere.
    ILike,
}

impl Op {
    pub const ALL: [Op; 6] = [Op::Eq, Op::Ne, Op::Gt, Op::Lt, Op::Like, Op::ILike];

    /// The operator as written in a label (`=`, `<>`, `LIKE` ...).
    pub fn symbol(self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::Ne => "<>",
            Op::Gt => ">",
            Op::Lt => "<",
            Op::Like => "LIKE",
            Op::ILike => "ILIKE",
        }
    }

    pub fn is_like(self) -> bool {
        matches!(self, Op::Like | Op::ILike)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cond {
    /// `col <op> value`. `value` is the text (a number stays unquoted when
    /// `numeric`); for LIKE it is the whole pattern, `esc` = `%`, `_` and `!`
    /// in it were escaped with `!`.
    Cmp {
        col: String,
        op: Op,
        value: String,
        numeric: bool,
        esc: bool,
    },
    IsNull(String),
    NotNull(String),
    /// A WHERE clause typed by the user, put in parentheses.
    Raw(String),
}

/// One `ORDER BY` term.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    pub col: String,
    pub desc: bool,
}

/// Filters and orders of one result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColView {
    #[serde(default)]
    pub conds: Vec<Cond>,
    #[serde(default)]
    pub orders: Vec<Order>,
}

/// Escapes `%`, `_` and the escape character `!` so `v` matches literally;
/// the second value tells whether anything changed (then `ESCAPE '!'` is needed).
pub fn escape_like(v: &str) -> (String, bool) {
    let mut out = String::with_capacity(v.len());
    let mut changed = false;
    for c in v.chars() {
        if matches!(c, '%' | '_' | '!') {
            out.push('!');
            changed = true;
        }
        out.push(c);
    }
    (out, changed)
}

impl Cond {
    /// `col <op> value` for a plain comparison.
    pub fn cmp(col: &str, op: Op, value: &str, numeric: bool) -> Cond {
        Cond::Cmp {
            col: col.into(),
            op,
            value: value.into(),
            numeric,
            esc: false,
        }
    }

    /// `col LIKE '%value%'` (or ILIKE) matching `value` literally.
    pub fn contains(col: &str, ci: bool, value: &str, numeric: bool) -> Cond {
        let (v, esc) = escape_like(value);
        Cond::Cmp {
            col: col.into(),
            op: if ci { Op::ILike } else { Op::Like },
            value: format!("%{v}%"),
            numeric,
            esc,
        }
    }

    /// The column the condition is about (`None` for a raw clause).
    pub fn column(&self) -> Option<&str> {
        match self {
            Cond::Cmp { col, .. } | Cond::IsNull(col) | Cond::NotNull(col) => Some(col),
            Cond::Raw(_) => None,
        }
    }

    /// The condition as SQL; `quote` quotes an identifier.
    pub fn sql(&self, d: Dialect, quote: &dyn Fn(&str) -> String) -> String {
        match self {
            Cond::IsNull(c) => format!("{} IS NULL", quote(c)),
            Cond::NotNull(c) => format!("{} IS NOT NULL", quote(c)),
            Cond::Raw(w) => format!("({})", w.trim()),
            Cond::Cmp {
                col,
                op,
                value,
                numeric,
                esc,
            } => {
                let q = quote(col);
                let v = Value::Text(value.clone());
                if !op.is_like() {
                    return format!("{q} {} {}", op.symbol(), sql_literal(&v, *numeric, d));
                }
                // A number compared with a pattern: PostgreSQL wants text.
                let lhs = if *numeric && d == Dialect::Postgres {
                    format!("CAST({q} AS TEXT)")
                } else {
                    q
                };
                let lit = sql_literal(&v, false, d);
                let tail = if *esc { " ESCAPE '!'" } else { "" };
                match (op, d) {
                    (Op::ILike, Dialect::Postgres) => format!("{lhs} ILIKE {lit}{tail}"),
                    (Op::ILike, _) => format!("LOWER({lhs}) LIKE LOWER({lit}){tail}"),
                    _ => format!("{lhs} LIKE {lit}{tail}"),
                }
            }
        }
    }
}

impl ColView {
    pub fn is_empty(&self) -> bool {
        self.conds.is_empty() && self.orders.is_empty()
    }

    /// Adds a condition unless the same one is already there.
    pub fn add_cond(&mut self, c: Cond) {
        if !self.conds.contains(&c) {
            self.conds.push(c);
        }
    }

    /// Orders by `col`: changes the direction when the column is already
    /// ordered, else appends a term.
    pub fn set_order(&mut self, col: &str, desc: bool) {
        match self.orders.iter_mut().find(|o| o.col == col) {
            Some(o) => o.desc = desc,
            None => self.orders.push(Order {
                col: col.into(),
                desc,
            }),
        }
    }

    pub fn filtered(&self, col: &str) -> bool {
        self.conds.iter().any(|c| c.column() == Some(col))
    }

    pub fn order_of(&self, col: &str) -> Option<bool> {
        self.orders.iter().find(|o| o.col == col).map(|o| o.desc)
    }

    /// Conditions AND-ed (empty when there are none).
    pub fn where_sql(&self, d: Dialect, quote: &dyn Fn(&str) -> String) -> String {
        let parts: Vec<String> = self.conds.iter().map(|c| c.sql(d, quote)).collect();
        parts.join(" AND ")
    }

    pub fn order_sql(&self, quote: &dyn Fn(&str) -> String) -> String {
        let parts: Vec<String> = self
            .orders
            .iter()
            .map(|o| format!("{} {}", quote(&o.col), if o.desc { "DESC" } else { "ASC" }))
            .collect();
        parts.join(", ")
    }

    /// The whole query: `body` (from `db_core::sql::select_body`) wrapped.
    pub fn wrap(&self, body: &str, d: Dialect, quote: &dyn Fn(&str) -> String) -> String {
        db_core::sql::view_wrapper(body, &self.where_sql(d, quote), &self.order_sql(quote))
    }
}

/// Identifier quoting of the dialect (doubling the quote character).
pub fn quote_ident(d: Dialect, name: &str) -> String {
    match d {
        Dialect::Mysql => format!("`{}`", name.replace('`', "``")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

/// Elides the middle of a long value for a menu label: `abcdefg ... xyz`.
pub fn elide(v: &str, max: usize) -> String {
    let n = v.chars().count();
    if n <= max {
        return v.to_string();
    }
    let keep = max.saturating_sub(5);
    let head = keep.div_ceil(2);
    let tail = keep / 2;
    let a: String = v.chars().take(head).collect();
    let b: String = v.chars().skip(n - tail).collect();
    format!("{a} ... {b}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(d: Dialect) -> impl Fn(&str) -> String {
        move |s| quote_ident(d, s)
    }
    fn sql(c: &Cond, d: Dialect) -> String {
        c.sql(d, &q(d))
    }

    #[test]
    fn strings_and_numbers() {
        let s = Cond::cmp("name", Op::Eq, "O'Re\\illy", false);
        assert_eq!(sql(&s, Dialect::Postgres), "\"name\" = 'O''Re\\illy'");
        assert_eq!(sql(&s, Dialect::Mysql), "`name` = 'O''Re\\\\illy'");
        let n = Cond::cmp("f", Op::Eq, "10020515", true);
        assert_eq!(sql(&n, Dialect::Mysql), "`f` = 10020515");
        // Not a number although the column is numeric: quoted, never injected.
        let bad = Cond::cmp("f", Op::Gt, "1 OR 1=1", true);
        assert_eq!(sql(&bad, Dialect::Oracle), "\"f\" > '1 OR 1=1'");
        assert_eq!(
            sql(&Cond::cmp("f", Op::Ne, "7", false), Dialect::Sqlite),
            "\"f\" <> '7'"
        );
        assert_eq!(
            sql(&Cond::cmp("f", Op::Lt, "-1.5", true), Dialect::Sqlite),
            "\"f\" < -1.5"
        );
    }

    #[test]
    fn null_and_raw() {
        assert_eq!(
            sql(&Cond::IsNull("a`b".into()), Dialect::Mysql),
            "`a``b` IS NULL"
        );
        assert_eq!(
            sql(&Cond::NotNull("a".into()), Dialect::Oracle),
            "\"a\" IS NOT NULL"
        );
        assert_eq!(
            sql(&Cond::Raw(" a = 1 OR b = 2 ".into()), Dialect::Mysql),
            "(a = 1 OR b = 2)"
        );
    }

    #[test]
    fn like_escaping() {
        let c = Cond::contains("t", false, "50%_off!'", false);
        assert_eq!(
            sql(&c, Dialect::Postgres),
            "\"t\" LIKE '%50!%!_off!!''%' ESCAPE '!'"
        );
        // Nothing to escape: no ESCAPE clause.
        let c = Cond::contains("t", false, "abc", false);
        assert_eq!(sql(&c, Dialect::Mysql), "`t` LIKE '%abc%'");
        // Typed pattern is used as typed.
        let c = Cond::cmp("t", Op::Like, "a_c%", false);
        assert_eq!(sql(&c, Dialect::Oracle), "\"t\" LIKE 'a_c%'");
    }

    #[test]
    fn ilike_per_dialect() {
        let c = Cond::contains("t", true, "Ab", false);
        assert_eq!(sql(&c, Dialect::Postgres), "\"t\" ILIKE '%Ab%'");
        assert_eq!(sql(&c, Dialect::Mysql), "LOWER(`t`) LIKE LOWER('%Ab%')");
        assert_eq!(sql(&c, Dialect::Oracle), "LOWER(\"t\") LIKE LOWER('%Ab%')");
        assert_eq!(sql(&c, Dialect::Sqlite), "LOWER(\"t\") LIKE LOWER('%Ab%')");
        let e = Cond::contains("t", true, "a%", false);
        assert_eq!(
            sql(&e, Dialect::Sqlite),
            "LOWER(\"t\") LIKE LOWER('%a!%%') ESCAPE '!'"
        );
    }

    #[test]
    fn numeric_like_casts_on_postgres_only() {
        let c = Cond::contains("n", false, "10", true);
        assert_eq!(
            sql(&c, Dialect::Postgres),
            "CAST(\"n\" AS TEXT) LIKE '%10%'"
        );
        assert_eq!(sql(&c, Dialect::Mysql), "`n` LIKE '%10%'");
    }

    #[test]
    fn combine_conditions_and_orders() {
        let mut v = ColView::default();
        assert!(v.is_empty());
        v.add_cond(Cond::cmp("a", Op::Eq, "1", true));
        v.add_cond(Cond::cmp("a", Op::Eq, "1", true));
        v.add_cond(Cond::IsNull("b".into()));
        v.add_cond(Cond::Raw("c > 2 OR c < 0".into()));
        v.set_order("a", false);
        v.set_order("b", true);
        v.set_order("a", true);
        assert_eq!(v.conds.len(), 3);
        assert!(v.filtered("b") && !v.filtered("z"));
        assert_eq!(v.order_of("a"), Some(true));
        let d = Dialect::Mysql;
        assert_eq!(
            v.where_sql(d, &q(d)),
            "`a` = 1 AND `b` IS NULL AND (c > 2 OR c < 0)"
        );
        assert_eq!(v.order_sql(&q(d)), "`a` DESC, `b` DESC");
        assert_eq!(
            v.wrap("SELECT * FROM t", d, &q(d)),
            "SELECT * FROM (\nSELECT * FROM t\n) wb_view\nWHERE `a` = 1 AND `b` IS NULL AND (c > 2 OR c < 0)\nORDER BY `a` DESC, `b` DESC"
        );
        assert_eq!(
            ColView::default().wrap("SELECT 1", d, &q(d)),
            "SELECT * FROM (\nSELECT 1\n) wb_view"
        );
    }

    #[test]
    fn elides_the_middle() {
        assert_eq!(elide("short", 20), "short");
        let e = elide("abcdefghijklmnopqrstuvwxyz", 15);
        assert!(e.contains(" ... ") && e.starts_with("abcde") && e.ends_with("vwxyz"));
        assert_eq!(e.chars().count(), 15);
    }
}
