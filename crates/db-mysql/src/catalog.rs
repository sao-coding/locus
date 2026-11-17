//! Schema introspection from `information_schema`.
//!
//! Every call takes its own connection from the pool, so the schema browser
//! keeps working while a query holds another connection and streams rows.
//! Names are bound parameters, never part of the SQL text.

use crate::errors::{Phase, map_error};
use crate::{Inner, MysqlDriver};
use db_core::dictionary as dd;
use db_core::{
    Catalog, CatalogFuture, ColumnInfo, DbError, DbObjectKind, ErrorKind, ForeignKey, InfoItem,
    ObjectEntry, ObjectKind, Principal, RoutineParam, SchemaObject,
};
use mysql_async::prelude::Queryable;
use std::future::Future;
use std::sync::Arc;

/// Runs `work` on the driver's runtime with a pooled connection and returns a
/// future that can be awaited from any executor.
fn on_pool<T, F, Fut>(inner: &Arc<Inner>, work: F) -> CatalogFuture<T>
where
    T: Send + 'static,
    F: FnOnce(mysql_async::Conn) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, mysql_async::Error>> + Send + 'static,
{
    let inner = inner.clone();
    let task = inner.rt.clone().spawn(async move {
        let conn = inner
            .pool
            .get_conn()
            .await
            .map_err(|e| map_error(&e, Phase::Connect))?;
        work(conn).await.map_err(|e| map_error(&e, Phase::Query))
    });
    Box::pin(async move {
        match task.await {
            Ok(result) => result,
            Err(e) => Err(DbError::new(
                ErrorKind::Internal,
                format!("catalog task failed: {e}"),
            )),
        }
    })
}

/// A column value as text; NULL is None.
fn value_text(v: mysql_async::Value) -> Option<String> {
    use mysql_async::Value;
    match v {
        Value::NULL => None,
        Value::Bytes(b) => Some(String::from_utf8_lossy(&b).into_owned()),
        Value::Int(i) => Some(i.to_string()),
        Value::UInt(u) => Some(u.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Double(f) => Some(f.to_string()),
        other => Some(other.as_sql(true).trim_matches('\'').to_string()),
    }
}

/// Runs the shared dictionary SQL (db_core::dictionary) on the pool.
fn dd_fetch(inner: &Arc<Inner>) -> dd::Fetch {
    let inner = inner.clone();
    Arc::new(move |s: dd::Stmt| {
        on_pool(&inner, move |mut conn| async move {
            let rows: Vec<mysql_async::Row> = if s.params.is_empty() {
                conn.query(s.sql).await?
            } else {
                conn.exec(
                    s.sql,
                    mysql_async::Params::Positional(crate::to_values(s.params)),
                )
                .await?
            };
            Ok(rows
                .into_iter()
                .map(|r| r.unwrap().into_iter().map(value_text).collect())
                .collect())
        })
    })
}

impl Catalog for MysqlDriver {
    fn supported_kinds(&self) -> Vec<DbObjectKind> {
        dd::Dialect::Mysql.supported_kinds()
    }

    fn list_objects(
        &self,
        schema: &str,
        kind: DbObjectKind,
        cap: u32,
    ) -> CatalogFuture<Vec<ObjectEntry>> {
        dd::list_objects(dd::Dialect::Mysql, dd_fetch(&self.inner), schema, kind, cap)
    }

    fn list_page(
        &self,
        schema: &str,
        kind: DbObjectKind,
        filter: Option<&str>,
        cap: u32,
    ) -> CatalogFuture<db_core::ObjectPage> {
        dd::list_page(
            dd::Dialect::Mysql,
            dd_fetch(&self.inner),
            schema,
            kind,
            filter,
            cap,
        )
    }

    fn children(
        &self,
        schema: &str,
        table: &str,
        kind: DbObjectKind,
        cap: u32,
    ) -> CatalogFuture<Vec<ObjectEntry>> {
        dd::children(
            dd::Dialect::Mysql,
            dd_fetch(&self.inner),
            schema,
            table,
            kind,
            cap,
        )
    }

    fn routine_params(
        &self,
        schema: &str,
        kind: DbObjectKind,
        name: &str,
        cap: u32,
    ) -> CatalogFuture<Vec<RoutineParam>> {
        dd::routine_params(
            dd::Dialect::Mysql,
            dd_fetch(&self.inner),
            schema,
            kind,
            name,
            cap,
        )
    }

    fn ddl(&self, schema: &str, kind: DbObjectKind, name: &str) -> CatalogFuture<String> {
        dd::ddl(
            dd::Dialect::Mysql,
            dd_fetch(&self.inner),
            schema,
            kind,
            name,
        )
    }

    fn structure(
        &self,
        schema: &str,
        table: &str,
        cap: u32,
    ) -> CatalogFuture<db_core::structure::TableStructure> {
        db_core::struct_sql::structure(
            dd::Dialect::Mysql,
            dd_fetch(&self.inner),
            schema,
            table,
            cap,
        )
    }

    fn principals(&self, cap: u32) -> CatalogFuture<Vec<Principal>> {
        dd::principals(dd::Dialect::Mysql, dd_fetch(&self.inner), cap)
    }

    fn server_info(&self, cap: u32) -> CatalogFuture<Vec<InfoItem>> {
        dd::server_info(dd::Dialect::Mysql, dd_fetch(&self.inner), cap)
    }

    fn foreign_keys(&self, schema: &str) -> CatalogFuture<Vec<ForeignKey>> {
        let schema = schema.to_string();
        on_pool(&self.inner, move |mut conn| async move {
            let rows: Vec<(String, String, String, String, String, String)> = conn
                .exec(
                    "SELECT CONSTRAINT_NAME, TABLE_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA, \
                            REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
                     FROM information_schema.KEY_COLUMN_USAGE \
                     WHERE TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME IS NOT NULL \
                     ORDER BY TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION",
                    (schema,),
                )
                .await?;
            let mut keys: Vec<ForeignKey> = Vec::new();
            for (name, table, column, ref_schema, ref_table, ref_column) in rows {
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
        on_pool(&self.inner, |mut conn| async move {
            conn.query("SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME")
                .await
        })
    }

    fn objects(&self, schema: &str) -> CatalogFuture<Vec<SchemaObject>> {
        let schema = schema.to_string();
        on_pool(&self.inner, move |mut conn| async move {
            let rows: Vec<(String, String)> = conn
                .exec(
                    "SELECT TABLE_NAME, TABLE_TYPE FROM information_schema.TABLES \
                     WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME",
                    (schema,),
                )
                .await?;
            Ok(rows
                .into_iter()
                .map(|(name, kind)| SchemaObject {
                    name,
                    kind: if kind.ends_with("VIEW") {
                        ObjectKind::View
                    } else {
                        ObjectKind::Table
                    },
                })
                .collect())
        })
    }

    fn columns(&self, schema: &str, object: &str) -> CatalogFuture<Vec<ColumnInfo>> {
        let (schema, object) = (schema.to_string(), object.to_string());
        on_pool(&self.inner, move |mut conn| async move {
            let rows: Vec<(String, String, String, String)> = conn
                .exec(
                    "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY FROM information_schema.COLUMNS \
                     WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
                    (schema, object),
                )
                .await?;
            Ok(rows
                .into_iter()
                .map(|(name, type_name, nullable, key)| ColumnInfo {
                    name,
                    type_name,
                    nullable: nullable.eq_ignore_ascii_case("YES"),
                    primary_key: key == "PRI",
                })
                .collect())
        })
    }

    fn current_schema(&self) -> CatalogFuture<Option<String>> {
        on_pool(&self.inner, |mut conn| async move {
            // One row, one nullable column: NULL when no database is selected.
            let row: Option<Option<String>> = conn.query_first("SELECT DATABASE()").await?;
            Ok(row.flatten())
        })
    }
}
