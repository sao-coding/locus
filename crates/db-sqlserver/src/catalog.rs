//! Schema introspection from the catalog views of the current database
//! (`sys.schemas`, `sys.objects`, `INFORMATION_SCHEMA.COLUMNS`, `sys.foreign_keys`).
//! Schemas are the schemas of the database the connection is in.
//!
//! Every call runs on a pooled connection, so the schema browser works while a
//! query streams on another one. Names are bound parameters. The extended API
//! (object kinds, DDL, routines, structure, principals) is not there yet: the
//! trait defaults report it as unsupported.

use crate::{MssqlDriver, Source, Stmt, spawn_query};
use db_core::{
    Catalog, CatalogFuture, Cell, ColumnInfo, DbError, ErrorKind, ForeignKey, ObjectKind, Outcome,
    Param, QueryEvent, QueryOptions, SchemaObject,
};

type Rows = Vec<Vec<Option<String>>>;

fn collect(d: &MssqlDriver, sql: &str, params: Vec<Param>) -> CatalogFuture<Rows> {
    let stmt = if params.is_empty() {
        Stmt::Text(sql.to_string())
    } else {
        Stmt::Prepared(sql.to_string(), params)
    };
    let mut h = spawn_query(&d.inner, Source::Pool, stmt, QueryOptions::default());
    Box::pin(async move {
        let mut cols = 0;
        let mut rows: Rows = Vec::new();
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

fn text(r: &[Option<String>], i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}

/// Type text as SQL Server spells it, e.g. `nvarchar(50)`, `varchar(max)`,
/// `decimal(10,2)`, `datetime2(7)`.
pub(crate) fn type_text(
    ty: &str,
    max_len: Option<&str>,
    precision: Option<&str>,
    scale: Option<&str>,
    dt_precision: Option<&str>,
) -> String {
    match ty {
        "char" | "varchar" | "nchar" | "nvarchar" | "binary" | "varbinary" => match max_len {
            Some("-1") => format!("{ty}(max)"),
            Some(l) => format!("{ty}({l})"),
            None => ty.to_string(),
        },
        "decimal" | "numeric" => match (precision, scale) {
            (Some(p), Some(s)) => format!("{ty}({p},{s})"),
            (Some(p), None) => format!("{ty}({p})"),
            _ => ty.to_string(),
        },
        "datetime2" | "datetimeoffset" | "time" => match dt_precision {
            Some(p) if p != "7" => format!("{ty}({p})"),
            _ => ty.to_string(),
        },
        _ => ty.to_string(),
    }
}

/// Folds rows `(constraint, table, column, ref_schema, ref_table, ref_column)`
/// sorted by table, constraint, position into foreign keys.
fn fold_foreign_keys(rows: Rows) -> Vec<ForeignKey> {
    let mut keys: Vec<ForeignKey> = Vec::new();
    for r in rows {
        let [name, table, column, ref_schema, ref_table, ref_column] =
            [0, 1, 2, 3, 4, 5].map(|i| text(&r, i));
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
    keys
}

impl Catalog for MssqlDriver {
    fn foreign_keys(&self, schema: &str) -> CatalogFuture<Vec<ForeignKey>> {
        let rows = collect(
            self,
            "SELECT fk.name, t.name, c.name, SCHEMA_NAME(rt.schema_id), rt.name, rc.name \
             FROM sys.foreign_keys fk \
             JOIN sys.tables t ON t.object_id = fk.parent_object_id \
             JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
             JOIN sys.columns c ON c.object_id = fkc.parent_object_id AND c.column_id = fkc.parent_column_id \
             JOIN sys.tables rt ON rt.object_id = fk.referenced_object_id \
             JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
             WHERE SCHEMA_NAME(t.schema_id) = ? \
             ORDER BY t.name, fk.name, fkc.constraint_column_id",
            vec![Param::Text(schema.to_string())],
        );
        Box::pin(async move { Ok(fold_foreign_keys(rows.await?)) })
    }

    fn schemas(&self) -> CatalogFuture<Vec<String>> {
        // Fixed database roles show up as schemas too; leave them out.
        let rows = collect(
            self,
            "SELECT s.name FROM sys.schemas s \
             WHERE s.principal_id < 16384 AND s.name NOT IN ('guest', 'INFORMATION_SCHEMA', 'sys') \
             ORDER BY s.name",
            vec![],
        );
        Box::pin(async move { Ok(rows.await?.iter().map(|r| text(r, 0)).collect()) })
    }

    fn objects(&self, schema: &str) -> CatalogFuture<Vec<SchemaObject>> {
        let rows = collect(
            self,
            "SELECT o.name, o.type FROM sys.objects o \
             WHERE o.schema_id = SCHEMA_ID(?) AND o.type IN ('U', 'V') AND o.is_ms_shipped = 0 \
             ORDER BY o.name",
            vec![Param::Text(schema.to_string())],
        );
        Box::pin(async move {
            Ok(rows
                .await?
                .iter()
                .map(|r| SchemaObject {
                    name: text(r, 0),
                    kind: if text(r, 1).trim() == "V" {
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
            self,
            "SELECT c.COLUMN_NAME, c.DATA_TYPE, CAST(c.CHARACTER_MAXIMUM_LENGTH AS varchar(12)), \
                    CAST(c.NUMERIC_PRECISION AS varchar(12)), CAST(c.NUMERIC_SCALE AS varchar(12)), \
                    CAST(c.DATETIME_PRECISION AS varchar(12)), c.IS_NULLABLE, \
                    CASE WHEN EXISTS ( \
                        SELECT 1 FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc \
                        JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE k \
                          ON k.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA AND k.CONSTRAINT_NAME = tc.CONSTRAINT_NAME \
                        WHERE tc.CONSTRAINT_TYPE = 'PRIMARY KEY' AND tc.TABLE_SCHEMA = c.TABLE_SCHEMA \
                          AND tc.TABLE_NAME = c.TABLE_NAME AND k.COLUMN_NAME = c.COLUMN_NAME \
                    ) THEN 'Y' ELSE 'N' END \
             FROM INFORMATION_SCHEMA.COLUMNS c \
             WHERE c.TABLE_SCHEMA = ? AND c.TABLE_NAME = ? \
             ORDER BY c.ORDINAL_POSITION",
            vec![
                Param::Text(schema.to_string()),
                Param::Text(object.to_string()),
            ],
        );
        Box::pin(async move {
            Ok(rows
                .await?
                .iter()
                .map(|r| {
                    let o = |i: usize| r.get(i).and_then(|v| v.as_deref());
                    // NUMERIC_PRECISION is set for every number type; only
                    // decimal / numeric spell it out.
                    ColumnInfo {
                        name: text(r, 0),
                        type_name: type_text(&text(r, 1), o(2), o(3), o(4), o(5)),
                        nullable: text(r, 6).eq_ignore_ascii_case("YES"),
                        primary_key: text(r, 7) == "Y",
                    }
                })
                .collect())
        })
    }

    fn current_schema(&self) -> CatalogFuture<Option<String>> {
        let rows = collect(self, "SELECT SCHEMA_NAME()", vec![]);
        Box::pin(async move {
            Ok(rows
                .await?
                .into_iter()
                .next()
                .and_then(|r| r.into_iter().next())
                .flatten())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_text_spells_lengths() {
        assert_eq!(
            type_text("nvarchar", Some("50"), None, None, None),
            "nvarchar(50)"
        );
        assert_eq!(
            type_text("varchar", Some("-1"), None, None, None),
            "varchar(max)"
        );
        assert_eq!(
            type_text("decimal", None, Some("10"), Some("2"), None),
            "decimal(10,2)"
        );
        assert_eq!(type_text("int", None, Some("10"), Some("0"), None), "int");
        assert_eq!(
            type_text("datetime2", None, None, None, Some("3")),
            "datetime2(3)"
        );
        assert_eq!(
            type_text("datetime2", None, None, None, Some("7")),
            "datetime2"
        );
    }

    #[test]
    fn foreign_keys_fold() {
        let row = |n: &str, t: &str, c: &str| {
            [n, t, c, "dbo", "p", "id"]
                .iter()
                .map(|s| Some(s.to_string()))
                .collect::<Vec<_>>()
        };
        let k = fold_foreign_keys(vec![
            row("f", "a", "x"),
            row("f", "a", "y"),
            row("g", "b", "z"),
        ]);
        assert_eq!(k.len(), 2);
        assert_eq!(k[0].columns, ["x", "y"]);
    }
}
