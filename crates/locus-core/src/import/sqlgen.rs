//! SQL text of the import, per engine: quoting, CREATE TABLE, multi-row
//! INSERT with `?` placeholders, clearing the table.

use super::convert::TargetKind;
use crate::txn::Engine;

pub fn quote_ident(e: Engine, name: &str) -> String {
    match e {
        Engine::Mysql => format!("`{}`", name.replace('`', "``")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

pub fn qualified(e: Engine, schema: Option<&str>, table: &str) -> String {
    match schema.filter(|s| !s.is_empty()) {
        Some(s) => format!("{}.{}", quote_ident(e, s), quote_ident(e, table)),
        None => quote_ident(e, table),
    }
}

/// One column of a new table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewColumn {
    pub name: String,
    pub sql_type: String,
}

pub fn create_table_sql(
    e: Engine,
    schema: Option<&str>,
    table: &str,
    cols: &[NewColumn],
) -> String {
    let body: Vec<String> = cols
        .iter()
        .map(|c| format!("  {} {}", quote_ident(e, &c.name), c.sql_type))
        .collect();
    format!(
        "CREATE TABLE {} (\n{}\n)",
        qualified(e, schema, table),
        body.join(",\n")
    )
}

/// The placeholder of one value, with the engine's conversion when the text
/// needs one (Oracle takes dates through TO_DATE / TO_TIMESTAMP).
fn slot(e: Engine, kind: TargetKind) -> &'static str {
    match (e, kind) {
        (Engine::Oracle, TargetKind::Date) => "TO_DATE(?, 'YYYY-MM-DD')",
        (Engine::Oracle, TargetKind::DateTime) => "TO_TIMESTAMP(?, 'YYYY-MM-DD HH24:MI:SS')",
        _ => "?",
    }
}

/// Rows one INSERT statement may carry: the batch size, cut by the engine's
/// limit on bound values (SQLite 32766, MySQL / PostgreSQL / Oracle 65535) and,
/// for Oracle's INSERT ALL, 999 values.
pub fn rows_per_statement(e: Engine, ncols: usize, batch: usize) -> usize {
    let ncols = ncols.max(1);
    let cap = match e {
        Engine::Sqlite => 32000 / ncols,
        Engine::Oracle => 999 / ncols,
        _ => 65000 / ncols,
    };
    batch.clamp(1, cap.max(1))
}

/// `INSERT INTO t (a, b) VALUES (?, ?), (?, ?)`; Oracle has no multi-row
/// VALUES, it gets `INSERT ALL INTO t (a, b) VALUES (?, ?) INTO … SELECT 1 FROM DUAL`.
pub fn insert_sql(e: Engine, table_q: &str, cols: &[(String, TargetKind)], rows: usize) -> String {
    let names: Vec<String> = cols.iter().map(|(n, _)| quote_ident(e, n)).collect();
    let tuple = format!(
        "({})",
        cols.iter()
            .map(|(_, k)| slot(e, *k))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let names = names.join(", ");
    if e == Engine::Oracle {
        let mut s = String::from("INSERT ALL");
        for _ in 0..rows {
            s.push_str(&format!(" INTO {table_q} ({names}) VALUES {tuple}"));
        }
        s.push_str(" SELECT 1 FROM DUAL");
        s
    } else {
        format!(
            "INSERT INTO {table_q} ({names}) VALUES {}",
            vec![tuple; rows].join(", ")
        )
    }
}

/// Empties the table. Inside one transaction it is a DELETE (it rolls back
/// with the import); MySQL / Oracle TRUNCATE would commit on the spot.
pub fn clear_sql(e: Engine, table_q: &str, transactional: bool) -> String {
    match e {
        Engine::Sqlite => format!("DELETE FROM {table_q}"),
        _ if transactional => format!("DELETE FROM {table_q}"),
        _ => format!("TRUNCATE TABLE {table_q}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TargetKind::*;

    #[test]
    fn quoting() {
        assert_eq!(quote_ident(Engine::Mysql, "a`b"), "`a``b`");
        assert_eq!(quote_ident(Engine::Postgres, "a\"b"), "\"a\"\"b\"");
        assert_eq!(qualified(Engine::Oracle, Some("HR"), "T"), "\"HR\".\"T\"");
        assert_eq!(qualified(Engine::Sqlite, None, "t"), "\"t\"");
        assert_eq!(qualified(Engine::Sqlite, Some(""), "t"), "\"t\"");
    }

    #[test]
    fn inserts() {
        let cols = vec![("id".to_string(), Integer), ("d".to_string(), Date)];
        assert_eq!(
            insert_sql(Engine::Mysql, "`t`", &cols, 2),
            "INSERT INTO `t` (`id`, `d`) VALUES (?, ?), (?, ?)"
        );
        assert_eq!(
            insert_sql(Engine::Postgres, "\"t\"", &cols, 1),
            "INSERT INTO \"t\" (\"id\", \"d\") VALUES (?, ?)"
        );
        assert_eq!(
            insert_sql(Engine::Oracle, "\"t\"", &cols, 2),
            "INSERT ALL INTO \"t\" (\"id\", \"d\") VALUES (?, TO_DATE(?, 'YYYY-MM-DD')) INTO \"t\" (\"id\", \"d\") VALUES (?, TO_DATE(?, 'YYYY-MM-DD')) SELECT 1 FROM DUAL"
        );
    }

    #[test]
    fn limits() {
        assert_eq!(rows_per_statement(Engine::Mysql, 10, 1000), 1000);
        assert_eq!(rows_per_statement(Engine::Mysql, 100, 1000), 650);
        assert_eq!(rows_per_statement(Engine::Sqlite, 100, 1000), 320);
        assert_eq!(rows_per_statement(Engine::Oracle, 10, 1000), 99);
        assert_eq!(rows_per_statement(Engine::Oracle, 2000, 1000), 1);
        assert_eq!(rows_per_statement(Engine::Mysql, 3, 0), 1);
    }

    #[test]
    fn create_and_clear() {
        let c = vec![
            NewColumn {
                name: "a".into(),
                sql_type: "BIGINT".into(),
            },
            NewColumn {
                name: "b c".into(),
                sql_type: "TEXT".into(),
            },
        ];
        assert_eq!(
            create_table_sql(Engine::Mysql, Some("s"), "t", &c),
            "CREATE TABLE `s`.`t` (\n  `a` BIGINT,\n  `b c` TEXT\n)"
        );
        assert_eq!(clear_sql(Engine::Mysql, "`t`", true), "DELETE FROM `t`");
        assert_eq!(clear_sql(Engine::Mysql, "`t`", false), "TRUNCATE TABLE `t`");
        assert_eq!(
            clear_sql(Engine::Sqlite, "\"t\"", false),
            "DELETE FROM \"t\""
        );
    }
}
