//! Import a CSV / TSV / spreadsheet file into a table.
//!
//! * `source`: streamed reading (encoding and delimiter detection, sheets)
//! * `convert`: text cell → canonical parameter text (numbers, booleans, dates)
//! * `infer`: column types from the preview, SQL type per engine
//! * `sqlgen`: quoting, CREATE TABLE, multi-row INSERT per engine
//! * `run`: the batched, cancellable import on one pinned session
//!
//! Nothing here needs a window; the wizard is `crates/app/src/import`.

pub mod convert;
pub mod infer;
pub mod run;
pub mod source;
pub mod sqlgen;

pub use convert::{ConvertOptions, DateFormat, TargetKind};
pub use run::{ImportJob, ImportOptions, OnError, Progress, Report, RowError, TxScope, run};
pub use source::{CsvOptions, SourceSpec};

use crate::txn::Engine;

/// A column of an existing target table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetColumn {
    pub name: String,
    pub type_name: String,
}

/// Source column → target column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnMap {
    /// Index of the source column.
    pub source: usize,
    pub target: String,
    pub kind: TargetKind,
}

/// The kind of a column of this engine (Oracle's DATE holds a time).
pub fn kind_of(engine: Engine, type_name: &str) -> TargetKind {
    let k = TargetKind::of_type(type_name);
    if engine == Engine::Oracle && k == TargetKind::Date {
        TargetKind::DateTime
    } else {
        k
    }
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// For every target column the source column of the same name (case and
/// punctuation ignored); with no header, the same position.
pub fn auto_map(
    source_names: &[String],
    has_header: bool,
    targets: &[TargetColumn],
) -> Vec<Option<usize>> {
    let mut used = vec![false; source_names.len()];
    let mut out: Vec<Option<usize>> = targets
        .iter()
        .map(|t| {
            let n = norm(&t.name);
            let i = (0..source_names.len()).find(|&i| !used[i] && norm(&source_names[i]) == n)?;
            used[i] = true;
            Some(i)
        })
        .collect();
    if !has_header {
        out = (0..targets.len())
            .map(|i| (i < source_names.len()).then_some(i))
            .collect();
    }
    out
}

/// The inverse of `auto_map`: for every source column the target column index.
pub fn auto_map_sources(
    source_names: &[String],
    has_header: bool,
    targets: &[TargetColumn],
) -> Vec<Option<usize>> {
    let mut out = vec![None; source_names.len()];
    for (t, s) in auto_map(source_names, has_header, targets)
        .into_iter()
        .enumerate()
    {
        if let Some(s) = s {
            out[s] = Some(t);
        }
    }
    out
}

/// New table from the preview: one column per source column, named as in the
/// file, typed from the sample. `types` are the SQL types (editable in the
/// wizard); `skip[i]` leaves source column i out.
pub fn plan_new_table(
    names: &[String],
    sql_types: &[String],
    skip: &[bool],
    engine: Engine,
) -> (Vec<sqlgen::NewColumn>, Vec<ColumnMap>) {
    let mut cols = Vec::new();
    let mut maps = Vec::new();
    for (i, n) in names.iter().enumerate() {
        if skip.get(i).copied().unwrap_or(false) {
            continue;
        }
        let t = sql_types.get(i).cloned().unwrap_or_else(|| "TEXT".into());
        maps.push(ColumnMap {
            source: i,
            target: n.clone(),
            kind: kind_of(engine, &t),
        });
        cols.push(sqlgen::NewColumn {
            name: n.clone(),
            sql_type: t,
        });
    }
    (cols, maps)
}

/// A typed new-table name `schema.table` picks that schema; a plain name
/// stays in `default_schema`. More than one dot is left alone (the name is
/// then taken whole and quoted by the engine).
pub fn split_target(default_schema: Option<&str>, typed: &str) -> (Option<String>, String) {
    let t = typed.trim();
    match t.split_once('.') {
        Some((s, n)) if !s.trim().is_empty() && !n.trim().is_empty() && !n.contains('.') => {
            (Some(s.trim().to_string()), n.trim().to_string())
        }
        _ => (default_schema.map(String::from), t.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[::core::prelude::v1::test]
    fn typed_schema_in_new_table_name() {
        assert_eq!(
            split_target(Some("bench"), "t"),
            (Some("bench".into()), "t".into())
        );
        assert_eq!(
            split_target(Some("bench"), " wb.t "),
            (Some("wb".into()), "t".into())
        );
        assert_eq!(split_target(None, "t"), (None, "t".into()));
        assert_eq!(
            split_target(Some("bench"), "a.b.c"),
            (Some("bench".into()), "a.b.c".into())
        );
        assert_eq!(
            split_target(Some("bench"), ".t"),
            (Some("bench".into()), ".t".into())
        );
    }

    fn tc(n: &str) -> TargetColumn {
        TargetColumn {
            name: n.into(),
            type_name: "text".into(),
        }
    }

    #[test]
    fn maps_by_name() {
        let src: Vec<String> = ["Student ID", "name", "Age"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let m = auto_map(
            &src,
            true,
            &[tc("age"), tc("STUDENT_ID"), tc("name"), tc("extra")],
        );
        assert_eq!(m, vec![Some(2), Some(0), Some(1), None]);
    }

    #[test]
    fn maps_per_source() {
        let src: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            auto_map_sources(&src, true, &[tc("c"), tc("a")]),
            vec![Some(1), None, Some(0)]
        );
    }

    #[test]
    fn maps_by_position_without_header() {
        let src: Vec<String> = ["col1", "col2"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            auto_map(&src, false, &[tc("a"), tc("b"), tc("c")]),
            vec![Some(0), Some(1), None]
        );
    }

    #[test]
    fn oracle_date_has_time() {
        assert_eq!(kind_of(Engine::Oracle, "DATE"), TargetKind::DateTime);
        assert_eq!(kind_of(Engine::Mysql, "date"), TargetKind::Date);
    }
}
