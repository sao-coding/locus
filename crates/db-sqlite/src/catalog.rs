//! Schema introspection from `sqlite_master` and the `PRAGMA` table functions.
//!
//! Every call opens its own connection on a short-lived thread, so the schema
//! browser keeps working while a query streams. SQLite has no schemas in the
//! server sense: `main` (and any attached database) is the only level.

use crate::{Cfg, SqliteDriver, map_error, open};
use db_core::{
    Catalog, CatalogFuture, ColumnInfo, DbError, DbObjectKind, ErrorKind, ForeignKey, InfoItem,
    ObjectEntry, ObjectKind, SchemaObject,
};
use rusqlite::Connection;
use std::sync::Arc;

/// Runs `work` on its own thread with a fresh connection.
pub(crate) fn on_conn<T, F>(cfg: &Arc<Cfg>, work: F) -> CatalogFuture<T>
where
    T: Send + 'static,
    F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
{
    let cfg = cfg.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let r = open(&cfg).and_then(|c| work(&c).map_err(|e| map_error(&e)));
        let _ = tx.send(r);
    });
    Box::pin(async move {
        rx.await
            .unwrap_or_else(|_| Err(DbError::new(ErrorKind::Internal, "catalog thread failed")))
    })
}

pub(crate) fn q(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Tables / views / indexes / triggers of one schema as `(name, tbl_name, sql)`.
pub(crate) fn master(
    c: &Connection,
    schema: &str,
    kind: &str,
    table: Option<&str>,
) -> rusqlite::Result<Vec<(String, String, Option<String>)>> {
    let mut sql = format!(
        "SELECT name, tbl_name, sql FROM {}.sqlite_master WHERE type = ?1 AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'",
        q(schema)
    );
    if table.is_some() {
        sql.push_str(" AND tbl_name = ?2");
    }
    sql.push_str(" ORDER BY name COLLATE NOCASE");
    let mut st = c.prepare(&sql)?;
    let map = |r: &rusqlite::Row| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    };
    let rows = match table {
        Some(t) => st
            .query_map([kind, t], map)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        None => st
            .query_map([kind], map)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    };
    Ok(rows)
}

pub(crate) fn table_columns(
    c: &Connection,
    schema: &str,
    table: &str,
) -> rusqlite::Result<Vec<ColumnInfo>> {
    let mut st = c.prepare(&format!("PRAGMA {}.table_info({})", q(schema), q(table)))?;
    // cid, name, type, notnull, dflt_value, pk
    let rows = st.query_map([], |r| {
        let pk: i64 = r.get(5)?;
        let notnull: i64 = r.get(3)?;
        Ok(ColumnInfo {
            name: r.get(1)?,
            type_name: r.get(2)?,
            nullable: notnull == 0 && pk == 0,
            primary_key: pk > 0,
        })
    })?;
    rows.collect()
}

fn index_detail(
    c: &Connection,
    schema: &str,
    index: &str,
    unique: Option<bool>,
) -> rusqlite::Result<String> {
    let mut st = c.prepare(&format!("PRAGMA {}.index_info({})", q(schema), q(index)))?;
    let cols = st
        .query_map([], |r| r.get::<_, Option<String>>(2))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let cols: Vec<String> = cols
        .into_iter()
        .map(|c| c.unwrap_or_else(|| "<expr>".into()))
        .collect();
    Ok(format!(
        "{}{}",
        if unique == Some(true) { "UNIQUE " } else { "" },
        cols.join(", ")
    ))
}

fn uniqueness(c: &Connection, schema: &str, table: &str) -> rusqlite::Result<Vec<(String, bool)>> {
    let mut st = c.prepare(&format!("PRAGMA {}.index_list({})", q(schema), q(table)))?;
    // seq, name, unique, origin, partial
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, String>(1)?, r.get::<_, i64>(2)? != 0))
    })?;
    rows.collect()
}

fn foreign_keys_of(c: &Connection, schema: &str, table: &str) -> rusqlite::Result<Vec<ForeignKey>> {
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
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out: Vec<(i64, ForeignKey)> = Vec::new();
    for (id, ref_table, from, to) in rows {
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
                ForeignKey {
                    // SQLite does not name foreign keys.
                    name: format!("{table}_fk{id}"),
                    table: table.to_string(),
                    columns: vec![from],
                    ref_schema: schema.to_string(),
                    ref_table,
                    ref_columns: vec![to],
                },
            )),
        }
    }
    Ok(out.into_iter().map(|(_, f)| f).collect())
}

impl Catalog for SqliteDriver {
    fn foreign_keys(&self, schema: &str) -> CatalogFuture<Vec<ForeignKey>> {
        let schema = schema.to_string();
        on_conn(&self.cfg, move |c| {
            let mut all = Vec::new();
            for (t, _, _) in master(c, &schema, "table", None)? {
                all.extend(foreign_keys_of(c, &schema, &t)?);
            }
            all.sort_by(|a, b| {
                (a.table.to_lowercase(), &a.name).cmp(&(b.table.to_lowercase(), &b.name))
            });
            Ok(all)
        })
    }

    fn schemas(&self) -> CatalogFuture<Vec<String>> {
        on_conn(&self.cfg, |c| {
            let mut st = c.prepare("PRAGMA database_list")?;
            let names = st
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(names.into_iter().filter(|n| n != "temp").collect())
        })
    }

    fn objects(&self, schema: &str) -> CatalogFuture<Vec<SchemaObject>> {
        let schema = schema.to_string();
        on_conn(&self.cfg, move |c| {
            let mut out: Vec<SchemaObject> = Vec::new();
            for (kind, k) in [("table", ObjectKind::Table), ("view", ObjectKind::View)] {
                out.extend(
                    master(c, &schema, kind, None)?
                        .into_iter()
                        .map(|(name, ..)| SchemaObject { name, kind: k }),
                );
            }
            out.sort_by_key(|o| o.name.to_lowercase());
            Ok(out)
        })
    }

    fn columns(&self, schema: &str, object: &str) -> CatalogFuture<Vec<ColumnInfo>> {
        let (schema, object) = (schema.to_string(), object.to_string());
        on_conn(&self.cfg, move |c| table_columns(c, &schema, &object))
    }

    fn current_schema(&self) -> CatalogFuture<Option<String>> {
        Box::pin(async { Ok(Some("main".to_string())) })
    }

    fn supported_kinds(&self) -> Vec<DbObjectKind> {
        vec![
            DbObjectKind::Table,
            DbObjectKind::View,
            DbObjectKind::Index,
            DbObjectKind::Trigger,
        ]
    }

    fn list_objects(
        &self,
        schema: &str,
        kind: DbObjectKind,
        cap: u32,
    ) -> CatalogFuture<Vec<ObjectEntry>> {
        let schema = schema.to_string();
        on_conn(&self.cfg, move |c| {
            let name = match kind {
                DbObjectKind::Table => "table",
                DbObjectKind::View => "view",
                DbObjectKind::Index => "index",
                DbObjectKind::Trigger => "trigger",
                _ => return Ok(Vec::new()),
            };
            let mut out = Vec::new();
            for (n, tbl, _) in master(c, &schema, name, None)?
                .into_iter()
                .take(cap as usize)
            {
                let (parent, detail) = match kind {
                    DbObjectKind::Index => {
                        let u = uniqueness(c, &schema, &tbl)?
                            .into_iter()
                            .find(|(i, _)| *i == n)
                            .map(|(_, u)| u);
                        (Some(tbl), Some(index_detail(c, &schema, &n, u)?))
                    }
                    DbObjectKind::Trigger => (Some(tbl), None),
                    _ => (None, None),
                };
                out.push(ObjectEntry {
                    name: n,
                    parent,
                    detail,
                });
            }
            Ok(out)
        })
    }

    fn children(
        &self,
        schema: &str,
        table: &str,
        kind: DbObjectKind,
        cap: u32,
    ) -> CatalogFuture<Vec<ObjectEntry>> {
        let (schema, table) = (schema.to_string(), table.to_string());
        on_conn(&self.cfg, move |c| {
            let mut out = Vec::new();
            match kind {
                DbObjectKind::Index => {
                    let uniq = uniqueness(c, &schema, &table)?;
                    for (n, ..) in master(c, &schema, "index", Some(&table))? {
                        let u = uniq.iter().find(|(i, _)| *i == n).map(|(_, u)| *u);
                        out.push(ObjectEntry {
                            name: n.clone(),
                            parent: Some(table.clone()),
                            detail: Some(index_detail(c, &schema, &n, u)?),
                        });
                    }
                }
                DbObjectKind::Trigger => {
                    for (n, ..) in master(c, &schema, "trigger", Some(&table))? {
                        out.push(ObjectEntry {
                            name: n,
                            parent: Some(table.clone()),
                            detail: None,
                        });
                    }
                }
                _ => {}
            }
            out.truncate(cap as usize);
            Ok(out)
        })
    }

    fn ddl(&self, schema: &str, kind: DbObjectKind, name: &str) -> CatalogFuture<String> {
        let (schema, name) = (schema.to_string(), name.to_string());
        on_conn(&self.cfg, move |c| {
            let ty = match kind {
                DbObjectKind::Table => "table",
                DbObjectKind::View => "view",
                DbObjectKind::Index => "index",
                DbObjectKind::Trigger => "trigger",
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            let sql: Option<String> = c.query_row(
                &format!(
                    "SELECT sql FROM {}.sqlite_master WHERE type = ?1 AND name = ?2",
                    q(&schema)
                ),
                [ty, name.as_str()],
                |r| r.get(0),
            )?;
            Ok(sql.map(|s| format!("{s};")).unwrap_or_default())
        })
    }

    fn structure(
        &self,
        schema: &str,
        table: &str,
        cap: u32,
    ) -> CatalogFuture<db_core::structure::TableStructure> {
        let (schema, table) = (schema.to_string(), table.to_string());
        on_conn(&self.cfg, move |c| {
            crate::structure::read(c, &schema, &table, cap as usize)
        })
    }

    fn server_info(&self, _cap: u32) -> CatalogFuture<Vec<InfoItem>> {
        let path = self.cfg.path.display().to_string();
        let ro = self.cfg.file_read_only;
        on_conn(&self.cfg, move |c| {
            let item = |group: &str, name: &str, value: String| InfoItem {
                group: group.into(),
                name: name.into(),
                value,
            };
            let mut out = vec![
                item(
                    "server",
                    "version",
                    c.query_row("SELECT sqlite_version()", [], |r| r.get(0))?,
                ),
                item("server", "file", path),
                item("server", "opened read-only", ro.to_string()),
            ];
            for p in [
                "journal_mode",
                "page_size",
                "page_count",
                "encoding",
                "user_version",
                "foreign_keys",
            ] {
                let v: rusqlite::types::Value =
                    c.query_row(&format!("PRAGMA {p}"), [], |r| r.get(0))?;
                let v = match v {
                    rusqlite::types::Value::Integer(i) => i.to_string(),
                    rusqlite::types::Value::Text(t) => t,
                    other => format!("{other:?}"),
                };
                out.push(item("pragma", p, v));
            }
            Ok(out)
        })
    }
}
