//! Schema introspection from the PostgreSQL system catalogs (`pg_class`,
//! `pg_attribute`, `pg_constraint`, ...). The extended API (kinds, children,
//! DDL, roles, server info) uses the SQL of `db_core::dictionary`.
//!
//! Every call runs on a pooled connection, so the schema browser works while a
//! query streams on another one. Names are bound parameters.

use crate::{Inner, PgDriver, Source, Stmt, spawn_query};
use db_core::dictionary as dd;
use db_core::{
    Catalog, CatalogFuture, Cell, ColumnInfo, DbError, DbObjectKind, ErrorKind, ForeignKey,
    InfoItem, ObjectEntry, ObjectKind, Outcome, Param, Principal, QueryEvent, QueryOptions,
    RoutineParam, SchemaObject,
};
use std::sync::Arc;

/// Runs one statement on the pool and collects its rows as text.
fn collect(inner: &Arc<Inner>, sql: String, params: Vec<Param>) -> CatalogFuture<Vec<dd::Row>> {
    let stmt = if params.is_empty() {
        Stmt::Text(sql)
    } else {
        Stmt::Prepared(sql, params)
    };
    let mut h = spawn_query(inner, Source::Pool, stmt, QueryOptions::default());
    Box::pin(async move {
        let mut cols = 0;
        let mut rows: Vec<dd::Row> = Vec::new();
        while let Some(ev) = h.next().await {
            match ev {
                QueryEvent::Columns(c) => cols = c.len(),
                QueryEvent::Batch(b) => {
                    for r in 0..b.rows() {
                        rows.push(
                            (0..cols)
                                .map(|c| match b.cell(r, c) {
                                    Cell::Null => None,
                                    Cell::Text(s) => Some(s.to_string()),
                                    Cell::Bytes(x) => Some(String::from_utf8_lossy(x).into_owned()),
                                })
                                .collect(),
                        );
                    }
                }
                QueryEvent::Finished(Outcome::Done { .. }) => return Ok(rows),
                QueryEvent::Finished(Outcome::Failed(e)) => return Err(e),
                QueryEvent::Finished(Outcome::Cancelled) => {
                    return Err(DbError::new(
                        ErrorKind::Internal,
                        "the catalog read was cancelled",
                    ));
                }
                _ => {}
            }
        }
        Err(DbError::new(
            ErrorKind::Internal,
            "the catalog read ended without a result",
        ))
    })
}

fn fetch(inner: &Arc<Inner>) -> dd::Fetch {
    let inner = inner.clone();
    Arc::new(move |s: dd::Stmt| collect(&inner, s.sql, s.params))
}

fn text(r: &dd::Row, i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}

const D: dd::Dialect = dd::Dialect::Postgres;

impl Catalog for PgDriver {
    fn foreign_keys(&self, schema: &str) -> CatalogFuture<Vec<ForeignKey>> {
        let rows = collect(
            &self.inner,
            "SELECT k.conname, c.relname, a.attname, rn.nspname, rc.relname, ra.attname \
             FROM pg_constraint k JOIN pg_class c ON c.oid = k.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_class rc ON rc.oid = k.confrelid JOIN pg_namespace rn ON rn.oid = rc.relnamespace \
             CROSS JOIN LATERAL unnest(k.conkey, k.confkey) WITH ORDINALITY AS u(ck, rk, ord) \
             JOIN pg_attribute a ON a.attrelid = k.conrelid AND a.attnum = u.ck \
             JOIN pg_attribute ra ON ra.attrelid = k.confrelid AND ra.attnum = u.rk \
             WHERE k.contype = 'f' AND n.nspname = ? ORDER BY c.relname, k.conname, u.ord"
                .to_string(),
            vec![Param::Text(schema.to_string())],
        );
        Box::pin(async move {
            let mut keys: Vec<ForeignKey> = Vec::new();
            for r in rows.await? {
                let (name, table, column, ref_schema, ref_table, ref_column) = (
                    text(&r, 0),
                    text(&r, 1),
                    text(&r, 2),
                    text(&r, 3),
                    text(&r, 4),
                    text(&r, 5),
                );
                match keys.last_mut() {
                    Some(k) if k.name == name && k.table == table => {
                        k.columns.push(column);
                        k.ref_columns.push(ref_column);
                    }
                    _ => keys.push(ForeignKey {
                        name,
                        table,
                        columns: vec![column],
                        ref_schema,
                        ref_table,
                        ref_columns: vec![ref_column],
                    }),
                }
            }
            Ok(keys)
        })
    }

    fn schemas(&self) -> CatalogFuture<Vec<String>> {
        // pg_toast, pg_toast_temp_N and pg_temp_N are plumbing, not for browsing.
        let rows = collect(
            &self.inner,
            "SELECT nspname FROM pg_namespace WHERE nspname !~ '^pg_(toast|temp)' ORDER BY nspname"
                .to_string(),
            vec![],
        );
        Box::pin(async move { Ok(rows.await?.iter().map(|r| text(r, 0)).collect()) })
    }

    fn objects(&self, schema: &str) -> CatalogFuture<Vec<SchemaObject>> {
        let rows = collect(
            &self.inner,
            "SELECT c.relname, c.relkind::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relkind IN ('r', 'p', 'f', 'v', 'm') ORDER BY c.relname"
                .to_string(),
            vec![Param::Text(schema.to_string())],
        );
        Box::pin(async move {
            Ok(rows
                .await?
                .iter()
                .map(|r| SchemaObject {
                    name: text(r, 0),
                    kind: if matches!(text(r, 1).as_str(), "v" | "m") {
                        ObjectKind::View
                    } else {
                        ObjectKind::Table
                    },
                })
                .collect())
        })
    }

    fn columns(&self, schema: &str, object: &str) -> CatalogFuture<Vec<ColumnInfo>> {
        let rows = collect(
            &self.inner,
            "SELECT a.attname, format_type(a.atttypid, a.atttypmod), CASE WHEN a.attnotnull THEN 'N' ELSE 'Y' END, \
                    CASE WHEN EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid AND i.indisprimary AND a.attnum = ANY (i.indkey)) \
                         THEN 'Y' ELSE 'N' END \
             FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = ? AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum"
                .to_string(),
            vec![Param::Text(schema.to_string()), Param::Text(object.to_string())],
        );
        Box::pin(async move {
            Ok(rows
                .await?
                .iter()
                .map(|r| ColumnInfo {
                    name: text(r, 0),
                    type_name: text(r, 1),
                    nullable: text(r, 2) == "Y",
                    primary_key: text(r, 3) == "Y",
                })
                .collect())
        })
    }

    fn current_schema(&self) -> CatalogFuture<Option<String>> {
        let rows = collect(&self.inner, "SELECT current_schema()".to_string(), vec![]);
        Box::pin(async move {
            Ok(rows
                .await?
                .into_iter()
                .next()
                .and_then(|r| r.into_iter().next())
                .flatten())
        })
    }

    fn supported_kinds(&self) -> Vec<DbObjectKind> {
        D.supported_kinds()
    }

    fn list_objects(
        &self,
        schema: &str,
        kind: DbObjectKind,
        cap: u32,
    ) -> CatalogFuture<Vec<ObjectEntry>> {
        dd::list_objects(D, fetch(&self.inner), schema, kind, cap)
    }

    fn list_page(
        &self,
        schema: &str,
        kind: DbObjectKind,
        filter: Option<&str>,
        cap: u32,
    ) -> CatalogFuture<db_core::ObjectPage> {
        dd::list_page(D, fetch(&self.inner), schema, kind, filter, cap)
    }

    fn children(
        &self,
        schema: &str,
        table: &str,
        kind: DbObjectKind,
        cap: u32,
    ) -> CatalogFuture<Vec<ObjectEntry>> {
        dd::children(D, fetch(&self.inner), schema, table, kind, cap)
    }

    fn routine_params(
        &self,
        schema: &str,
        kind: DbObjectKind,
        name: &str,
        cap: u32,
    ) -> CatalogFuture<Vec<RoutineParam>> {
        dd::routine_params(D, fetch(&self.inner), schema, kind, name, cap)
    }

    fn ddl(&self, schema: &str, kind: DbObjectKind, name: &str) -> CatalogFuture<String> {
        dd::ddl(D, fetch(&self.inner), schema, kind, name)
    }

    fn structure(
        &self,
        schema: &str,
        table: &str,
        cap: u32,
    ) -> CatalogFuture<db_core::structure::TableStructure> {
        db_core::struct_sql::structure(D, fetch(&self.inner), schema, table, cap)
    }

    fn principals(&self, cap: u32) -> CatalogFuture<Vec<Principal>> {
        dd::principals(D, fetch(&self.inner), cap)
    }

    fn server_info(&self, cap: u32) -> CatalogFuture<Vec<InfoItem>> {
        dd::server_info(D, fetch(&self.inner), cap)
    }
}
