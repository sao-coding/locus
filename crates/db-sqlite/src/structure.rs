//! `Catalog::structure` for SQLite: the `PRAGMA` table functions plus the
//! CREATE text in `sqlite_master` (checks, trigger timing).

use crate::catalog::{master, q, table_columns};
use db_core::ObjectEntry;
use db_core::structure::{
    StructCheck, StructColumn, StructForeignKey, StructIndex, TableStructure, sqlite_checks,
};
use rusqlite::Connection;

fn columns(c: &Connection, schema: &str, table: &str) -> rusqlite::Result<Vec<StructColumn>> {
    let mut st = c.prepare(&format!("PRAGMA {}.table_info({})", q(schema), q(table)))?;
    // cid, name, type, notnull, dflt_value, pk
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let pk_count = rows.iter().filter(|r| r.4 > 0).count();
    Ok(rows
        .into_iter()
        .map(|(name, type_name, notnull, default, pk)| {
            // A lone INTEGER PRIMARY KEY is the rowid: the engine numbers it.
            let rowid = pk_count == 1 && pk > 0 && type_name.eq_ignore_ascii_case("integer");
            StructColumn {
                name,
                nullable: notnull == 0 && !rowid,
                default: default.filter(|d| !d.is_empty()),
                primary_key: pk > 0,
                auto_increment: rowid,
                comment: None,
                type_name,
            }
        })
        .collect())
}

fn indexes(c: &Connection, schema: &str, table: &str) -> rusqlite::Result<Vec<StructIndex>> {
    let mut st = c.prepare(&format!("PRAGMA {}.index_list({})", q(schema), q(table)))?;
    // seq, name, unique, origin, partial
    let list = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? != 0,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)? != 0,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    for (name, unique, origin, partial) in list {
        let mut info = c.prepare(&format!("PRAGMA {}.index_info({})", q(schema), q(&name)))?;
        let cols = info
            .query_map([], |r| r.get::<_, Option<String>>(2))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        out.push(StructIndex {
            name,
            columns: cols
                .into_iter()
                .map(|c| c.unwrap_or_else(|| "<expr>".into()))
                .collect(),
            unique,
            primary: origin == "pk",
            kind: if partial {
                "BTREE (partial)".into()
            } else {
                "BTREE".into()
            },
        });
    }
    out.sort_by(|a, b| {
        b.primary
            .cmp(&a.primary)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

fn foreign_keys(
    c: &Connection,
    schema: &str,
    table: &str,
) -> rusqlite::Result<Vec<StructForeignKey>> {
    let mut st = c.prepare(&format!(
        "PRAGMA {}.foreign_key_list({})",
        q(schema),
        q(table)
    ))?;
    // id, seq, table, from, to, on_update, on_delete, match
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out: Vec<(i64, StructForeignKey)> = Vec::new();
    for (id, ref_table, from, to, on_update, on_delete) in rows {
        // `to` is NULL when the key names no column: it points at the primary key.
        let to = match to {
            Some(t) => t,
            None => {
                let pk: Vec<String> = table_columns(c, schema, &ref_table)?
                    .into_iter()
                    .filter(|c| c.primary_key)
                    .map(|c| c.name)
                    .collect();
                let at = out
                    .iter()
                    .find(|(i, _)| *i == id)
                    .map_or(0, |(_, f)| f.columns.len());
                pk.get(at).cloned().unwrap_or_default()
            }
        };
        match out.iter_mut().find(|(i, _)| *i == id) {
            Some((_, f)) => {
                f.columns.push(from);
                f.ref_columns.push(to);
            }
            None => out.push((
                id,
                StructForeignKey {
                    // SQLite does not name foreign keys.
                    name: format!("{table}_fk{id}"),
                    schema: schema.to_string(),
                    table: table.to_string(),
                    columns: vec![from],
                    ref_schema: schema.to_string(),
                    ref_table,
                    ref_columns: vec![to],
                    on_delete,
                    on_update,
                },
            )),
        }
    }
    Ok(out.into_iter().map(|(_, f)| f).collect())
}

/// `AFTER INSERT` out of `CREATE TRIGGER name AFTER INSERT ON t ...`.
pub fn trigger_detail(sql: &str) -> Option<String> {
    let words: Vec<&str> = sql.split_whitespace().collect();
    let on = words.iter().position(|w| w.eq_ignore_ascii_case("on"))?;
    let at = words
        .iter()
        .position(|w| w.eq_ignore_ascii_case("trigger"))?;
    // `IF NOT EXISTS` and a schema-qualified name sit between TRIGGER and the timing.
    let mut from = at + 2;
    if words
        .get(at + 1)
        .is_some_and(|w| w.eq_ignore_ascii_case("if"))
    {
        from = at + 5;
    }
    (from < on).then(|| words[from..on].join(" ").to_uppercase())
}

pub fn read(
    c: &Connection,
    schema: &str,
    table: &str,
    cap: usize,
) -> rusqlite::Result<TableStructure> {
    let mut cols = columns(c, schema, table)?;
    cols.truncate(cap);
    let mut idx = indexes(c, schema, table)?;
    idx.truncate(cap);
    let mut fks = foreign_keys(c, schema, table)?;
    fks.truncate(cap);
    let mut incoming = Vec::new();
    for (name, ..) in master(c, schema, "table", None)? {
        for k in foreign_keys(c, schema, &name)? {
            if k.ref_table.eq_ignore_ascii_case(table) {
                incoming.push(k);
            }
        }
    }
    incoming.truncate(cap);
    let sql: Option<String> = master(c, schema, "table", Some(table))?
        .into_iter()
        .find(|(n, ..)| n.eq_ignore_ascii_case(table))
        .and_then(|(_, _, s)| s);
    let checks = sql
        .map(|s| {
            sqlite_checks(&s)
                .into_iter()
                .enumerate()
                .map(|(i, (name, expression))| StructCheck {
                    name: if name.is_empty() {
                        format!("check_{}", i + 1)
                    } else {
                        name
                    },
                    expression,
                })
                .collect()
        })
        .unwrap_or_default();
    let triggers = master(c, schema, "trigger", Some(table))?
        .into_iter()
        .take(cap)
        .map(|(name, tbl, sql)| ObjectEntry {
            name,
            parent: Some(tbl),
            detail: sql.as_deref().and_then(trigger_detail),
        })
        .collect();
    Ok(TableStructure {
        columns: cols,
        indexes: idx,
        foreign_keys: fks,
        referenced_by: incoming,
        checks,
        triggers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_timing_and_event() {
        assert_eq!(
            trigger_detail("CREATE TRIGGER tr AFTER INSERT ON t BEGIN SELECT 1; END").as_deref(),
            Some("AFTER INSERT")
        );
        assert_eq!(
            trigger_detail(
                "CREATE TRIGGER IF NOT EXISTS tr BEFORE UPDATE OF a ON t BEGIN SELECT 1; END"
            )
            .as_deref(),
            Some("BEFORE UPDATE OF A")
        );
        assert_eq!(trigger_detail("nothing"), None);
    }
}
