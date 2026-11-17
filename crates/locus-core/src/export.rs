//! Export a result to a file: CSV (RFC 4180, optional UTF-8 BOM for Excel),
//! TSV, JSON, JSON Lines, Excel .xlsx, SQL INSERT statements, a Markdown
//! table or an HTML table.
//!
//! Rows are written one by one to a buffered file, so memory stays at a few
//! chunks whatever the row count. The grid path reads rows on the UI thread
//! and hands them to a writer thread through a bounded channel; the server
//! path (`export_stream`) streams batches straight from a driver.

use crate::copyfmt::{self, Dialect, Value};
use std::io::Write;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Csv,
    /// CSV with a UTF-8 byte-order mark, so Excel reads CJK text correctly.
    CsvExcel,
    Tsv,
    Json,
    JsonLines,
    Xlsx,
    /// `INSERT INTO … VALUES …;` statements.
    Sql,
    Markdown,
    Html,
}

impl Format {
    /// Every format, in the order the dialog lists them.
    pub const ALL: [Format; 9] = [
        Format::Csv,
        Format::CsvExcel,
        Format::Xlsx,
        Format::Json,
        Format::JsonLines,
        Format::Sql,
        Format::Tsv,
        Format::Markdown,
        Format::Html,
    ];

    pub fn extension(self) -> &'static str {
        match self {
            Format::Csv | Format::CsvExcel => "csv",
            Format::Tsv => "tsv",
            Format::Json => "json",
            Format::JsonLines => "jsonl",
            Format::Xlsx => "xlsx",
            Format::Sql => "sql",
            Format::Markdown => "md",
            Format::Html => "html",
        }
    }

    /// Locale key of the format's name (`export.fmt_<id>`).
    pub fn id(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::CsvExcel => "csv_excel",
            Format::Tsv => "tsv",
            Format::Json => "json",
            Format::JsonLines => "jsonl",
            Format::Xlsx => "xlsx",
            Format::Sql => "sql",
            Format::Markdown => "md",
            Format::Html => "html",
        }
    }
}

pub type Row = Vec<Option<String>>;

/// How a column's text is written where the format has types (xlsx, JSON,
/// SQL).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColKind {
    #[default]
    Text,
    Number,
    /// DATE / DATETIME / TIMESTAMP (text `YYYY-MM-DD[ hh:mm:ss[.fff]]`).
    Date,
    /// BLOB / BINARY: the text is `0x` and hex digits.
    Binary,
}

/// Decides a column's kind from the server's type name.
pub fn col_kind(type_name: &str) -> ColKind {
    if copyfmt::is_numeric_type(type_name) {
        return ColKind::Number;
    }
    let t = type_name.to_ascii_lowercase();
    let t = t.split(['(', ' ']).next().unwrap_or("");
    if t.starts_with("timestamp")
        || matches!(t, "date" | "datetime" | "datetime2" | "smalldatetime")
    {
        ColKind::Date
    } else if t.ends_with("blob")
        || matches!(
            t,
            "binary" | "varbinary" | "bytea" | "raw" | "longraw" | "bfile"
        )
    {
        ColKind::Binary
    } else {
        ColKind::Text
    }
}

/// What a writer needs to know besides the rows.
#[derive(Clone, Debug)]
pub struct Spec {
    pub columns: Vec<String>,
    /// Same length as `columns`; missing entries count as text.
    pub kinds: Vec<ColKind>,
    /// Target of the INSERT statements; may be `schema.table`.
    pub table: String,
    pub dialect: Dialect,
    /// Rows per INSERT statement (Oracle always writes one).
    pub batch: usize,
    /// CSV / TSV: write the column names first.
    pub header: bool,
    /// xlsx: rows per sheet including the header row (Excel's limit is
    /// 1,048,576); more rows go to the next sheet.
    pub sheet_rows: u32,
}

impl Spec {
    pub fn new(columns: Vec<String>) -> Self {
        Self {
            kinds: vec![ColKind::Text; columns.len()],
            columns,
            table: "table".into(),
            dialect: Dialect::Mysql,
            batch: 100,
            header: true,
            sheet_rows: XLSX_MAX_ROWS,
        }
    }

    fn kind(&self, c: usize) -> ColKind {
        self.kinds.get(c).copied().unwrap_or_default()
    }
}

pub const XLSX_MAX_ROWS: u32 = 1_048_576;

pub fn csv_field(v: &Option<String>) -> String {
    match v {
        None => String::new(),
        Some(s) if s.contains([',', '"', '\n', '\r']) => format!("\"{}\"", s.replace('"', "\"\"")),
        Some(s) => s.clone(),
    }
}

/// Identifier quoting for the INSERT target: backticks for MySQL, double
/// quotes elsewhere. `a.b` is quoted per part.
pub fn quote_name(d: Dialect, name: &str) -> String {
    let q = |p: &str| match d {
        Dialect::Mysql => format!("`{}`", p.replace('`', "``")),
        _ => format!("\"{}\"", p.replace('"', "\"\"")),
    };
    name.split('.')
        .filter(|p| !p.is_empty())
        .map(q)
        .collect::<Vec<_>>()
        .join(".")
}

fn tsv_field(v: &Option<String>) -> String {
    match v {
        None => String::new(),
        Some(s) => s
            .replace('\\', "\\\\")
            .replace('\t', "\\t")
            .replace('\n', "\\n")
            .replace('\r', "\\r"),
    }
}

fn md_cell(v: &Option<String>) -> String {
    match v {
        None => "NULL".into(),
        Some(s) => s
            .replace('|', "\\|")
            .replace("\r\n", "<br>")
            .replace(['\n', '\r'], "<br>"),
    }
}

pub fn html_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c => o.push(c),
        }
    }
    o
}

/// One value as the SQL literal the INSERT writer emits.
fn sql_value(v: &Option<String>, kind: ColKind, d: Dialect) -> String {
    let Some(s) = v else { return "NULL".into() };
    match kind {
        ColKind::Binary => match s.strip_prefix("0x") {
            Some(h)
                if !h.is_empty()
                    && h.len() % 2 == 0
                    && h.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                copyfmt::sql_literal(&Value::Hex(h.to_ascii_uppercase()), false, d)
            }
            _ => copyfmt::sql_literal(&Value::Text(s.clone()), false, d),
        },
        ColKind::Date if d == Dialect::Oracle => match date_parts(s) {
            Some((_, false)) => format!("DATE '{s}'"),
            Some((_, true)) => format!("TIMESTAMP '{s}'"),
            None => copyfmt::sql_literal(&Value::Text(s.clone()), false, d),
        },
        k => copyfmt::sql_literal(&Value::Text(s.clone()), k == ColKind::Number, d),
    }
}

/// `YYYY-MM-DD` or `YYYY-MM-DD hh:mm:ss[.f…]`: whether the text is one, and
/// whether it has a time part.
pub(crate) fn date_parts(s: &str) -> Option<((), bool)> {
    let b = s.as_bytes();
    let digits =
        |r: std::ops::Range<usize>| b.get(r).is_some_and(|x| x.iter().all(u8::is_ascii_digit));
    if b.len() < 10
        || !digits(0..4)
        || b[4] != b'-'
        || !digits(5..7)
        || b[7] != b'-'
        || !digits(8..10)
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s[r].parse::<u32>().unwrap_or(0);
    if num(0..4) == 0 || !(1..=12).contains(&num(5..7)) || !(1..=31).contains(&num(8..10)) {
        return None;
    }
    if b.len() == 10 {
        return Some(((), false));
    }
    if b.len() < 19
        || (b[10] != b' ' && b[10] != b'T')
        || !digits(11..13)
        || b[13] != b':'
        || !digits(14..16)
        || b[16] != b':'
        || !digits(17..19)
    {
        return None;
    }
    match b.get(19) {
        None => Some(((), true)),
        Some(b'.') if b.len() > 20 && digits(20..b.len()) => Some(((), true)),
        _ => None,
    }
}

fn json_key(out: &mut impl Write, s: &str) -> std::io::Result<()> {
    serde_json::to_writer(out, s).map_err(std::io::Error::other)
}

fn json_object(out: &mut impl Write, spec: &Spec, row: &Row) -> std::io::Result<()> {
    out.write_all(b"{")?;
    for (c, (name, v)) in spec.columns.iter().zip(row).enumerate() {
        if c > 0 {
            out.write_all(b",")?;
        }
        json_key(out, name)?;
        out.write_all(b":")?;
        match v {
            None => out.write_all(b"null")?,
            Some(s) if spec.kind(c) == ColKind::Number && copyfmt::is_json_number(s) => {
                out.write_all(s.as_bytes())?
            }
            Some(s) => json_key(out, s)?,
        }
    }
    out.write_all(b"}")
}

enum Inner {
    Text(TextWriter),
    Xlsx(Box<crate::export_xlsx::XlsxWriter>),
}

struct TextWriter {
    out: std::io::BufWriter<std::fs::File>,
    format: Format,
    spec: Spec,
    rows: u64,
    /// SQL: rows written into the INSERT statement that is open.
    in_batch: usize,
}

pub struct Writer {
    inner: Inner,
}

impl Writer {
    pub fn create(
        path: &std::path::Path,
        format: Format,
        columns: Vec<String>,
    ) -> std::io::Result<Self> {
        Self::create_with(path, format, Spec::new(columns))
    }

    pub fn create_with(
        path: &std::path::Path,
        format: Format,
        spec: Spec,
    ) -> std::io::Result<Self> {
        if format == Format::Xlsx {
            return Ok(Self {
                inner: Inner::Xlsx(Box::new(crate::export_xlsx::XlsxWriter::create(
                    path, spec,
                )?)),
            });
        }
        let mut out = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(path)?);
        if format == Format::CsvExcel {
            out.write_all(&[0xEF, 0xBB, 0xBF])?;
        }
        let mut w = TextWriter {
            out,
            format,
            spec,
            rows: 0,
            in_batch: 0,
        };
        w.head()?;
        Ok(Self {
            inner: Inner::Text(w),
        })
    }

    pub fn row(&mut self, row: &Row) -> std::io::Result<()> {
        match &mut self.inner {
            Inner::Text(w) => w.row(row),
            Inner::Xlsx(w) => w.row(row),
        }
    }

    pub fn finish(self) -> std::io::Result<()> {
        match self.inner {
            Inner::Text(w) => w.finish(),
            Inner::Xlsx(w) => w.finish(),
        }
    }
}

impl TextWriter {
    fn head(&mut self) -> std::io::Result<()> {
        let cols = &self.spec.columns;
        match self.format {
            Format::Csv | Format::CsvExcel if self.spec.header => {
                let head: Vec<String> = cols.iter().map(|c| csv_field(&Some(c.clone()))).collect();
                self.out.write_all(head.join(",").as_bytes())?;
                self.out.write_all(b"\r\n")
            }
            Format::Tsv if self.spec.header => {
                let head: Vec<String> = cols.iter().map(|c| tsv_field(&Some(c.clone()))).collect();
                self.out.write_all(head.join("\t").as_bytes())?;
                self.out.write_all(b"\r\n")
            }
            Format::Json => self.out.write_all(b"["),
            Format::Markdown => {
                let head = cols
                    .iter()
                    .map(|c| md_cell(&Some(c.clone())))
                    .collect::<Vec<_>>()
                    .join(" | ");
                writeln!(self.out, "| {head} |")?;
                writeln!(
                    self.out,
                    "|{}|",
                    cols.iter().map(|_| "---").collect::<Vec<_>>().join("|")
                )
            }
            Format::Html => {
                self.out.write_all(b"<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>Result</title>\n<style>table{border-collapse:collapse;font-family:sans-serif;font-size:13px}th,td{border:1px solid #ccc;padding:3px 8px;text-align:left}th{background:#f2f2f2}td.null{color:#999;font-style:italic}</style></head>\n<body>\n<table>\n<thead><tr>")?;
                for c in cols {
                    write!(self.out, "<th>{}</th>", html_escape(c))?;
                }
                self.out.write_all(b"</tr></thead>\n<tbody>\n")
            }
            _ => Ok(()),
        }
    }

    fn row(&mut self, row: &Row) -> std::io::Result<()> {
        self.rows += 1;
        match self.format {
            Format::Csv | Format::CsvExcel => {
                let f: Vec<String> = row.iter().map(csv_field).collect();
                self.out.write_all(f.join(",").as_bytes())?;
                self.out.write_all(b"\r\n")
            }
            Format::Tsv => {
                let f: Vec<String> = row.iter().map(tsv_field).collect();
                self.out.write_all(f.join("\t").as_bytes())?;
                self.out.write_all(b"\r\n")
            }
            Format::Json => {
                self.out
                    .write_all(if self.rows == 1 { b"\n" } else { b",\n" })?;
                json_object(&mut self.out, &self.spec, row)
            }
            Format::JsonLines => {
                json_object(&mut self.out, &self.spec, row)?;
                self.out.write_all(b"\n")
            }
            Format::Markdown => writeln!(
                self.out,
                "| {} |",
                row.iter().map(md_cell).collect::<Vec<_>>().join(" | ")
            ),
            Format::Html => {
                self.out.write_all(b"<tr>")?;
                for v in row {
                    match v {
                        None => self.out.write_all(b"<td class=\"null\"></td>")?,
                        Some(s) => write!(self.out, "<td>{}</td>", html_escape(s))?,
                    }
                }
                self.out.write_all(b"</tr>\n")
            }
            Format::Sql => {
                let batch = if self.spec.dialect == Dialect::Oracle {
                    1
                } else {
                    self.spec.batch.max(1)
                };
                if self.in_batch == 0 {
                    let cols = self
                        .spec
                        .columns
                        .iter()
                        .map(|c| quote_name(self.spec.dialect, c))
                        .collect::<Vec<_>>()
                        .join(", ");
                    writeln!(
                        self.out,
                        "INSERT INTO {} ({cols}) VALUES",
                        quote_name(self.spec.dialect, &self.spec.table)
                    )?;
                } else {
                    self.out.write_all(b",\n")?;
                }
                let vals = row
                    .iter()
                    .enumerate()
                    .map(|(c, v)| sql_value(v, self.spec.kind(c), self.spec.dialect))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(self.out, "({vals})")?;
                self.in_batch += 1;
                if self.in_batch >= batch {
                    self.in_batch = 0;
                    self.out.write_all(b";\n")?;
                }
                Ok(())
            }
            Format::Xlsx => unreachable!("xlsx has its own writer"),
        }
    }

    fn finish(mut self) -> std::io::Result<()> {
        match self.format {
            Format::Json => self
                .out
                .write_all(if self.rows == 0 { b"]\n" } else { b"\n]\n" })?,
            Format::Html => self
                .out
                .write_all(b"</tbody>\n</table>\n</body></html>\n")?,
            Format::Sql if self.in_batch > 0 => self.out.write_all(b";\n")?,
            _ => {}
        }
        self.out.flush()
    }
}

/// Starts the writer thread. Send chunks; drop the sender to finish. The
/// receiver of the result says how it ended.
pub fn spawn(
    path: std::path::PathBuf,
    format: Format,
    columns: Vec<String>,
) -> (SyncSender<Vec<Row>>, Receiver<std::io::Result<u64>>) {
    spawn_with(path, format, Spec::new(columns))
}

pub fn spawn_with(
    path: std::path::PathBuf,
    format: Format,
    spec: Spec,
) -> (SyncSender<Vec<Row>>, Receiver<std::io::Result<u64>>) {
    let (tx, rx) = sync_channel::<Vec<Row>>(4);
    let (done_tx, done_rx) = sync_channel(1);
    std::thread::Builder::new()
        .name("export".into())
        .spawn(move || {
            let res = (|| {
                let mut w = Writer::create_with(&path, format, spec)?;
                let mut n = 0u64;
                for chunk in rx {
                    for r in &chunk {
                        w.row(r)?;
                    }
                    n += chunk.len() as u64;
                }
                w.finish()?;
                Ok(n)
            })();
            let _ = done_tx.send(res);
        })
        .expect("export thread");
    (tx, done_rx)
}

/// Number of rows an export writes: the visible rows (`view` = source row
/// indices in display order, after filter and sort) or all `total` rows.
pub fn visible_len(total: usize, view: Option<&[u32]>) -> usize {
    view.map(|v| v.len()).unwrap_or(total)
}

/// Display rows `start..end` as export rows. `cell(src, col)` reads a source
/// cell; `view` maps display position to source row (`None` = identity).
pub fn read_chunk(
    view: Option<&[u32]>,
    start: usize,
    end: usize,
    ncols: usize,
    cell: &dyn Fn(usize, usize) -> Option<String>,
) -> Vec<Row> {
    // A position past the end of `view` is skipped, never a panic.
    (start..end)
        .filter_map(|r| {
            let src = match view {
                Some(v) => *v.get(r)? as usize,
                None => r,
            };
            Some((0..ncols).map(|c| cell(src, c)).collect())
        })
        .collect()
}

#[cfg(test)]
/// Writes the visible rows to `path` in one go (the app streams chunks
/// instead). Returns the number of rows written.
pub fn write_file(
    path: &std::path::Path,
    format: Format,
    columns: Vec<String>,
    total: usize,
    view: Option<&[u32]>,
    cell: &dyn Fn(usize, usize) -> Option<String>,
) -> std::io::Result<u64> {
    let ncols = columns.len();
    let mut w = Writer::create(path, format, columns)?;
    let n = visible_len(total, view);
    let mut start = 0;
    while start < n {
        let end = (start + 2000).min(n);
        for r in read_chunk(view, start, end, ncols, cell) {
            w.row(&r)?;
        }
        start = end;
    }
    w.finish()?;
    Ok(n as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Vec<Row> {
        vec![
            vec![Some("1".into()), Some("a,b".into())],
            vec![Some("2".into()), None],
            vec![Some("3".into()), Some("line\nbreak \"q\"".into())],
            vec![Some("4".into()), Some("中文".into())],
            vec![Some("5".into()), Some("0xDEADBEEF".into())],
        ]
    }

    fn run(format: Format, view: Option<&[u32]>) -> (u64, Vec<u8>) {
        let d = data();
        let p = std::env::temp_dir().join(format!(
            "wb-export-view-{:?}-{}.out",
            format,
            view.map(|v| v.len() as i64).unwrap_or(-1)
        ));
        let n = write_file(
            &p,
            format,
            vec!["id".into(), "v".into()],
            d.len(),
            view,
            &|r, c| d[r][c].clone(),
        )
        .unwrap();
        let b = std::fs::read(&p).unwrap();
        let _ = std::fs::remove_file(p);
        (n, b)
    }

    #[test]
    fn csv_writes_only_visible_rows_in_view_order() {
        let (n, b) = run(Format::Csv, Some(&[3, 1, 0]));
        assert_eq!(n, 3);
        assert_eq!(
            String::from_utf8(b).unwrap(),
            "id,v\r\n4,中文\r\n2,\r\n1,\"a,b\"\r\n"
        );
    }

    #[test]
    fn no_view_writes_everything() {
        let (n, b) = run(Format::Csv, None);
        assert_eq!(n, 5);
        assert_eq!(String::from_utf8(b).unwrap().lines().count(), 1 + 5 + 1); // embedded newline adds a line
    }

    #[test]
    fn empty_view_writes_header_only() {
        let (n, b) = run(Format::Csv, Some(&[]));
        assert_eq!(n, 0);
        assert_eq!(b, b"id,v\r\n");
    }

    #[test]
    fn json_visible_rows_null_and_quotes() {
        let (n, b) = run(Format::Json, Some(&[2, 1]));
        assert_eq!(n, 2);
        let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert_eq!(v[0]["v"], "line\nbreak \"q\"");
        assert_eq!(v[1]["v"], serde_json::Value::Null);
    }

    #[test]
    fn csv_field_quoting_cases() {
        assert_eq!(csv_field(&Some("中文,x".into())), "\"中文,x\"");
        assert_eq!(csv_field(&Some("a\r\nb".into())), "\"a\r\nb\"");
        assert_eq!(csv_field(&Some(String::new())), "");
        assert_eq!(csv_field(&Some("0xDEADBEEF".into())), "0xDEADBEEF");
    }

    #[test]
    fn out_of_range_view_position_is_skipped() {
        let view = [2u32, 0];
        let c = read_chunk(Some(&view), 0, 5, 1, &|r, _| Some(r.to_string()));
        assert_eq!(c, vec![vec![Some("2".into())], vec![Some("0".into())]]);
    }

    #[test]
    fn large_view_is_not_copied_into_rows() {
        let total = 100_000;
        let view: Vec<u32> = (0..total as u32).rev().step_by(3).collect();
        let c = read_chunk(Some(&view), 0, 2, 1, &|r, _| Some(r.to_string()));
        assert_eq!(
            c,
            vec![vec![Some("99999".into())], vec![Some("99996".into())]]
        );
        assert_eq!(visible_len(total, Some(&view)), view.len());
    }

    #[test]
    fn rfc4180_and_json() {
        assert_eq!(csv_field(&Some("a,b".into())), "\"a,b\"");
        assert_eq!(csv_field(&Some("say \"hi\"".into())), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field(&Some("line\nbreak".into())), "\"line\nbreak\"");
        assert_eq!(csv_field(&None), "");
        let dir = std::env::temp_dir();
        let p = dir.join("wb-export-test.json");
        let mut w = Writer::create(&p, Format::Json, vec!["a".into(), "名".into()]).unwrap();
        w.row(&vec![Some("1".into()), None]).unwrap();
        w.row(&vec![Some("x\"y".into()), Some("中文".into())])
            .unwrap();
        w.finish().unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(v[0]["名"], serde_json::Value::Null);
        assert_eq!(v[1]["名"], "中文");
        let p2 = dir.join("wb-export-test.csv");
        let mut w = Writer::create(&p2, Format::CsvExcel, vec!["a".into()]).unwrap();
        w.row(&vec![Some("x,y".into())]).unwrap();
        w.finish().unwrap();
        assert_eq!(std::fs::read(&p2).unwrap(), b"\xEF\xBB\xBFa\r\n\"x,y\"\r\n");
        let _ = (std::fs::remove_file(p), std::fs::remove_file(p2));
    }
}
