//! Column metadata: server type names as a user would write them in DDL.

use db_core::{BatchBuilder, ColumnMeta, ColumnOrigin};
use mysql_async::consts::{ColumnFlags, ColumnType};
use mysql_async::{Column, Value};

/// Collation id 63 is `binary`: the column holds bytes, not text.
const BINARY_CHARSET: u16 = 63;

pub(crate) fn column_meta(col: &Column) -> ColumnMeta {
    ColumnMeta {
        name: col.name_str().into_owned(),
        type_name: type_name(col),
        nullable: Some(!col.flags().contains(ColumnFlags::NOT_NULL_FLAG)),
        origin: origin(col),
    }
}

/// The table column behind a result column, from the column definition
/// packet. An empty `org_table` means an expression (or a derived column).
fn origin(col: &Column) -> Option<ColumnOrigin> {
    let table = col.org_table_str();
    if table.is_empty() {
        return None;
    }
    Some(ColumnOrigin {
        schema: col.schema_str().into_owned(),
        table: table.into_owned(),
        column: col.org_name_str().into_owned(),
    })
}

/// Text form of a cell. The text protocol only sends bytes or NULL; the
/// binary protocol (prepared statements) sends typed values, which are
/// written the way the text protocol would have written them.
pub(crate) fn push_cell(builder: &mut BatchBuilder, value: Option<&Value>, ty: ColumnType) {
    use std::fmt::Write;
    let mut s = String::new();
    match value {
        Some(Value::Bytes(bytes)) => return builder.push(Some(bytes)),
        Some(Value::NULL) | None => return builder.push(None),
        Some(Value::Int(v)) => write!(s, "{v}").unwrap(),
        Some(Value::UInt(v)) => write!(s, "{v}").unwrap(),
        Some(Value::Float(v)) => write!(s, "{v}").unwrap(),
        Some(Value::Double(v)) => write!(s, "{v}").unwrap(),
        Some(&Value::Date(y, mo, d, h, mi, sec, us)) => {
            write!(s, "{y:04}-{mo:02}-{d:02}").unwrap();
            if !matches!(
                ty,
                ColumnType::MYSQL_TYPE_DATE | ColumnType::MYSQL_TYPE_NEWDATE
            ) {
                write!(s, " {h:02}:{mi:02}:{sec:02}").unwrap();
                if us > 0 {
                    write!(s, ".{us:06}").unwrap();
                }
            }
        }
        Some(&Value::Time(neg, days, h, mi, sec, us)) => {
            let hours = u32::from(h) + days * 24;
            write!(
                s,
                "{}{hours:02}:{mi:02}:{sec:02}",
                if neg { "-" } else { "" }
            )
            .unwrap();
            if us > 0 {
                write!(s, ".{us:06}").unwrap();
            }
        }
    }
    builder.push(Some(s.as_bytes()));
}

fn type_name(col: &Column) -> String {
    use ColumnType::*;
    let flags = col.flags();
    let binary = col.character_set() == BINARY_CHARSET;
    let int = |name: &str| {
        if flags.contains(ColumnFlags::UNSIGNED_FLAG) {
            format!("{name} UNSIGNED")
        } else {
            name.to_string()
        }
    };
    let text_or_blob = |text: &str, blob: &str| {
        if binary {
            blob.to_string()
        } else {
            text.to_string()
        }
    };
    let string = |plain: &str, bin: &str| {
        if flags.contains(ColumnFlags::ENUM_FLAG) {
            "ENUM".to_string()
        } else if flags.contains(ColumnFlags::SET_FLAG) {
            "SET".to_string()
        } else if binary {
            bin.to_string()
        } else {
            plain.to_string()
        }
    };
    match col.column_type() {
        MYSQL_TYPE_TINY => int("TINYINT"),
        MYSQL_TYPE_SHORT => int("SMALLINT"),
        MYSQL_TYPE_INT24 => int("MEDIUMINT"),
        MYSQL_TYPE_LONG => int("INT"),
        MYSQL_TYPE_LONGLONG => int("BIGINT"),
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "DECIMAL".into(),
        MYSQL_TYPE_FLOAT => "FLOAT".into(),
        MYSQL_TYPE_DOUBLE => "DOUBLE".into(),
        MYSQL_TYPE_NULL => "NULL".into(),
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "TIMESTAMP".into(),
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "DATE".into(),
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "TIME".into(),
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "DATETIME".into(),
        MYSQL_TYPE_YEAR => "YEAR".into(),
        MYSQL_TYPE_BIT => "BIT".into(),
        MYSQL_TYPE_JSON => "JSON".into(),
        MYSQL_TYPE_ENUM => "ENUM".into(),
        MYSQL_TYPE_SET => "SET".into(),
        MYSQL_TYPE_GEOMETRY => "GEOMETRY".into(),
        MYSQL_TYPE_VECTOR => "VECTOR".into(),
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING => string("VARCHAR", "VARBINARY"),
        MYSQL_TYPE_STRING => string("CHAR", "BINARY"),
        MYSQL_TYPE_TINY_BLOB => text_or_blob("TINYTEXT", "TINYBLOB"),
        MYSQL_TYPE_BLOB => text_or_blob("TEXT", "BLOB"),
        MYSQL_TYPE_MEDIUM_BLOB => text_or_blob("MEDIUMTEXT", "MEDIUMBLOB"),
        MYSQL_TYPE_LONG_BLOB => text_or_blob("LONGTEXT", "LONGBLOB"),
        other => format!("{other:?}"),
    }
}
