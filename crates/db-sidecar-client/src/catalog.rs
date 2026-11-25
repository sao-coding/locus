//! Schema introspection for connections that go through the sidecar.
//!
//! The sidecar has no catalog frame: the host runs data-dictionary SQL
//! through the ordinary `run` frame (bound parameters, so schema and table
//! names are never part of the SQL text) and turns the rows into the
//! `db_core` catalog types. One SQL set per dialect, chosen from the driver
//! name the sidecar reported.
//!
//! * MySQL: `information_schema`, the same statements as `db-mysql`.
//! * Oracle: `ALL_USERS`, `ALL_OBJECTS`, `ALL_TAB_COLUMNS`, `ALL_CONSTRAINTS`
//!   and `ALL_CONS_COLUMNS`. Every read except `ALL_USERS` is filtered by
//!   owner (and table where it applies), and every read is wrapped in
//!   `ROWNUM <= cap` (see `SidecarDriver::set_catalog_row_cap`). Nothing
//!   scans the whole dictionary without a filter. `ROWNUM` instead of
//!   `FETCH FIRST` keeps it working on 11g.

use crate::Inner;
use db_core::dictionary as dd;
use db_core::{
    Catalog, CatalogFuture, Cell, ColumnInfo, DbError, DbObjectKind, ErrorKind, ForeignKey,
    InfoItem, ObjectEntry, ObjectKind, Outcome, Param, Principal, QueryEvent, QueryOptions,
    RoutineParam, SchemaObject,
};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Rows a dictionary read may return on Oracle unless the caller lowers it.
pub(crate) const DEFAULT_ROW_CAP: u32 = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dialect {
    Mysql,
    Oracle,
    Mssql,
}

impl Dialect {
    pub(crate) fn of(driver: &str) -> Option<Dialect> {
        match driver {
            "mysql" => Some(Dialect::Mysql),
            "oracle" => Some(Dialect::Oracle),
            "mssql" => Some(Dialect::Mssql),
            _ => None,
        }
    }
}

type Rows = Vec<Vec<Option<String>>>;

/// Runs one dictionary statement with bound parameters and collects the rows
/// as text. Runs on the driver's runtime; the future can be awaited anywhere.
fn fetch(
    inner: &Arc<Inner>,
    sql: String,
    params: Vec<Param>,
) -> impl Future<Output = Result<Rows, DbError>> + Send + 'static {
    let inner = inner.clone();
    let rt = inner.rt.clone();
    let task = rt.spawn(async move {
        let mut h = inner.run_statement(0, None, sql, params, QueryOptions::default());
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
    });
    async move {
        task.await.unwrap_or_else(|e| {
            Err(DbError::new(
                ErrorKind::Internal,
                format!("catalog task failed: {e}"),
            ))
        })
    }
}

fn text(v: &Option<String>) -> String {
    v.clone().unwrap_or_default()
}

fn cap(inner: &Inner) -> Param {
    Param::Text(inner.catalog_row_cap.load(Ordering::Relaxed).to_string())
}

/// `SELECT * FROM (<inner>) WHERE ROWNUM <= TO_NUMBER(?)`: the cap is the
/// last bound parameter.
fn capped(inner_sql: &str) -> String {
    format!("SELECT * FROM ({inner_sql}) WHERE ROWNUM <= TO_NUMBER(?)")
}

/// SQL Server: `SELECT TOP (CAST(? AS bigint)) <rest>`, the cap is the first
/// bound parameter.
fn mssql_capped(rest: &str) -> String {
    format!("SELECT TOP (CAST(? AS bigint)) {rest}")
}

/// Type text as SQL Server spells it, e.g. `nvarchar(50)`, `varchar(max)`,
/// `decimal(10,2)`, `datetime2(3)`.
pub(crate) fn mssql_type(
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

/// Type text as the Oracle dictionary columns spell it, e.g. `NUMBER(10,2)`,
/// `VARCHAR2(50 CHAR)`, `TIMESTAMP(6)`.
pub(crate) fn oracle_type(
    data_type: &str,
    data_length: Option<&str>,
    precision: Option<&str>,
    scale: Option<&str>,
    char_length: Option<&str>,
    char_used: Option<&str>,
) -> String {
    fn nz(s: Option<&str>) -> Option<&str> {
        s.filter(|v| !v.is_empty())
    }
    match data_type {
        "NUMBER" => match (nz(precision), nz(scale)) {
            (Some(p), Some(s)) if s != "0" => format!("NUMBER({p},{s})"),
            (Some(p), _) => format!("NUMBER({p})"),
            (None, Some("0")) => "NUMBER(*,0)".to_string(),
            (None, Some(s)) => format!("NUMBER(*,{s})"),
            (None, None) => "NUMBER".to_string(),
        },
        "FLOAT" => match nz(precision) {
            Some(p) => format!("FLOAT({p})"),
            None => "FLOAT".to_string(),
        },
        "VARCHAR2" | "CHAR" => match (nz(char_used), nz(char_length), nz(data_length)) {
            (Some("C"), Some(l), _) => format!("{data_type}({l} CHAR)"),
            (_, _, Some(l)) => format!("{data_type}({l})"),
            _ => data_type.to_string(),
        },
        "NVARCHAR2" | "NCHAR" => match nz(char_length) {
            Some(l) => format!("{data_type}({l})"),
            None => data_type.to_string(),
        },
        "RAW" => match nz(data_length) {
            Some(l) => format!("RAW({l})"),
            None => "RAW".to_string(),
        },
        other => other.to_string(),
    }
}

/// Folds rows `(constraint, table, column, ref_schema, ref_table, ref_column)`
/// that arrive sorted by table, constraint, position into foreign keys.
fn fold_foreign_keys(rows: Rows) -> Vec<ForeignKey> {
    let mut keys: Vec<ForeignKey> = Vec::new();
    for r in rows {
        let [name, table, column, ref_schema, ref_table, ref_column] =
            [0, 1, 2, 3, 4, 5].map(|i| text(r.get(i).unwrap_or(&None)));
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

impl Catalog for crate::SidecarDriver {
    fn foreign_keys(&self, schema: &str) -> CatalogFuture<Vec<ForeignKey>> {
        let inner = self.inner.clone();
        let (sql, params) = match dialect(&inner) {
            Ok(Dialect::Mysql) => (
                "SELECT CONSTRAINT_NAME, TABLE_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA, \
                        REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
                 FROM information_schema.KEY_COLUMN_USAGE \
                 WHERE TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME IS NOT NULL \
                 ORDER BY TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION"
                    .to_string(),
                vec![Param::Text(schema.to_string())],
            ),
            Ok(Dialect::Oracle) => (
                capped(
                    "SELECT k.CONSTRAINT_NAME AS FK_NAME, k.TABLE_NAME AS FK_TABLE, c.COLUMN_NAME AS FK_COLUMN, \
                            r.OWNER AS REF_OWNER, r.TABLE_NAME AS REF_TABLE, rc.COLUMN_NAME AS REF_COLUMN \
                     FROM ALL_CONSTRAINTS k \
                     JOIN ALL_CONS_COLUMNS c ON c.OWNER = k.OWNER AND c.CONSTRAINT_NAME = k.CONSTRAINT_NAME \
                     JOIN ALL_CONSTRAINTS r ON r.OWNER = k.R_OWNER AND r.CONSTRAINT_NAME = k.R_CONSTRAINT_NAME \
                     JOIN ALL_CONS_COLUMNS rc ON rc.OWNER = r.OWNER AND rc.CONSTRAINT_NAME = r.CONSTRAINT_NAME \
                                              AND rc.POSITION = c.POSITION \
                     WHERE k.OWNER = ? AND k.CONSTRAINT_TYPE = 'R' \
                     ORDER BY k.TABLE_NAME, k.CONSTRAINT_NAME, c.POSITION",
                ),
                vec![Param::Text(schema.to_string()), cap(&inner)],
            ),
            Ok(Dialect::Mssql) => (
                mssql_capped(
                    "fk.name, t.name, c.name, SCHEMA_NAME(rt.schema_id), rt.name, rc.name \
                     FROM sys.foreign_keys fk \
                     JOIN sys.tables t ON t.object_id = fk.parent_object_id \
                     JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
                     JOIN sys.columns c ON c.object_id = fkc.parent_object_id AND c.column_id = fkc.parent_column_id \
                     JOIN sys.tables rt ON rt.object_id = fk.referenced_object_id \
                     JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
                     WHERE t.schema_id = SCHEMA_ID(?) \
                     ORDER BY t.name, fk.name, fkc.constraint_column_id",
                ),
                vec![cap(&inner), Param::Text(schema.to_string())],
            ),
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let rows = fetch(&inner, sql, params);
        Box::pin(async move { Ok(fold_foreign_keys(rows.await?)) })
    }

    fn schemas(&self) -> CatalogFuture<Vec<String>> {
        let inner = self.inner.clone();
        let (sql, params) = match dialect(&inner) {
            Ok(Dialect::Mysql) => (
                "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME"
                    .to_string(),
                vec![],
            ),
            // ALL_USERS is the list of schemas; it has no owner column to filter on.
            Ok(Dialect::Oracle) => (
                capped("SELECT USERNAME FROM ALL_USERS ORDER BY USERNAME"),
                vec![cap(&inner)],
            ),
            // Schemas of the current database; fixed database roles and the
            // system schemas are left out.
            Ok(Dialect::Mssql) => (
                mssql_capped(
                    "s.name FROM sys.schemas s \
                     WHERE s.principal_id < 16384 AND s.name NOT IN ('guest', 'INFORMATION_SCHEMA', 'sys') \
                     ORDER BY s.name",
                ),
                vec![cap(&inner)],
            ),
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let rows = fetch(&inner, sql, params);
        Box::pin(async move { Ok(rows.await?.iter().map(|r| text(&r[0])).collect()) })
    }

    fn objects(&self, schema: &str) -> CatalogFuture<Vec<SchemaObject>> {
        let inner = self.inner.clone();
        let schema = Param::Text(schema.to_string());
        let (sql, params) = match dialect(&inner) {
            Ok(Dialect::Mysql) => (
                "SELECT TABLE_NAME, TABLE_TYPE FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME".to_string(),
                vec![schema],
            ),
            Ok(Dialect::Oracle) => (
                capped(
                    "SELECT OBJECT_NAME, OBJECT_TYPE FROM ALL_OBJECTS \
                     WHERE OWNER = ? AND OBJECT_TYPE IN ('TABLE', 'VIEW') AND OBJECT_NAME NOT LIKE 'BIN$%' \
                     ORDER BY OBJECT_NAME",
                ),
                vec![schema, cap(&inner)],
            ),
            // `type` is char(2): 'U ' / 'V '. ends_with("VIEW") below needs a name.
            Ok(Dialect::Mssql) => (
                mssql_capped(
                    "o.name, CASE o.type WHEN 'V' THEN 'VIEW' ELSE 'TABLE' END FROM sys.objects o \
                     WHERE o.schema_id = SCHEMA_ID(?) AND o.type IN ('U', 'V') AND o.is_ms_shipped = 0 \
                     ORDER BY o.name",
                ),
                vec![cap(&inner), schema],
            ),
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let rows = fetch(&inner, sql, params);
        Box::pin(async move {
            Ok(rows
                .await?
                .iter()
                .map(|r| SchemaObject {
                    name: text(&r[0]),
                    kind: if text(&r[1]).ends_with("VIEW") {
                        ObjectKind::View
                    } else {
                        ObjectKind::Table
                    },
                })
                .collect())
        })
    }

    fn columns(&self, schema: &str, object: &str) -> CatalogFuture<Vec<ColumnInfo>> {
        let inner = self.inner.clone();
        let (schema, object) = (
            Param::Text(schema.to_string()),
            Param::Text(object.to_string()),
        );
        match dialect(&inner) {
            Ok(Dialect::Mysql) => {
                let rows = fetch(
                    &inner,
                    "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY FROM information_schema.COLUMNS \
                     WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION"
                        .to_string(),
                    vec![schema, object],
                );
                Box::pin(async move {
                    Ok(rows
                        .await?
                        .iter()
                        .map(|r| ColumnInfo {
                            name: text(&r[0]),
                            type_name: text(&r[1]),
                            nullable: text(&r[2]).eq_ignore_ascii_case("YES"),
                            primary_key: text(&r[3]) == "PRI",
                        })
                        .collect())
                })
            }
            Ok(Dialect::Oracle) => {
                let cols = fetch(
                    &inner,
                    capped(
                        "SELECT COLUMN_NAME, DATA_TYPE, DATA_LENGTH, DATA_PRECISION, DATA_SCALE, NULLABLE, CHAR_LENGTH, CHAR_USED \
                         FROM ALL_TAB_COLUMNS WHERE OWNER = ? AND TABLE_NAME = ? ORDER BY COLUMN_ID",
                    ),
                    vec![schema.clone(), object.clone(), cap(&inner)],
                );
                let pk = fetch(
                    &inner,
                    capped(
                        "SELECT cc.COLUMN_NAME FROM ALL_CONSTRAINTS k \
                         JOIN ALL_CONS_COLUMNS cc ON cc.OWNER = k.OWNER AND cc.CONSTRAINT_NAME = k.CONSTRAINT_NAME \
                         WHERE k.OWNER = ? AND k.TABLE_NAME = ? AND k.CONSTRAINT_TYPE = 'P'",
                    ),
                    vec![schema, object, cap(&inner)],
                );
                Box::pin(async move {
                    let (cols, pk) = (cols.await?, pk.await?);
                    let pk: Vec<String> = pk.iter().map(|r| text(&r[0])).collect();
                    Ok(cols
                        .iter()
                        .map(|r| {
                            let o = |i: usize| r.get(i).and_then(|v| v.as_deref());
                            let name = text(&r[0]);
                            ColumnInfo {
                                type_name: oracle_type(&text(&r[1]), o(2), o(3), o(4), o(6), o(7)),
                                nullable: o(5) == Some("Y"),
                                primary_key: pk.contains(&name),
                                name,
                            }
                        })
                        .collect())
                })
            }
            Ok(Dialect::Mssql) => {
                let rows = fetch(
                    &inner,
                    mssql_capped(
                        "c.COLUMN_NAME, c.DATA_TYPE, CAST(c.CHARACTER_MAXIMUM_LENGTH AS varchar(12)), \
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
                    ),
                    vec![cap(&inner), schema, object],
                );
                Box::pin(async move {
                    Ok(rows
                        .await?
                        .iter()
                        .map(|r| {
                            let o = |i: usize| r.get(i).and_then(|v| v.as_deref());
                            ColumnInfo {
                                name: text(&r[0]),
                                type_name: mssql_type(&text(&r[1]), o(2), o(3), o(4), o(5)),
                                nullable: o(6).is_some_and(|v| v.eq_ignore_ascii_case("YES")),
                                primary_key: o(7) == Some("Y"),
                            }
                        })
                        .collect())
                })
            }
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }

    fn supported_kinds(&self) -> Vec<DbObjectKind> {
        dd::Dialect::of(&self.inner.driver)
            .map(|d| d.supported_kinds())
            .unwrap_or_default()
    }

    fn list_objects(
        &self,
        schema: &str,
        kind: DbObjectKind,
        cap: u32,
    ) -> CatalogFuture<Vec<ObjectEntry>> {
        match dd_dialect(&self.inner) {
            Ok(d) => dd::list_objects(d, dd_fetch(&self.inner), schema, kind, cap),
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }

    fn list_page(
        &self,
        schema: &str,
        kind: DbObjectKind,
        filter: Option<&str>,
        cap: u32,
    ) -> CatalogFuture<db_core::ObjectPage> {
        match dd_dialect(&self.inner) {
            Ok(d) => dd::list_page(d, dd_fetch(&self.inner), schema, kind, filter, cap),
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }

    fn children(
        &self,
        schema: &str,
        table: &str,
        kind: DbObjectKind,
        cap: u32,
    ) -> CatalogFuture<Vec<ObjectEntry>> {
        match dd_dialect(&self.inner) {
            Ok(d) => dd::children(d, dd_fetch(&self.inner), schema, table, kind, cap),
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }

    fn routine_params(
        &self,
        schema: &str,
        kind: DbObjectKind,
        name: &str,
        cap: u32,
    ) -> CatalogFuture<Vec<RoutineParam>> {
        match dd_dialect(&self.inner) {
            Ok(d) => dd::routine_params(d, dd_fetch(&self.inner), schema, kind, name, cap),
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }

    fn ddl(&self, schema: &str, kind: DbObjectKind, name: &str) -> CatalogFuture<String> {
        match dd_dialect(&self.inner) {
            Ok(d) => dd::ddl(d, dd_fetch(&self.inner), schema, kind, name),
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }

    fn structure(
        &self,
        schema: &str,
        table: &str,
        cap: u32,
    ) -> CatalogFuture<db_core::structure::TableStructure> {
        match dd_dialect(&self.inner) {
            Ok(d) => db_core::struct_sql::structure(d, dd_fetch(&self.inner), schema, table, cap),
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }

    fn principals(&self, cap: u32) -> CatalogFuture<Vec<Principal>> {
        match dd_dialect(&self.inner) {
            Ok(d) => dd::principals(d, dd_fetch(&self.inner), cap),
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }

    fn server_info(&self, cap: u32) -> CatalogFuture<Vec<InfoItem>> {
        match dd_dialect(&self.inner) {
            Ok(d) => dd::server_info(d, dd_fetch(&self.inner), cap),
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }

    fn current_schema(&self) -> CatalogFuture<Option<String>> {
        let inner = self.inner.clone();
        let sql = match dialect(&inner) {
            // NULL when no database is selected.
            Ok(Dialect::Mysql) => "SELECT DATABASE()",
            Ok(Dialect::Oracle) => "SELECT SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA') FROM DUAL",
            Ok(Dialect::Mssql) => "SELECT SCHEMA_NAME()",
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let rows = fetch(&inner, sql.to_string(), vec![]);
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

fn dd_dialect(inner: &Inner) -> Result<dd::Dialect, DbError> {
    dd::Dialect::of(&inner.driver).ok_or_else(|| {
        DbError::new(
            ErrorKind::Internal,
            format!("no catalog for the {} driver", inner.driver),
        )
    })
}

/// The statement runner the shared dictionary SQL (`db_core::dictionary`) needs.
fn dd_fetch(inner: &Arc<Inner>) -> dd::Fetch {
    let inner = inner.clone();
    Arc::new(move |s: dd::Stmt| Box::pin(fetch(&inner, s.sql, s.params)))
}

fn dialect(inner: &Inner) -> Result<Dialect, DbError> {
    Dialect::of(&inner.driver).ok_or_else(|| {
        DbError::new(
            ErrorKind::Internal,
            format!("no catalog for the {} driver", inner.driver),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_type_text() {
        let t = |a, b, c, d, e, f| oracle_type(a, b, c, d, e, f);
        assert_eq!(
            t("NUMBER", Some("22"), Some("10"), Some("2"), None, None),
            "NUMBER(10,2)"
        );
        assert_eq!(
            t("NUMBER", Some("22"), Some("10"), Some("0"), None, None),
            "NUMBER(10)"
        );
        assert_eq!(
            t("NUMBER", Some("22"), None, Some("0"), None, None),
            "NUMBER(*,0)"
        );
        assert_eq!(t("NUMBER", Some("22"), None, None, None, None), "NUMBER");
        assert_eq!(
            t("VARCHAR2", Some("200"), None, None, Some("50"), Some("C")),
            "VARCHAR2(50 CHAR)"
        );
        assert_eq!(
            t("VARCHAR2", Some("50"), None, None, Some("50"), Some("B")),
            "VARCHAR2(50)"
        );
        assert_eq!(
            t("NVARCHAR2", Some("100"), None, None, Some("50"), Some("C")),
            "NVARCHAR2(50)"
        );
        assert_eq!(t("RAW", Some("16"), None, None, None, None), "RAW(16)");
        assert_eq!(
            t("TIMESTAMP(6)", Some("11"), None, None, None, None),
            "TIMESTAMP(6)"
        );
        assert_eq!(t("DATE", Some("7"), None, None, None, None), "DATE");
        assert_eq!(t("CLOB", Some("4000"), None, None, None, None), "CLOB");
    }

    #[test]
    fn mssql_type_text() {
        let t = |a, b, c, d, e| mssql_type(a, b, c, d, e);
        assert_eq!(t("nvarchar", Some("50"), None, None, None), "nvarchar(50)");
        assert_eq!(t("varchar", Some("-1"), None, None, None), "varchar(max)");
        assert_eq!(
            t("decimal", None, Some("10"), Some("2"), None),
            "decimal(10,2)"
        );
        assert_eq!(t("int", None, Some("10"), Some("0"), None), "int");
        assert_eq!(t("datetime2", None, None, None, Some("3")), "datetime2(3)");
        assert_eq!(t("datetime2", None, None, None, Some("7")), "datetime2");
        assert!(mssql_capped("1").starts_with("SELECT TOP (CAST(? AS bigint)) "));
    }

    #[test]
    fn foreign_key_rows_fold_by_constraint() {
        let row = |n: &str, t: &str, c: &str, rc: &str| {
            [n, t, c, "S", "P", rc]
                .iter()
                .map(|s| Some(s.to_string()))
                .collect::<Vec<_>>()
        };
        let keys = fold_foreign_keys(vec![
            row("fk1", "a", "x", "id1"),
            row("fk1", "a", "y", "id2"),
            row("fk2", "a", "z", "id"),
            row("fk1", "b", "x", "id"),
        ]);
        assert_eq!(keys.len(), 3);
        assert_eq!(
            (keys[0].columns.clone(), keys[0].ref_columns.clone()),
            (
                vec!["x".into(), "y".into()],
                vec!["id1".into(), "id2".into()]
            )
        );
        assert_eq!(keys[2].table, "b");
    }

    #[test]
    fn oracle_reads_are_filtered_and_capped() {
        // Every Oracle statement of the catalog except the user list filters on OWNER
        // and all of them carry the row cap.
        {
            let sql = capped("SELECT 1 FROM ALL_OBJECTS WHERE OWNER = ?");
            assert!(sql.contains("ROWNUM <= TO_NUMBER(?)") && sql.contains("OWNER = ?"));
        }
    }
}
