//! Data-dictionary SQL and row folding for [`Catalog::structure`]
//! (MySQL / MariaDB, PostgreSQL and Oracle read the dictionary with plain
//! SQL; SQLite has its own driver code). Same rules as `dictionary`: names
//! are bound, everything is a SELECT, Oracle reads are bounded by `ROWNUM`.
//!
//! Row layouts (all text, `None` = NULL):
//! - columns: name, type, nullable `Y`/`N`, default, primary `Y`/`N`,
//!   auto `Y`/`N`, comment, ordinal
//! - indexes: name, column, unique `Y`/`N`, primary `Y`/`N`, type, position
//! - foreign keys: name, owner schema, owner table, column, ref schema,
//!   ref table, ref column, on delete, on update, position
//! - checks: name, expression

use crate::dictionary::{Dialect, Fetch, Row, Stmt};
use crate::structure::{
    StructCheck, StructColumn, StructForeignKey, StructIndex, TableStructure, bare_check,
    is_not_null_check,
};
use crate::{CatalogFuture, DbError, DbObjectKind, Param};

fn t(s: &str) -> Param {
    Param::Text(s.to_string())
}

fn stmt(sql: impl Into<String>, params: Vec<Param>) -> Stmt {
    Stmt {
        sql: sql.into(),
        params,
        optional: false,
    }
}

/// The dictionary reads of one table.
pub struct StructStmts {
    pub columns: Stmt,
    /// Oracle identity columns (names only); the other engines say it in `columns`.
    pub auto: Option<Stmt>,
    pub indexes: Stmt,
    pub fks_out: Stmt,
    pub fks_in: Stmt,
    /// Alternatives for engines that differ by version: the first one that
    /// does not fail wins.
    pub checks: Vec<Stmt>,
}

const PG_RULE: &str = "WHEN 'a' THEN 'NO ACTION' WHEN 'r' THEN 'RESTRICT' WHEN 'c' THEN 'CASCADE' WHEN 'n' THEN 'SET NULL' WHEN 'd' THEN 'SET DEFAULT' END";

pub fn statements(d: Dialect, schema: &str, table: &str, cap: u32) -> StructStmts {
    let st = || vec![t(schema), t(table)];
    match d {
        Dialect::Mysql => {
            let fk = |filter: &str| {
                stmt(
                    format!(
                        "SELECT k.CONSTRAINT_NAME, k.TABLE_SCHEMA, k.TABLE_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_SCHEMA, \
                         k.REFERENCED_TABLE_NAME, k.REFERENCED_COLUMN_NAME, r.DELETE_RULE, r.UPDATE_RULE, k.ORDINAL_POSITION \
                         FROM information_schema.KEY_COLUMN_USAGE k JOIN information_schema.REFERENTIAL_CONSTRAINTS r \
                         ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME \
                         WHERE k.REFERENCED_TABLE_NAME IS NOT NULL AND {filter} \
                         ORDER BY k.TABLE_SCHEMA, k.TABLE_NAME, k.CONSTRAINT_NAME, k.ORDINAL_POSITION LIMIT {cap}"
                    ),
                    st(),
                )
            };
            let check = |extra: &str| {
                stmt(
                    format!(
                        "SELECT cc.CONSTRAINT_NAME, cc.CHECK_CLAUSE FROM information_schema.CHECK_CONSTRAINTS cc \
                         JOIN information_schema.TABLE_CONSTRAINTS tc ON tc.CONSTRAINT_SCHEMA = cc.CONSTRAINT_SCHEMA \
                         AND tc.CONSTRAINT_NAME = cc.CONSTRAINT_NAME AND tc.CONSTRAINT_TYPE = 'CHECK' {extra} \
                         WHERE tc.TABLE_SCHEMA = ? AND tc.TABLE_NAME = ? ORDER BY cc.CONSTRAINT_NAME LIMIT {cap}"
                    ),
                    st(),
                )
            };
            StructStmts {
                columns: stmt(
                    format!(
                        "SELECT COLUMN_NAME, COLUMN_TYPE, CASE IS_NULLABLE WHEN 'YES' THEN 'Y' ELSE 'N' END, COLUMN_DEFAULT, \
                         CASE WHEN COLUMN_KEY = 'PRI' THEN 'Y' ELSE 'N' END, \
                         CASE WHEN EXTRA LIKE '%auto_increment%' THEN 'Y' ELSE 'N' END, COLUMN_COMMENT, ORDINAL_POSITION \
                         FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION LIMIT {cap}"
                    ),
                    st(),
                ),
                auto: None,
                indexes: stmt(
                    format!(
                        "SELECT INDEX_NAME, COALESCE(COLUMN_NAME, '<expr>'), CASE WHEN NON_UNIQUE = 0 THEN 'Y' ELSE 'N' END, \
                         CASE WHEN INDEX_NAME = 'PRIMARY' THEN 'Y' ELSE 'N' END, INDEX_TYPE, SEQ_IN_INDEX \
                         FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
                         ORDER BY INDEX_NAME, SEQ_IN_INDEX LIMIT {cap}"
                    ),
                    st(),
                ),
                fks_out: fk("k.TABLE_SCHEMA = ? AND k.TABLE_NAME = ?"),
                fks_in: fk("k.REFERENCED_TABLE_SCHEMA = ? AND k.REFERENCED_TABLE_NAME = ?"),
                // MariaDB keeps the table in CHECK_CONSTRAINTS (names repeat across tables); MySQL 8 does not.
                checks: vec![check("AND cc.TABLE_NAME = tc.TABLE_NAME"), check("")],
            }
        }
        Dialect::Postgres => {
            let fk = |filter: &str| {
                stmt(
                    format!(
                        "SELECT k.conname, n.nspname, c.relname, a.attname, rn.nspname, rc.relname, ra.attname, \
                         CASE k.confdeltype {PG_RULE}, CASE k.confupdtype {PG_RULE}, u.ord::text \
                         FROM pg_constraint k JOIN pg_class c ON c.oid = k.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                         JOIN pg_class rc ON rc.oid = k.confrelid JOIN pg_namespace rn ON rn.oid = rc.relnamespace \
                         CROSS JOIN LATERAL unnest(k.conkey, k.confkey) WITH ORDINALITY AS u(ck, rk, ord) \
                         JOIN pg_attribute a ON a.attrelid = k.conrelid AND a.attnum = u.ck \
                         JOIN pg_attribute ra ON ra.attrelid = k.confrelid AND ra.attnum = u.rk \
                         WHERE k.contype = 'f' AND {filter} ORDER BY n.nspname, c.relname, k.conname, u.ord LIMIT {cap}"
                    ),
                    st(),
                )
            };
            StructStmts {
                columns: stmt(
                    format!(
                        "SELECT a.attname, format_type(a.atttypid, a.atttypmod), CASE WHEN a.attnotnull THEN 'N' ELSE 'Y' END, \
                         pg_get_expr(d.adbin, d.adrelid), \
                         CASE WHEN EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid AND i.indisprimary AND a.attnum = ANY (i.indkey)) THEN 'Y' ELSE 'N' END, \
                         CASE WHEN a.attidentity::text <> '' OR pg_get_expr(d.adbin, d.adrelid) LIKE 'nextval(%' THEN 'Y' ELSE 'N' END, \
                         col_description(a.attrelid, a.attnum), a.attnum::text \
                         FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                         LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                         WHERE n.nspname = ? AND c.relname = ? AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum LIMIT {cap}"
                    ),
                    st(),
                ),
                auto: None,
                indexes: stmt(
                    format!(
                        "SELECT ci.relname, pg_get_indexdef(i.indexrelid, k.n, true), CASE WHEN i.indisunique THEN 'Y' ELSE 'N' END, \
                         CASE WHEN i.indisprimary THEN 'Y' ELSE 'N' END, am.amname, k.n::text \
                         FROM pg_index i JOIN pg_class ci ON ci.oid = i.indexrelid JOIN pg_class ct ON ct.oid = i.indrelid \
                         JOIN pg_namespace n ON n.oid = ct.relnamespace JOIN pg_am am ON am.oid = ci.relam \
                         CROSS JOIN LATERAL generate_series(1, i.indnkeyatts::int) AS k(n) \
                         WHERE n.nspname = ? AND ct.relname = ? ORDER BY ci.relname, k.n LIMIT {cap}"
                    ),
                    st(),
                ),
                fks_out: fk("n.nspname = ? AND c.relname = ?"),
                fks_in: fk("rn.nspname = ? AND rc.relname = ?"),
                checks: vec![stmt(
                    format!(
                        "SELECT k.conname, pg_get_constraintdef(k.oid, true) FROM pg_constraint k JOIN pg_class c ON c.oid = k.conrelid \
                         JOIN pg_namespace n ON n.oid = c.relnamespace WHERE k.contype = 'c' AND n.nspname = ? AND c.relname = ? \
                         ORDER BY k.conname LIMIT {cap}"
                    ),
                    st(),
                )],
            }
        }
        Dialect::Oracle => {
            let capp = || Param::Text(cap.to_string());
            let fk = |filter: &str| {
                let mut p = st();
                p.push(capp());
                stmt(
                    format!(
                        "SELECT k.CONSTRAINT_NAME, k.OWNER, k.TABLE_NAME, c.COLUMN_NAME, r.OWNER, r.TABLE_NAME, rc.COLUMN_NAME, \
                         k.DELETE_RULE, 'NO ACTION', TO_CHAR(c.POSITION) \
                         FROM ALL_CONSTRAINTS k JOIN ALL_CONS_COLUMNS c ON c.OWNER = k.OWNER AND c.CONSTRAINT_NAME = k.CONSTRAINT_NAME \
                         JOIN ALL_CONSTRAINTS r ON r.OWNER = k.R_OWNER AND r.CONSTRAINT_NAME = k.R_CONSTRAINT_NAME \
                         JOIN ALL_CONS_COLUMNS rc ON rc.OWNER = r.OWNER AND rc.CONSTRAINT_NAME = r.CONSTRAINT_NAME AND rc.POSITION = c.POSITION \
                         WHERE k.CONSTRAINT_TYPE = 'R' AND {filter} AND ROWNUM <= TO_NUMBER(?)"
                    ),
                    p,
                )
            };
            let with_cap = |sql: &str| {
                let mut p = st();
                p.push(capp());
                stmt(sql, p)
            };
            StructStmts {
                // DATA_DEFAULT is a LONG: it cannot go through an inline view, so ROWNUM sits in the WHERE
                // and the rows are sorted by COLUMN_ID here.
                columns: with_cap(
                    "SELECT c.COLUMN_NAME, \
                     CASE WHEN c.DATA_TYPE = 'NUMBER' THEN 'NUMBER' || CASE WHEN c.DATA_PRECISION IS NOT NULL THEN '(' || c.DATA_PRECISION || \
                          CASE WHEN NVL(c.DATA_SCALE, 0) <> 0 THEN ',' || c.DATA_SCALE END || ')' WHEN c.DATA_SCALE = 0 THEN '(*,0)' END \
                          WHEN c.DATA_TYPE IN ('VARCHAR2', 'CHAR') THEN c.DATA_TYPE || '(' || \
                          CASE WHEN c.CHAR_USED = 'C' THEN c.CHAR_LENGTH || ' CHAR' ELSE TO_CHAR(c.DATA_LENGTH) END || ')' \
                          WHEN c.DATA_TYPE IN ('NVARCHAR2', 'NCHAR') THEN c.DATA_TYPE || '(' || c.CHAR_LENGTH || ')' \
                          WHEN c.DATA_TYPE = 'RAW' THEN 'RAW(' || c.DATA_LENGTH || ')' \
                          WHEN c.DATA_TYPE = 'FLOAT' AND c.DATA_PRECISION IS NOT NULL THEN 'FLOAT(' || c.DATA_PRECISION || ')' \
                          ELSE c.DATA_TYPE END, \
                     c.NULLABLE, c.DATA_DEFAULT, \
                     CASE WHEN EXISTS (SELECT 1 FROM ALL_CONSTRAINTS pk JOIN ALL_CONS_COLUMNS pc ON pc.OWNER = pk.OWNER AND pc.CONSTRAINT_NAME = pk.CONSTRAINT_NAME \
                          WHERE pk.OWNER = c.OWNER AND pk.TABLE_NAME = c.TABLE_NAME AND pk.CONSTRAINT_TYPE = 'P' AND pc.COLUMN_NAME = c.COLUMN_NAME) THEN 'Y' ELSE 'N' END, \
                     'N', \
                     (SELECT m.COMMENTS FROM ALL_COL_COMMENTS m WHERE m.OWNER = c.OWNER AND m.TABLE_NAME = c.TABLE_NAME AND m.COLUMN_NAME = c.COLUMN_NAME), \
                     TO_CHAR(c.COLUMN_ID) \
                     FROM ALL_TAB_COLUMNS c WHERE c.OWNER = ? AND c.TABLE_NAME = ? AND ROWNUM <= TO_NUMBER(?)",
                ),
                auto: Some(Stmt {
                    // 12c and later only; on 11g the view is missing and this reads as "none".
                    optional: true,
                    ..with_cap(
                        "SELECT COLUMN_NAME FROM ALL_TAB_IDENTITY_COLS WHERE OWNER = ? AND TABLE_NAME = ? AND ROWNUM <= TO_NUMBER(?)",
                    )
                }),
                indexes: with_cap(
                    "SELECT ic.INDEX_NAME, ic.COLUMN_NAME, CASE WHEN i.UNIQUENESS = 'UNIQUE' THEN 'Y' ELSE 'N' END, \
                     CASE WHEN EXISTS (SELECT 1 FROM ALL_CONSTRAINTS p WHERE p.OWNER = i.OWNER AND p.INDEX_NAME = i.INDEX_NAME AND p.CONSTRAINT_TYPE = 'P') THEN 'Y' ELSE 'N' END, \
                     i.INDEX_TYPE, TO_CHAR(ic.COLUMN_POSITION) \
                     FROM ALL_IND_COLUMNS ic JOIN ALL_INDEXES i ON i.OWNER = ic.INDEX_OWNER AND i.INDEX_NAME = ic.INDEX_NAME \
                     WHERE ic.TABLE_OWNER = ? AND ic.TABLE_NAME = ? AND ROWNUM <= TO_NUMBER(?)",
                ),
                fks_out: fk("k.OWNER = ? AND k.TABLE_NAME = ?"),
                fks_in: fk("r.OWNER = ? AND r.TABLE_NAME = ?"),
                // SEARCH_CONDITION is a LONG too.
                checks: vec![with_cap(
                    "SELECT CONSTRAINT_NAME, SEARCH_CONDITION FROM ALL_CONSTRAINTS \
                     WHERE OWNER = ? AND TABLE_NAME = ? AND CONSTRAINT_TYPE = 'C' AND ROWNUM <= TO_NUMBER(?)",
                )],
            }
        }
    }
}

fn cell(r: &Row, i: usize) -> Option<String> {
    r.get(i).cloned().flatten()
}

fn text(r: &Row, i: usize) -> String {
    cell(r, i).unwrap_or_default()
}

fn pos(r: &Row, i: usize) -> i64 {
    cell(r, i).and_then(|p| p.trim().parse().ok()).unwrap_or(0)
}

fn yes(r: &Row, i: usize) -> bool {
    cell(r, i).is_some_and(|v| v == "Y")
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

pub fn fold_columns(d: Dialect, mut rows: Vec<Row>, identity: &[String]) -> Vec<StructColumn> {
    rows.sort_by_key(|r| pos(r, 7));
    rows.iter()
        .map(|r| {
            let nullable = yes(r, 2);
            let mut default = non_empty(cell(r, 3));
            // MariaDB writes "NULL" for a nullable column without a default.
            if d == Dialect::Mysql && nullable && default.as_deref() == Some("NULL") {
                default = None;
            }
            let name = text(r, 0);
            StructColumn {
                auto_increment: yes(r, 5) || identity.contains(&name),
                name,
                type_name: text(r, 1),
                nullable,
                default,
                primary_key: yes(r, 4),
                comment: non_empty(cell(r, 6)),
            }
        })
        .collect()
}

/// Rows arrive grouped by index name or not; the columns are ordered by position.
pub fn fold_indexes(mut rows: Vec<Row>) -> Vec<StructIndex> {
    rows.sort_by_key(|r| pos(r, 5));
    let mut out: Vec<StructIndex> = Vec::new();
    for r in &rows {
        let name = text(r, 0);
        let col = text(r, 1);
        match out.iter_mut().find(|i| i.name == name) {
            Some(i) => i.columns.push(col),
            None => out.push(StructIndex {
                name,
                columns: vec![col],
                unique: yes(r, 2),
                primary: yes(r, 3),
                kind: text(r, 4),
            }),
        }
    }
    // Primary key first, then by name.
    out.sort_by(|a, b| {
        b.primary
            .cmp(&a.primary)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

pub fn fold_foreign_keys(mut rows: Vec<Row>) -> Vec<StructForeignKey> {
    rows.sort_by_key(|r| pos(r, 9));
    let mut out: Vec<StructForeignKey> = Vec::new();
    for r in &rows {
        let (name, schema, table) = (text(r, 0), text(r, 1), text(r, 2));
        match out
            .iter_mut()
            .find(|k| k.name == name && k.schema == schema && k.table == table)
        {
            Some(k) => {
                k.columns.push(text(r, 3));
                k.ref_columns.push(text(r, 6));
            }
            None => out.push(StructForeignKey {
                name,
                schema,
                table,
                columns: vec![text(r, 3)],
                ref_schema: text(r, 4),
                ref_table: text(r, 5),
                ref_columns: vec![text(r, 6)],
                on_delete: non_empty(cell(r, 7)).unwrap_or_else(|| "NO ACTION".into()),
                on_update: non_empty(cell(r, 8)).unwrap_or_else(|| "NO ACTION".into()),
            }),
        }
    }
    out.sort_by(|a, b| {
        (a.schema.as_str(), a.table.to_lowercase(), a.name.as_str()).cmp(&(
            b.schema.as_str(),
            b.table.to_lowercase(),
            b.name.as_str(),
        ))
    });
    out
}

pub fn fold_checks(rows: Vec<Row>) -> Vec<StructCheck> {
    let mut out: Vec<StructCheck> = rows
        .iter()
        .filter(|r| !is_not_null_check(&text(r, 1)))
        .map(|r| StructCheck {
            name: text(r, 0),
            expression: bare_check(&text(r, 1)),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Runs the reads and folds them. Columns are required; the other parts turn
/// into empty lists when the account may not read them.
pub fn structure(
    d: Dialect,
    fetch: Fetch,
    schema: &str,
    table: &str,
    cap: u32,
) -> CatalogFuture<TableStructure> {
    let s = statements(d, schema, table, cap);
    let trig = d.children(schema, table, DbObjectKind::Trigger, cap);
    Box::pin(async move {
        let columns = fetch(s.columns).await?;
        let identity: Vec<String> = match s.auto {
            Some(a) => fetch(a)
                .await
                .unwrap_or_default()
                .iter()
                .map(|r| text(r, 0))
                .collect(),
            None => Vec::new(),
        };
        let soft = |r: Result<Vec<Row>, DbError>| r.unwrap_or_default();
        let indexes = soft(fetch(s.indexes).await);
        let fks_out = soft(fetch(s.fks_out).await);
        let fks_in = soft(fetch(s.fks_in).await);
        let mut checks = Vec::new();
        for c in s.checks {
            if let Ok(rows) = fetch(c).await {
                checks = rows;
                break;
            }
        }
        let triggers = match trig {
            Some(tr) => soft(fetch(tr).await)
                .iter()
                .map(|r| crate::ObjectEntry {
                    name: text(r, 0),
                    parent: cell(r, 1),
                    detail: cell(r, 2),
                })
                .collect(),
            None => Vec::new(),
        };
        let foreign_keys = fold_foreign_keys(fks_out);
        // A self reference sits in both lists; "referenced by" shows the other tables' keys and its own.
        Ok(TableStructure {
            columns: fold_columns(d, columns, &identity),
            indexes: fold_indexes(indexes),
            foreign_keys,
            referenced_by: fold_foreign_keys(fks_in),
            checks: fold_checks(checks),
            triggers,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn row(v: &[&str]) -> Row {
        v.iter()
            .map(|s| if *s == "~" { None } else { Some(s.to_string()) })
            .collect()
    }

    #[test]
    fn every_statement_binds_names_and_reads_only() {
        for d in [Dialect::Mysql, Dialect::Postgres, Dialect::Oracle] {
            let s = statements(d, "S'; DROP", "T", 50);
            let mut all = vec![&s.columns, &s.indexes, &s.fks_out, &s.fks_in];
            all.extend(s.auto.iter());
            all.extend(s.checks.iter());
            for st in all {
                assert!(!st.sql.contains("DROP"), "{}", st.sql);
                assert!(
                    st.sql.trim_start().to_uppercase().starts_with("SELECT"),
                    "{}",
                    st.sql
                );
                assert_eq!(st.params[0], Param::Text("S'; DROP".into()), "{}", st.sql);
                let marks = st.sql.matches('?').count();
                assert_eq!(marks, st.params.len(), "{d:?}: {}", st.sql);
                if d == Dialect::Oracle {
                    assert!(st.sql.contains("ROWNUM <= TO_NUMBER(?)"), "{}", st.sql);
                    assert_eq!(st.params.last(), Some(&Param::Text("50".into())));
                } else {
                    assert!(st.sql.contains("LIMIT 50"), "{}", st.sql);
                }
            }
        }
    }

    #[test]
    fn columns_fold_in_ordinal_order_with_defaults() {
        let rows = vec![
            row(&["note", "text", "Y", "NULL", "N", "N", "", "2"]),
            row(&["id", "int(11)", "N", "~", "Y", "Y", "the key", "1"]),
            row(&["qty", "int", "N", "0", "N", "N", "~", "3"]),
        ];
        let c = fold_columns(Dialect::Mysql, rows, &[]);
        assert_eq!(
            c.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["id", "note", "qty"]
        );
        assert!(c[0].primary_key && c[0].auto_increment && !c[0].nullable);
        assert_eq!(c[0].comment.as_deref(), Some("the key"));
        assert_eq!(c[1].default, None); // MariaDB "NULL"
        assert_eq!(c[1].comment, None);
        assert_eq!(c[2].default.as_deref(), Some("0"));
        // Oracle identity names come from the second read.
        let o = fold_columns(
            Dialect::Oracle,
            vec![row(&["ID", "NUMBER", "N", "~", "Y", "N", "~", "1"])],
            &["ID".into()],
        );
        assert!(o[0].auto_increment);
    }

    #[test]
    fn indexes_group_by_name_and_sort_by_position() {
        let rows = vec![
            row(&["idx_b", "y", "N", "N", "BTREE", "2"]),
            row(&["PRIMARY", "id", "Y", "Y", "BTREE", "1"]),
            row(&["idx_b", "x", "N", "N", "BTREE", "1"]),
            row(&["a_uq", "c", "Y", "N", "BTREE", "1"]),
        ];
        let i = fold_indexes(rows);
        assert_eq!(
            i.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            ["PRIMARY", "a_uq", "idx_b"]
        );
        assert_eq!(i[2].columns, ["x", "y"]);
        assert!(i[1].unique && !i[1].primary);
    }

    #[test]
    fn foreign_keys_fold_by_constraint() {
        let r = |n: &str, tb: &str, c: &str, rc: &str, p: &str| {
            row(&[n, "s", tb, c, "s", "parent", rc, "CASCADE", "NO ACTION", p])
        };
        let k = fold_foreign_keys(vec![
            r("fk1", "a", "y", "id2", "2"),
            r("fk1", "a", "x", "id1", "1"),
            r("fk0", "b", "z", "id", "1"),
        ]);
        assert_eq!(k.len(), 2);
        assert_eq!((k[0].table.as_str(), k[0].name.as_str()), ("a", "fk1"));
        assert_eq!(k[0].columns, ["x", "y"]);
        assert_eq!(k[0].ref_columns, ["id1", "id2"]);
        assert_eq!(
            (k[0].on_delete.as_str(), k[0].on_update.as_str()),
            ("CASCADE", "NO ACTION")
        );
    }

    #[test]
    fn checks_drop_oracle_not_null_and_strip_clause() {
        let c = fold_checks(vec![
            row(&["SYS_C1", "\"ID\" IS NOT NULL"]),
            row(&["ck_qty", "CHECK ((qty > 0))"]),
            row(&["ck_a", "a < b"]),
        ]);
        assert_eq!(
            c,
            vec![
                StructCheck {
                    name: "ck_a".into(),
                    expression: "a < b".into()
                },
                StructCheck {
                    name: "ck_qty".into(),
                    expression: "qty > 0".into()
                }
            ]
        );
    }

    #[test]
    fn structure_survives_optional_failures() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        let fetch: Fetch = Arc::new(move |s: Stmt| {
            l2.lock().unwrap().push(s.sql.clone());
            let r: Result<Vec<Row>, DbError> = if s.sql.contains("information_schema.COLUMNS") {
                Ok(vec![row(&["id", "int", "N", "~", "Y", "Y", "~", "1"])])
            } else if s.sql.contains("TABLE_NAME = tc.TABLE_NAME") {
                Err(DbError::new(crate::ErrorKind::Query, "unknown column"))
            } else if s.sql.contains("CHECK_CONSTRAINTS") {
                Ok(vec![row(&["c1", "(`id` > 0)"])])
            } else if s.sql.contains("STATISTICS") {
                Err(DbError::new(crate::ErrorKind::Query, "denied"))
            } else {
                Ok(Vec::new())
            };
            Box::pin(async move { r })
        });
        let st =
            futures::executor::block_on(structure(Dialect::Mysql, fetch, "db", "t", 100)).unwrap();
        assert_eq!(st.columns.len(), 1);
        assert!(st.indexes.is_empty());
        assert_eq!(
            st.checks,
            vec![StructCheck {
                name: "c1".into(),
                expression: "`id` > 0".into()
            }]
        );
        assert!(log.lock().unwrap().iter().any(|s| s.contains("TRIGGERS")));
    }
}
