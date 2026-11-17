//! Foreign-key navigation from a result grid: which keys a cell takes part
//! in, and the `SELECT * ... WHERE ...` that opens the other side. Pure text;
//! the grid supplies the driver's identifier quoting and the cell values.

use crate::copyfmt::{Dialect, Value, sql_literal};
use db_core::ForeignKey;

/// One key column of the target row: its name, the cell value and whether
/// the column holds numbers (decided from the type name, like copy-as).
#[derive(Clone, Debug, PartialEq)]
pub struct KeyPart {
    pub column: String,
    pub value: Value,
    pub numeric: bool,
}

/// A foreign key seen from a result: the key, and for each column of the
/// side that is in the result its index in the result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link<'a> {
    pub fk: &'a ForeignKey,
    pub indexes: Vec<usize>,
}

fn indexes(cols: &[String], names: &[String]) -> Option<Vec<usize>> {
    names
        .iter()
        .map(|n| cols.iter().position(|c| c.eq_ignore_ascii_case(n)))
        .collect()
}

/// Keys declared on `table` whose columns are all in the result and include
/// result column `c`. `cols` are the table column names of the result columns.
pub fn outgoing<'a>(
    fks: &'a [ForeignKey],
    table: &str,
    cols: &[String],
    c: usize,
) -> Vec<Link<'a>> {
    fks.iter()
        .filter(|fk| fk.table.eq_ignore_ascii_case(table))
        .filter_map(|fk| indexes(cols, &fk.columns).map(|indexes| Link { fk, indexes }))
        .filter(|l| l.indexes.contains(&c))
        .collect()
}

/// Keys of other tables that reference `schema.table` through columns that
/// are all in the result and include result column `c`.
pub fn incoming<'a>(
    fks: &'a [ForeignKey],
    schema: &str,
    table: &str,
    cols: &[String],
    c: usize,
) -> Vec<Link<'a>> {
    fks.iter()
        .filter(|fk| {
            fk.ref_table.eq_ignore_ascii_case(table) && fk.ref_schema.eq_ignore_ascii_case(schema)
        })
        .filter_map(|fk| indexes(cols, &fk.ref_columns).map(|indexes| Link { fk, indexes }))
        .filter(|l| l.indexes.contains(&c))
        .collect()
}

/// `SELECT * FROM <schema>.<table> WHERE <col> = <value> AND ...`. A NULL
/// part becomes `IS NULL` (an `= NULL` would match nothing). `quote` is the
/// driver's identifier quoting.
pub fn select_where(
    d: Dialect,
    quote: &dyn Fn(&str) -> String,
    schema: &str,
    table: &str,
    parts: &[KeyPart],
) -> String {
    let target = if schema.is_empty() {
        quote(table)
    } else {
        format!("{}.{}", quote(schema), quote(table))
    };
    let conds: Vec<String> = parts
        .iter()
        .map(|p| match &p.value {
            Value::Null => format!("{} IS NULL", quote(&p.column)),
            v => format!("{} = {}", quote(&p.column), sql_literal(v, p.numeric, d)),
        })
        .collect();
    if conds.is_empty() {
        format!("SELECT * FROM {target}")
    } else {
        format!("SELECT * FROM {target} WHERE {}", conds.join(" AND "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bt(s: &str) -> String {
        format!("`{}`", s.replace('`', "``"))
    }
    fn fk(name: &str, table: &str, cols: &[&str], rt: &str, rcols: &[&str]) -> ForeignKey {
        ForeignKey {
            name: name.into(),
            table: table.into(),
            columns: cols.iter().map(|s| s.to_string()).collect(),
            ref_schema: "erfk".into(),
            ref_table: rt.into(),
            ref_columns: rcols.iter().map(|s| s.to_string()).collect(),
        }
    }
    fn part(c: &str, v: Value, numeric: bool) -> KeyPart {
        KeyPart {
            column: c.into(),
            value: v,
            numeric,
        }
    }
    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn single_key_numeric_and_text() {
        let p = [part("id", Value::Text("7".into()), true)];
        assert_eq!(
            select_where(Dialect::Mysql, &bt, "erfk", "employee", &p),
            "SELECT * FROM `erfk`.`employee` WHERE `id` = 7"
        );
        let p = [part("code", Value::Text("O'Re\\illy".into()), false)];
        assert_eq!(
            select_where(Dialect::Mysql, &bt, "s", "t", &p),
            "SELECT * FROM `s`.`t` WHERE `code` = 'O''Re\\\\illy'"
        );
        let dq = |s: &str| format!("\"{s}\"");
        assert_eq!(
            select_where(Dialect::Postgres, &dq, "s", "t", &p),
            "SELECT * FROM \"s\".\"t\" WHERE \"code\" = 'O''Re\\illy'"
        );
    }

    #[test]
    fn numeric_column_with_non_number_text_stays_quoted() {
        let p = [part("id", Value::Text("1 OR 1=1".into()), true)];
        assert_eq!(
            select_where(Dialect::Mysql, &bt, "s", "t", &p),
            "SELECT * FROM `s`.`t` WHERE `id` = '1 OR 1=1'"
        );
    }

    #[test]
    fn composite_null_binary_and_quote_in_name() {
        let p = [
            part("order_id", Value::Text("3".into()), true),
            part("line`no", Value::Null, true),
            part("b", Value::Hex("0aff".into()), false),
        ];
        assert_eq!(
            select_where(Dialect::Mysql, &bt, "s", "order_item", &p),
            "SELECT * FROM `s`.`order_item` WHERE `order_id` = 3 AND `line``no` IS NULL AND `b` = 0x0aff"
        );
        assert_eq!(
            select_where(Dialect::Mysql, &bt, "", "t", &[]),
            "SELECT * FROM `t`"
        );
    }

    #[test]
    fn links_need_every_column_in_the_result() {
        let fks = vec![
            fk(
                "fk_ship",
                "shipment",
                &["order_id", "line_no"],
                "order_item",
                &["order_id", "line_no"],
            ),
            fk("fk_mgr", "employee", &["manager_id"], "employee", &["id"]),
        ];
        let cols = names(&["id", "Order_ID", "line_no"]);
        let l = outgoing(&fks, "shipment", &cols, 1);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].indexes, vec![1, 2]);
        assert!(outgoing(&fks, "shipment", &cols, 0).is_empty());
        assert!(outgoing(&fks, "shipment", &names(&["order_id"]), 0).is_empty());
        // The self reference is both outgoing (manager_id) and incoming (id).
        let emp = names(&["id", "manager_id"]);
        assert_eq!(outgoing(&fks, "employee", &emp, 1).len(), 1);
        assert_eq!(incoming(&fks, "erfk", "employee", &emp, 0).len(), 1);
        assert!(incoming(&fks, "other", "employee", &emp, 0).is_empty());
        let oi = names(&["order_id", "line_no"]);
        assert_eq!(
            incoming(&fks, "erfk", "order_item", &oi, 1)[0].fk.name,
            "fk_ship"
        );
    }
}
