//! "Copy as": CSV, JSON, INSERT statements and a Markdown table from rows
//! that were already read out of the grid. Pure text work.

/// Most rows one copy puts on the clipboard.
pub const COPY_CAP: usize = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Text(String),
    /// Binary cell as `0x…` hex digits (no prefix stored).
    Hex(String),
}

impl Value {
    fn plain(&self) -> Option<String> {
        match self {
            Value::Null => None,
            Value::Text(s) => Some(s.clone()),
            Value::Hex(h) => Some(format!("0x{h}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Mysql,
    Postgres,
    Oracle,
    Sqlite,
}

pub struct Table {
    pub columns: Vec<String>,
    /// Column holds numbers (decided from the server type name): written
    /// unquoted in JSON and INSERT when the text is a plain number.
    pub numeric: Vec<bool>,
    pub rows: Vec<Vec<Value>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Csv,
    Json,
    Insert,
    Markdown,
    Tsv,
    JsonLines,
    Html,
}

pub fn render(
    fmt: Format,
    t: &Table,
    table_name: &str,
    dialect: Dialect,
    quote: &dyn Fn(&str) -> String,
) -> String {
    match fmt {
        Format::Csv => csv(t),
        Format::Json => json(t),
        Format::Insert => insert(t, table_name, dialect, quote),
        Format::Markdown => markdown(t),
        Format::Tsv => tsv(t),
        Format::JsonLines => json_lines(t),
        Format::Html => html(t),
    }
}

/// Server type names that hold numbers.
pub fn is_numeric_type(type_name: &str) -> bool {
    let t = type_name.to_ascii_lowercase();
    let t = t.split(['(', ' ']).next().unwrap_or("");
    matches!(
        t,
        "int"
            | "integer"
            | "tinyint"
            | "smallint"
            | "mediumint"
            | "bigint"
            | "decimal"
            | "numeric"
            | "float"
            | "double"
            | "real"
            | "number"
            | "int2"
            | "int4"
            | "int8"
            | "float4"
            | "float8"
            | "serial"
            | "bigserial"
            | "money"
            | "binary_float"
            | "binary_double"
    )
}

/// Plain decimal number in JSON's grammar (no `+`, no leading zeros, no `inf`).
pub fn is_json_number(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if b.get(i) == Some(&b'-') {
        i += 1;
    }
    let int_start = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    let int_len = i - int_start;
    if int_len == 0 || (int_len > 1 && b[int_start] == b'0') {
        return false;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        let f = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == f {
            return false;
        }
    }
    if matches!(b.get(i), Some(b'e') | Some(b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+') | Some(b'-')) {
            i += 1;
        }
        let e = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == e {
            return false;
        }
    }
    i == b.len()
}

fn csv_field(v: &Value) -> String {
    match v.plain() {
        None => String::new(),
        Some(s) if s.contains([',', '"', '\n', '\r']) => format!("\"{}\"", s.replace('"', "\"\"")),
        Some(s) => s,
    }
}

pub fn csv(t: &Table) -> String {
    let head = t
        .columns
        .iter()
        .map(|c| csv_field(&Value::Text(c.clone())))
        .collect::<Vec<_>>()
        .join(",");
    let mut out = vec![head];
    for r in &t.rows {
        out.push(r.iter().map(csv_field).collect::<Vec<_>>().join(","));
    }
    out.join("\r\n")
}

fn json_string(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

pub fn json(t: &Table) -> String {
    if t.rows.is_empty() {
        return "[]".into();
    }
    let mut out = String::from("[\n");
    for (i, r) in t.rows.iter().enumerate() {
        out.push_str("  {\n");
        for (c, v) in r.iter().enumerate() {
            let val = match v {
                Value::Null => "null".to_string(),
                Value::Text(s)
                    if t.numeric.get(c).copied().unwrap_or(false) && is_json_number(s) =>
                {
                    s.clone()
                }
                other => json_string(&other.plain().unwrap_or_default()),
            };
            out.push_str(&format!("    {}: {}", json_string(&t.columns[c]), val));
            out.push_str(if c + 1 < r.len() { ",\n" } else { "\n" });
        }
        out.push_str(if i + 1 < t.rows.len() {
            "  },\n"
        } else {
            "  }\n"
        });
    }
    out.push(']');
    out
}

pub fn sql_literal(v: &Value, numeric: bool, d: Dialect) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Hex(h) => match d {
            Dialect::Mysql => format!("0x{h}"),
            Dialect::Postgres => format!("'\\x{h}'"),
            Dialect::Oracle => format!("HEXTORAW('{h}')"),
            Dialect::Sqlite => format!("X'{h}'"),
        },
        Value::Text(s) if numeric && is_json_number(s) => s.clone(),
        Value::Text(s) => {
            let esc = if d == Dialect::Mysql {
                s.replace('\\', "\\\\").replace('\'', "''")
            } else {
                s.replace('\'', "''")
            };
            format!("'{esc}'")
        }
    }
}

/// `INSERT INTO <table> (<cols>) VALUES (…);` one line per row, identifiers
/// quoted by `quote` (the connection's dialect).
pub fn insert(t: &Table, table_name: &str, d: Dialect, quote: &dyn Fn(&str) -> String) -> String {
    let cols = t
        .columns
        .iter()
        .map(|c| quote(c))
        .collect::<Vec<_>>()
        .join(", ");
    let tbl = quote(table_name);
    t.rows
        .iter()
        .map(|r| {
            let vals = r
                .iter()
                .enumerate()
                .map(|(c, v)| sql_literal(v, t.numeric.get(c).copied().unwrap_or(false), d))
                .collect::<Vec<_>>()
                .join(", ");
            format!("INSERT INTO {tbl} ({cols}) VALUES ({vals});")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn md_cell(v: &Value) -> String {
    match v.plain() {
        None => "NULL".into(),
        Some(s) => s
            .replace('|', "\\|")
            .replace("\r\n", "<br>")
            .replace(['\n', '\r'], "<br>"),
    }
}

pub fn markdown(t: &Table) -> String {
    let head = format!(
        "| {} |",
        t.columns
            .iter()
            .map(|c| md_cell(&Value::Text(c.clone())))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    let sep = format!(
        "|{}|",
        t.columns
            .iter()
            .map(|_| "---")
            .collect::<Vec<_>>()
            .join("|")
    );
    let mut out = vec![head, sep];
    for r in &t.rows {
        out.push(format!(
            "| {} |",
            r.iter().map(md_cell).collect::<Vec<_>>().join(" | ")
        ));
    }
    out.join("\n")
}

fn tsv_field(v: &Value) -> String {
    v.plain()
        .map(|s| {
            s.replace('\\', "\\\\")
                .replace('\t', "\\t")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
        })
        .unwrap_or_default()
}

/// Tab-separated, header first; tab, newline and backslash escaped, NULL empty.
pub fn tsv(t: &Table) -> String {
    let mut out = vec![
        t.columns
            .iter()
            .map(|c| tsv_field(&Value::Text(c.clone())))
            .collect::<Vec<_>>()
            .join("\t"),
    ];
    for r in &t.rows {
        out.push(r.iter().map(tsv_field).collect::<Vec<_>>().join("\t"));
    }
    out.join("\r\n")
}

/// One compact JSON object per line.
pub fn json_lines(t: &Table) -> String {
    t.rows
        .iter()
        .map(|r| {
            let cells = r
                .iter()
                .enumerate()
                .map(|(c, v)| {
                    let val = match v {
                        Value::Null => "null".to_string(),
                        Value::Text(s)
                            if t.numeric.get(c).copied().unwrap_or(false) && is_json_number(s) =>
                        {
                            s.clone()
                        }
                        other => json_string(&other.plain().unwrap_or_default()),
                    };
                    format!("{}:{}", json_string(&t.columns[c]), val)
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", cells.join(","))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn html_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A bare `<table>` (pastes into mail and documents).
pub fn html(t: &Table) -> String {
    let mut out = String::from("<table>\n<thead><tr>");
    for c in &t.columns {
        out.push_str(&format!("<th>{}</th>", html_text(c)));
    }
    out.push_str("</tr></thead>\n<tbody>\n");
    for r in &t.rows {
        out.push_str("<tr>");
        for v in r {
            match v.plain() {
                None => out.push_str("<td></td>"),
                Some(s) => out.push_str(&format!("<td>{}</td>", html_text(&s))),
            }
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</tbody>\n</table>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn tsv_jsonl_html_copy() {
        let tb = sample();
        assert_eq!(
            tsv(&tb),
            "id\tname\tnote\tblob\r\n1\tAnn, \"A\" Lee\t\t0xCAFE\r\n02\tit's a\\\\b\ttwo\\nlines | pipe\t"
        );
        let jl = json_lines(&tb);
        let lines: Vec<&str> = jl.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            "{\"id\":1,\"name\":\"Ann, \\\"A\\\" Lee\",\"note\":null,\"blob\":\"0xCAFE\"}"
        );
        assert!(lines[1].starts_with("{\"id\":\"02\""), "{}", lines[1]);
        let h = html(&tb);
        assert!(
            h.contains("<td>Ann, &quot;A&quot; Lee</td><td></td><td>0xCAFE</td>"),
            "{h}"
        );
        assert!(h.starts_with("<table>") && h.ends_with("</table>"));
    }

    fn t(s: &str) -> Value {
        Value::Text(s.into())
    }

    fn sample() -> Table {
        Table {
            columns: vec!["id".into(), "name".into(), "note".into(), "blob".into()],
            numeric: vec![true, false, false, false],
            rows: vec![
                vec![
                    t("1"),
                    t("Ann, \"A\" Lee"),
                    Value::Null,
                    Value::Hex("CAFE".into()),
                ],
                vec![t("02"), t("it's a\\b"), t("two\nlines | pipe"), Value::Null],
            ],
        }
    }

    fn mysql_q(n: &str) -> String {
        format!("`{}`", n.replace('`', "``"))
    }

    fn ansi_q(n: &str) -> String {
        format!("\"{}\"", n.replace('"', "\"\""))
    }

    #[::core::prelude::v1::test]
    fn csv_quotes_and_nulls() {
        assert_eq!(
            csv(&sample()),
            "id,name,note,blob\r\n1,\"Ann, \"\"A\"\" Lee\",,0xCAFE\r\n02,it's a\\b,\"two\nlines | pipe\","
        );
    }

    #[::core::prelude::v1::test]
    fn json_types_and_escapes() {
        let j = json(&sample());
        assert!(j.starts_with("[\n  {\n    \"id\": 1,\n    \"name\": \"Ann, \\\"A\\\" Lee\",\n    \"note\": null,\n    \"blob\": \"0xCAFE\"\n  },"), "{j}");
        // "02" is not a JSON number: stays a string even in a numeric column.
        assert!(j.contains("\"id\": \"02\""), "{j}");
        assert!(j.contains("\"note\": \"two\\nlines | pipe\""), "{j}");
        assert!(j.contains("\"name\": \"it's a\\\\b\""), "{j}");
        assert!(j.ends_with("  }\n]"), "{j}");
        assert_eq!(
            json(&Table {
                columns: vec!["a".into()],
                numeric: vec![false],
                rows: vec![]
            }),
            "[]"
        );
    }

    #[::core::prelude::v1::test]
    fn insert_mysql_quotes_with_backticks_and_escapes_backslash() {
        let s = insert(&sample(), "my`t", Dialect::Mysql, &mysql_q);
        let lines: Vec<&str> = s.lines().collect();
        // The second row's note holds a newline, so it spans two lines.
        assert_eq!(
            lines[0],
            "INSERT INTO `my``t` (`id`, `name`, `note`, `blob`) VALUES (1, 'Ann, \"A\" Lee', NULL, 0xCAFE);"
        );
        assert_eq!(
            lines[1],
            "INSERT INTO `my``t` (`id`, `name`, `note`, `blob`) VALUES ('02', 'it''s a\\\\b', 'two"
        );
        assert_eq!(lines[2], "lines | pipe', NULL);");
    }

    #[::core::prelude::v1::test]
    fn insert_postgres_and_oracle_use_double_quotes_and_plain_backslash() {
        let s = insert(&sample(), "T", Dialect::Postgres, &ansi_q);
        assert!(s.starts_with("INSERT INTO \"T\" (\"id\", \"name\", \"note\", \"blob\") VALUES (1, 'Ann, \"A\" Lee', NULL, '\\xCAFE');"), "{s}");
        assert!(s.contains("'it''s a\\b'"), "{s}");
        let o = insert(&sample(), "T", Dialect::Oracle, &ansi_q);
        assert!(o.contains("HEXTORAW('CAFE')"), "{o}");
    }

    #[::core::prelude::v1::test]
    fn markdown_escapes_pipes_and_newlines() {
        let m = markdown(&sample());
        let lines: Vec<&str> = m.lines().collect();
        assert_eq!(lines[0], "| id | name | note | blob |");
        assert_eq!(lines[1], "|---|---|---|---|");
        assert_eq!(lines[2], "| 1 | Ann, \"A\" Lee | NULL | 0xCAFE |");
        assert_eq!(
            lines[3],
            "| 02 | it's a\\b | two<br>lines \\| pipe | NULL |"
        );
    }

    #[::core::prelude::v1::test]
    fn json_number_grammar() {
        for ok in ["0", "-1", "10", "1.5", "-0.25", "1e5", "1E-5", "2.5e+3"] {
            assert!(is_json_number(ok), "{ok}");
        }
        for bad in [
            "", "-", "01", "+1", "1.", ".5", "1e", "inf", "NaN", "0x10", "1 2",
        ] {
            assert!(!is_json_number(bad), "{bad}");
        }
    }

    #[::core::prelude::v1::test]
    fn numeric_types() {
        for n in [
            "INT",
            "bigint(20)",
            "DECIMAL(10,2)",
            "NUMBER",
            "double",
            "int4",
        ] {
            assert!(is_numeric_type(n), "{n}");
        }
        for n in ["VARCHAR", "text", "datetime", "enum", "blob", "date"] {
            assert!(!is_numeric_type(n), "{n}");
        }
    }

    #[::core::prelude::v1::test]
    fn render_dispatches() {
        let tb = sample();
        assert_eq!(
            render(Format::Csv, &tb, "t", Dialect::Mysql, &mysql_q),
            csv(&tb)
        );
        assert_eq!(
            render(Format::Markdown, &tb, "t", Dialect::Mysql, &mysql_q),
            markdown(&tb)
        );
    }
}
