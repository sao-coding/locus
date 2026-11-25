//! Column type inference from the preview rows, and the SQL type per engine
//! for "Import into new table".

use super::convert::{ConvertOptions, TargetKind, parse_bool, parse_datetime, parse_number};
use crate::txn::Engine;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColType {
    Int,
    /// Digits before and after the decimal point.
    Decimal {
        int_digits: u32,
        scale: u32,
    },
    Bool,
    Date,
    DateTime,
    /// Longest text seen, in characters.
    Text {
        max_len: u32,
    },
}

impl ColType {
    pub fn kind(&self) -> TargetKind {
        match self {
            ColType::Int => TargetKind::Integer,
            ColType::Decimal { .. } => TargetKind::Number,
            ColType::Bool => TargetKind::Bool,
            ColType::Date => TargetKind::Date,
            ColType::DateTime => TargetKind::DateTime,
            ColType::Text { .. } => TargetKind::Text,
        }
    }

    /// The column type of a CREATE TABLE.
    pub fn sql(&self, e: Engine) -> String {
        use ColType::*;
        match (self, e) {
            (Int, Engine::Sqlite) => "INTEGER".into(),
            (Int, Engine::Oracle) => "NUMBER(19)".into(),
            (Int, _) => "BIGINT".into(),
            (Decimal { .. }, Engine::Sqlite) => "REAL".into(),
            (Decimal { int_digits, scale }, e) => {
                let p = (int_digits + scale).clamp(1, 38);
                let s = (*scale).min(p);
                match e {
                    Engine::Postgres => format!("NUMERIC({p},{s})"),
                    Engine::Oracle => format!("NUMBER({p},{s})"),
                    _ => format!("DECIMAL({p},{s})"),
                }
            }
            (Bool, Engine::Mysql) => "TINYINT(1)".into(),
            (Bool, Engine::Postgres) => "BOOLEAN".into(),
            (Bool, Engine::Sqlite) => "INTEGER".into(),
            (Bool, Engine::Oracle) => "NUMBER(1)".into(),
            (Bool, Engine::SqlServer) => "BIT".into(),
            (Date, Engine::Sqlite) => "TEXT".into(),
            (Date, _) => "DATE".into(),
            (DateTime, Engine::Mysql) => "DATETIME".into(),
            (DateTime, Engine::Postgres) | (DateTime, Engine::Oracle) => "TIMESTAMP".into(),
            (DateTime, Engine::Sqlite) => "TEXT".into(),
            (DateTime, Engine::SqlServer) => "DATETIME2".into(),
            (Text { max_len }, e) => text_type(*max_len, e),
        }
    }
}

/// Room is left above the longest preview value: later rows can be longer.
fn text_type(max_len: u32, e: Engine) -> String {
    let want = max_len.saturating_mul(2).max(1);
    match e {
        Engine::Sqlite | Engine::Postgres => "TEXT".into(),
        Engine::Mysql => match want {
            0..=255 => "VARCHAR(255)".into(),
            256..=1000 => "VARCHAR(1000)".into(),
            1001..=16000 => "TEXT".into(),
            _ => "LONGTEXT".into(),
        },
        Engine::Oracle => match want {
            0..=100 => "VARCHAR2(400 CHAR)".into(),
            101..=1000 => "VARCHAR2(1000 CHAR)".into(),
            1001..=1300 => "VARCHAR2(1300 CHAR)".into(),
            _ => "CLOB".into(),
        },
        Engine::SqlServer => match want {
            0..=4000 => format!("NVARCHAR({})", want.clamp(255, 4000)),
            _ => "NVARCHAR(MAX)".into(),
        },
    }
}

/// Infers every column from `rows` (each row's cells in column order).
/// A column is a number / boolean / date only when every non-empty cell
/// parses as one; otherwise it is text.
pub fn infer_columns(rows: &[Vec<String>], ncols: usize, o: &ConvertOptions) -> Vec<ColType> {
    (0..ncols)
        .map(|c| {
            infer_one(
                rows.iter()
                    .map(|r| r.get(c).map(String::as_str).unwrap_or("")),
                o,
            )
        })
        .collect()
}

fn infer_one<'a>(cells: impl Iterator<Item = &'a str>, o: &ConvertOptions) -> ColType {
    let vals: Vec<&str> = cells.map(|c| c.trim()).filter(|c| !c.is_empty()).collect();
    let text = || ColType::Text {
        max_len: vals
            .iter()
            .map(|v| v.chars().count() as u32)
            .max()
            .unwrap_or(0),
    };
    if vals.is_empty() {
        return text();
    }
    // Booleans first: only words (a 0/1 column is more likely a number).
    if vals.iter().all(|v| parse_bool(v).is_ok()) && vals.iter().any(|v| !matches!(*v, "0" | "1")) {
        return ColType::Bool;
    }
    if vals.iter().all(|v| parse_number(v).is_ok()) {
        let nums: Vec<String> = vals.iter().map(|v| parse_number(v).unwrap()).collect();
        // A leading zero ("007", phone numbers, codes) is text.
        if vals.iter().any(|v| {
            let d = v.trim_start_matches(['-', '+']);
            d.len() > 1 && d.starts_with('0') && !d.starts_with("0.") && !d.starts_with("0,")
        }) {
            return text();
        }
        if nums.iter().any(|n| n.contains('e')) {
            return ColType::Decimal {
                int_digits: 20,
                scale: 10,
            };
        }
        let (mut idig, mut scale) = (1u32, 0u32);
        for n in &nums {
            let n = n.trim_start_matches('-');
            let (i, f) = n.split_once('.').unwrap_or((n, ""));
            idig = idig.max(i.len() as u32);
            scale = scale.max(f.trim_end_matches('0').len() as u32);
        }
        if scale == 0 {
            return if idig <= 18 { ColType::Int } else { text() };
        }
        // Headroom for later rows: two more integer digits.
        return if idig + 2 + scale <= 38 {
            ColType::Decimal {
                int_digits: idig + 2,
                scale,
            }
        } else {
            text()
        };
    }
    let fmt = &o.date_format;
    if vals.iter().all(|v| parse_datetime(v, fmt).is_ok()) {
        let any_time = vals
            .iter()
            .any(|v| parse_datetime(v, fmt).map(|s| s.has_time).unwrap_or(false));
        return if any_time {
            ColType::DateTime
        } else {
            ColType::Date
        };
    }
    text()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(vals: &[&str]) -> ColType {
        let rows: Vec<Vec<String>> = vals.iter().map(|v| vec![v.to_string()]).collect();
        infer_columns(&rows, 1, &ConvertOptions::default()).remove(0)
    }

    #[test]
    fn infers() {
        assert_eq!(col(&["1", "2", "", "1,000"]), ColType::Int);
        assert_eq!(
            col(&["1.5", "20.25"]),
            ColType::Decimal {
                int_digits: 4,
                scale: 2
            }
        );
        assert_eq!(col(&["yes", "no"]), ColType::Bool);
        assert_eq!(col(&["0", "1", "1"]), ColType::Int);
        assert_eq!(col(&["2024-01-02", "2024/3/4"]), ColType::Date);
        assert_eq!(col(&["2024-01-02", "2024-03-04 10:00"]), ColType::DateTime);
        assert_eq!(col(&["007", "12"]), ColType::Text { max_len: 3 });
        assert_eq!(col(&["abc", "12"]), ColType::Text { max_len: 3 });
        assert_eq!(col(&["", ""]), ColType::Text { max_len: 0 });
        assert_eq!(col(&["王小明", "x"]), ColType::Text { max_len: 3 });
        assert_eq!(
            col(&["12345678901234567890"]),
            ColType::Text { max_len: 20 }
        );
    }

    #[test]
    fn sql_types() {
        let t = ColType::Decimal {
            int_digits: 4,
            scale: 2,
        };
        assert_eq!(t.sql(Engine::Mysql), "DECIMAL(6,2)");
        assert_eq!(t.sql(Engine::Postgres), "NUMERIC(6,2)");
        assert_eq!(t.sql(Engine::Oracle), "NUMBER(6,2)");
        assert_eq!(t.sql(Engine::Sqlite), "REAL");
        assert_eq!(ColType::Int.sql(Engine::Oracle), "NUMBER(19)");
        assert_eq!(ColType::Bool.sql(Engine::Postgres), "BOOLEAN");
        assert_eq!(ColType::DateTime.sql(Engine::Mysql), "DATETIME");
        assert_eq!(
            ColType::Text { max_len: 10 }.sql(Engine::Mysql),
            "VARCHAR(255)"
        );
        assert_eq!(
            ColType::Text { max_len: 20000 }.sql(Engine::Mysql),
            "LONGTEXT"
        );
        assert_eq!(
            ColType::Text { max_len: 50 }.sql(Engine::Oracle),
            "VARCHAR2(400 CHAR)"
        );
        assert_eq!(ColType::Text { max_len: 5000 }.sql(Engine::Oracle), "CLOB");
        assert_eq!(
            ColType::Text { max_len: 5000 }.sql(Engine::Postgres),
            "TEXT"
        );
    }
}
