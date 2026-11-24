//! Names of result tabs: the table a SELECT reads, when there is exactly one.

use crate::ColumnMeta;
use crate::sql::{has_multi_source, source_table, starts_with_select};

/// Longest tooltip text, in characters.
pub const TIP_CHARS: usize = 200;

/// The name a result tab shows for `sql`, which produced `cols`: the single table it reads
/// (schema dropped), or `None` for a join, subquery, CTE, UNION, table function, expression
/// only select and every statement that is not a SELECT.
///
/// The driver's column origins come first; the FROM of the statement is the fallback.
pub fn result_name(sql: &str, cols: &[ColumnMeta]) -> Option<String> {
    if !starts_with_select(sql) {
        return None;
    }
    let parsed = source_table(sql);
    // Origins of a join or set operation can agree by chance (only one side selected).
    if parsed.is_none() && has_multi_source(sql) {
        return None;
    }
    origin_table(cols).or_else(|| parsed.map(|t| t.table))
}

/// The table every column comes from, when the driver tells and they all agree.
fn origin_table(cols: &[ColumnMeta]) -> Option<String> {
    let mut found: Option<(&str, &str)> = None;
    for c in cols {
        let o = c.origin.as_ref()?;
        let this = (o.schema.as_str(), o.table.as_str());
        match found {
            None => found = Some(this),
            Some(f) if f == this => {}
            Some(_) => return None,
        }
    }
    found.map(|(_, t)| t.to_string()).filter(|t| !t.is_empty())
}

/// Makes the names of one run unique: later duplicates get ` (2)`, ` (3)`, ... (compared
/// case-insensitively). `None` entries stay `None`.
pub fn dedupe(names: &[Option<String>]) -> Vec<Option<String>> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::with_capacity(names.len());
    for n in names {
        let Some(n) = n else {
            out.push(None);
            continue;
        };
        let mut cand = n.clone();
        let mut k = 1;
        while seen.contains(&cand.to_lowercase()) {
            k += 1;
            cand = format!("{n} ({k})");
        }
        seen.push(cand.to_lowercase());
        out.push(Some(cand));
    }
    out
}

/// The statement for a tooltip: whitespace runs folded, cut to [`TIP_CHARS`] with an ellipsis.
pub fn elide(sql: &str) -> String {
    let mut out = String::new();
    let mut n = 0;
    for w in sql.split_whitespace() {
        if n > 0 {
            out.push(' ');
            n += 1;
        }
        for ch in w.chars() {
            if n == TIP_CHARS {
                out.push('…');
                return out;
            }
            out.push(ch);
            n += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ColumnOrigin;

    fn col(table: Option<(&str, &str)>) -> ColumnMeta {
        ColumnMeta {
            name: "c".into(),
            type_name: "INT".into(),
            nullable: None,
            origin: table.map(|(s, t)| ColumnOrigin {
                schema: s.into(),
                table: t.into(),
                column: "c".into(),
            }),
        }
    }

    fn name(sql: &str) -> Option<String> {
        result_name(sql, &[])
    }

    #[test]
    fn plain_and_qualified_names() {
        assert_eq!(
            name("SELECT * FROM booking_rec"),
            Some("booking_rec".into())
        );
        assert_eq!(
            name(r#"select * from "booking"."booking_rec" where id = 1"#),
            Some("booking_rec".into())
        );
        assert_eq!(name("SELECT * FROM `db`.`t 1` LIMIT 5"), Some("t 1".into()));
        assert_eq!(
            name("SELECT * FROM [dbo].[Order Details]"),
            Some("Order Details".into())
        );
        assert_eq!(name(r#"SELECT * FROM "HR"."EMP" e"#), Some("EMP".into()));
    }

    #[test]
    fn aliases_and_aggregates_keep_the_table() {
        assert_eq!(name("SELECT s.* FROM student AS s"), Some("student".into()));
        assert_eq!(
            name("SELECT s.* FROM student s WHERE 1=1"),
            Some("student".into())
        );
        assert_eq!(name("SELECT count(*) FROM t"), Some("t".into()));
        assert_eq!(
            name("SELECT a, sum(b) FROM t GROUP BY a HAVING sum(b) > 1"),
            Some("t".into())
        );
        assert_eq!(name("SELECT DISTINCT a FROM t"), Some("t".into()));
        assert_eq!(
            name("SELECT (SELECT max(x) FROM u) m FROM t"),
            Some("t".into())
        );
        assert_eq!(
            name("SELECT * FROM t WHERE id IN (SELECT id FROM other)"),
            Some("t".into())
        );
    }

    #[test]
    fn no_single_table() {
        for sql in [
            "SELECT * FROM a JOIN b ON a.id = b.id",
            "SELECT * FROM a, b",
            "SELECT * FROM (SELECT * FROM a) x",
            "WITH c AS (SELECT 1 x) SELECT * FROM c",
            "SELECT * FROM a UNION SELECT * FROM b",
            "SELECT * FROM generate_series(1, 3)",
            "SELECT 1",
            "UPDATE t SET a = 1",
            "SHOW TABLES",
            "EXPLAIN SELECT * FROM t",
            "SELECT * FROM t WHERE a = 'unterminated",
        ] {
            assert_eq!(name(sql), None, "{sql}");
        }
    }

    #[test]
    fn driver_origin_wins_and_agrees() {
        let one = [col(Some(("s", "real"))), col(Some(("s", "real")))];
        assert_eq!(result_name("SELECT * FROM v", &one), Some("real".into()));
        // A hint the parser does not know: origins carry it.
        assert_eq!(
            result_name("SELECT * FROM t USE INDEX (i)", &[col(Some(("s", "t")))]),
            Some("t".into())
        );
        // Two tables in the columns: fall back to the parse (a view over a join).
        let two = [col(Some(("s", "a"))), col(Some(("s", "b")))];
        assert_eq!(result_name("SELECT * FROM v", &two), Some("v".into()));
        // An expression column has no origin: parse.
        let mixed = [col(Some(("s", "t"))), col(None)];
        assert_eq!(result_name("SELECT a, 1 FROM t", &mixed), Some("t".into()));
    }

    #[test]
    fn join_ignores_origins() {
        let one = [col(Some(("s", "a")))];
        assert_eq!(
            result_name("SELECT a.x FROM a JOIN b ON a.id = b.id", &one),
            None
        );
    }

    #[test]
    fn duplicates_get_a_counter() {
        let n = |s: &str| Some(s.to_string());
        let got = dedupe(&[n("t"), None, n("u"), n("T"), n("t"), n("t (2)")]);
        assert_eq!(
            got,
            vec![n("t"), None, n("u"), n("T (2)"), n("t (3)"), n("t (2) (2)")]
        );
    }

    #[test]
    fn tooltip_is_folded_and_cut() {
        assert_eq!(elide("SELECT  *\n  FROM t"), "SELECT * FROM t");
        let long = format!("SELECT {}", "a".repeat(500));
        let e = elide(&long);
        assert_eq!(e.chars().count(), TIP_CHARS + 1);
        assert!(e.ends_with('…'));
        let exact = "x".repeat(TIP_CHARS);
        assert_eq!(elide(&exact), exact);
    }
}
