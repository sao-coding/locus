//! Reading the import file: CSV / TSV streamed through a transcoder (a 1 GB
//! file never sits in memory), spreadsheets through calamine (the chosen
//! sheet is loaded; Excel itself stops at 1,048,576 rows).

use calamine::{Data, Reader};
use encoding_rs::Encoding;
use encoding_rs_io::DecodeReaderBytesBuilder;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CsvOptions {
    pub delimiter: u8,
    /// `0` = no quote character.
    pub quote: u8,
    pub has_header: bool,
    /// An `encoding_rs` label: `UTF-8`, `Big5`, `UTF-16LE`, `GBK` …
    pub encoding: String,
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            delimiter: b',',
            quote: b'"',
            has_header: true,
            encoding: "UTF-8".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SourceSpec {
    pub path: PathBuf,
    /// Spreadsheet sheet name (first sheet when `None`).
    pub sheet: Option<String>,
    pub csv: CsvOptions,
}

impl SourceSpec {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            sheet: None,
            csv: CsvOptions::default(),
        }
    }
}

pub fn is_spreadsheet(path: &Path) -> bool {
    matches!(ext(path).as_str(), "xlsx" | "xlsm" | "xls" | "xlsb" | "ods")
}

/// A file the wizard accepts (drag and drop, file picker).
pub fn is_importable(path: &Path) -> bool {
    is_spreadsheet(path) || matches!(ext(path).as_str(), "csv" | "tsv" | "txt")
}

fn ext(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// What the sniffer found in the first bytes of a text file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    pub encoding: String,
    pub delimiter: u8,
}

/// Encoding names offered in the wizard.
pub const ENCODINGS: &[&str] = &[
    "UTF-8",
    "Big5",
    "GBK",
    "GB18030",
    "UTF-16LE",
    "UTF-16BE",
    "Shift_JIS",
    "EUC-KR",
    "windows-1252",
];

pub fn detect(path: &Path) -> std::io::Result<Detected> {
    let mut buf = Vec::new();
    File::open(path)?.take(256 * 1024).read_to_end(&mut buf)?;
    Ok(detect_bytes(&buf, ext(path) == "tsv"))
}

pub fn detect_bytes(buf: &[u8], tsv: bool) -> Detected {
    let encoding = sniff_encoding(buf);
    let text = decode_lossy(buf, &encoding);
    Detected {
        delimiter: if tsv { b'\t' } else { sniff_delimiter(&text) },
        encoding,
    }
}

fn decode_lossy(buf: &[u8], encoding: &str) -> String {
    let enc = Encoding::for_label(encoding.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    enc.decode(buf).0.into_owned()
}

fn sniff_encoding(buf: &[u8]) -> String {
    if buf.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return "UTF-8".into();
    }
    if buf.starts_with(&[0xFF, 0xFE]) {
        return "UTF-16LE".into();
    }
    if buf.starts_with(&[0xFE, 0xFF]) {
        return "UTF-16BE".into();
    }
    // UTF-16 without BOM: ASCII text has a NUL in every second byte.
    if buf.len() >= 8 {
        let n = buf.len().min(4096) / 2;
        let even = (0..n).filter(|i| buf[i * 2] == 0).count();
        let odd = (0..n).filter(|i| buf[i * 2 + 1] == 0).count();
        if odd * 10 > n * 7 && even == 0 {
            return "UTF-16LE".into();
        }
        if even * 10 > n * 7 && odd == 0 {
            return "UTF-16BE".into();
        }
    }
    match std::str::from_utf8(buf) {
        Ok(_) => return "UTF-8".into(),
        // A sample cut in the middle of a character is still UTF-8.
        Err(e) if e.error_len().is_none() => return "UTF-8".into(),
        Err(_) => {}
    }
    let mut d = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    d.feed(buf, true);
    d.guess(None, chardetng::Utf8Detection::Deny)
        .name()
        .to_string()
}

fn sniff_delimiter(text: &str) -> u8 {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(20)
        .collect();
    let mut best = (b',', 0usize);
    for d in *b",\t;|" {
        let counts: Vec<usize> = lines
            .iter()
            .map(|l| l.bytes().filter(|&b| b == d).count())
            .collect();
        let Some(&first) = counts.first() else {
            continue;
        };
        if first == 0 {
            continue;
        }
        // Consistency first (how many lines share the first line's count), then the count.
        let same = counts.iter().filter(|&&c| c == first).count();
        let score = same * 1000 + first.min(999);
        if score > best.1 {
            best = (d, score);
        }
    }
    best.0
}

/// Sheet names of a spreadsheet.
pub fn sheet_names(path: &Path) -> Result<Vec<String>, String> {
    let wb = calamine::open_workbook_auto(path).map_err(|e| e.to_string())?;
    Ok(wb.sheet_names())
}

/// One row at a time; `line` is the 1-based line of the file (CSV) or row of
/// the sheet.
pub trait RowSource: Send {
    /// Fills `row` and returns its line, `None` at the end.
    fn next_row(&mut self, row: &mut Vec<String>) -> Result<Option<u64>, String>;
    /// 0.0 – 1.0 of the file read so far.
    fn fraction(&self) -> f64;
}

struct Counting<R> {
    inner: R,
    n: Arc<AtomicU64>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let k = self.inner.read(buf)?;
        self.n.fetch_add(k as u64, Ordering::Relaxed);
        Ok(k)
    }
}

struct CsvRows {
    rdr: csv::Reader<Box<dyn Read + Send>>,
    rec: csv::StringRecord,
    read: Arc<AtomicU64>,
    size: u64,
    /// The file ends its lines with LF (or CRLF): the reader splits on LF only and the CR is
    /// dropped here, because the csv crate counts lines of CRLF files one short.
    strip_cr: bool,
}

impl RowSource for CsvRows {
    fn next_row(&mut self, row: &mut Vec<String>) -> Result<Option<u64>, String> {
        loop {
            match self.rdr.read_record(&mut self.rec) {
                Ok(false) => return Ok(None),
                Ok(true) => {
                    // A blank line is not a row.
                    if self.rec.len() == 1 && self.rec.get(0).is_some_and(|s| s.is_empty()) {
                        continue;
                    }
                    row.clear();
                    row.extend(self.rec.iter().map(str::to_string));
                    if self.strip_cr
                        && let Some(last) = row.last_mut()
                        && last.ends_with('\r')
                    {
                        last.pop();
                    }
                    return Ok(Some(self.rec.position().map(|p| p.line()).unwrap_or(0)));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    fn fraction(&self) -> f64 {
        if self.size == 0 {
            1.0
        } else {
            (self.read.load(Ordering::Relaxed) as f64 / self.size as f64).min(1.0)
        }
    }
}

struct SheetRows {
    rows: std::vec::IntoIter<Vec<String>>,
    first_line: u64,
    next: u64,
    total: usize,
    done: usize,
}

impl RowSource for SheetRows {
    fn next_row(&mut self, row: &mut Vec<String>) -> Result<Option<u64>, String> {
        let Some(r) = self.rows.next() else {
            return Ok(None);
        };
        *row = r;
        let line = self.first_line + self.next;
        self.next += 1;
        self.done += 1;
        Ok(Some(line))
    }

    fn fraction(&self) -> f64 {
        if self.total == 0 {
            1.0
        } else {
            self.done as f64 / self.total as f64
        }
    }
}

pub fn cell_text(d: &Data) -> String {
    match d {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{}", *f as i64)
            } else {
                format!("{f}")
            }
        }
        Data::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Data::DateTime(dt) => {
            if dt.is_datetime() {
                let (y, mo, d, h, mi, s, _) = dt.to_ymd_hms_milli();
                if h == 0 && mi == 0 && s == 0 {
                    format!("{y:04}-{mo:02}-{d:02}")
                } else {
                    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
                }
            } else {
                dt.as_f64().to_string()
            }
        }
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("{e:?}"),
    }
}

pub struct Opened {
    /// Header names (`None` when the file has no header row).
    pub header: Option<Vec<String>>,
    pub rows: Box<dyn RowSource>,
}

pub fn open(spec: &SourceSpec) -> Result<Opened, String> {
    let mut src: Box<dyn RowSource> = if is_spreadsheet(&spec.path) {
        open_sheet(spec)?
    } else {
        open_csv(spec)?
    };
    let header = if spec.csv.has_header {
        let mut h = Vec::new();
        match src.next_row(&mut h)? {
            Some(_) => Some(h.iter().map(|s| s.trim().to_string()).collect()),
            None => Some(Vec::new()),
        }
    } else {
        None
    };
    Ok(Opened { header, rows: src })
}

fn open_csv(spec: &SourceSpec) -> Result<Box<dyn RowSource>, String> {
    let f = File::open(&spec.path).map_err(|e| format!("{}: {e}", spec.path.display()))?;
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let read = Arc::new(AtomicU64::new(0));
    let enc = Encoding::for_label(spec.csv.encoding.as_bytes())
        .ok_or_else(|| format!("unknown encoding {}", spec.csv.encoding))?;
    let dec = DecodeReaderBytesBuilder::new()
        .encoding(Some(enc))
        .bom_override(true)
        .strip_bom(true)
        .build(Counting {
            inner: f,
            n: read.clone(),
        });
    // Line ending: sniffed from the first 64 KB (raw bytes, so also UTF-16). A file with no LF at all
    // (old Mac CR) keeps the crate's own terminator handling.
    let mut head = Vec::new();
    let _ = File::open(&spec.path).and_then(|g| g.take(65_536).read_to_end(&mut head));
    let strip_cr = head.contains(&b'\n');
    let mut b = csv::ReaderBuilder::new();
    b.has_headers(false)
        .flexible(true)
        .delimiter(spec.csv.delimiter);
    if strip_cr {
        b.terminator(csv::Terminator::Any(b'\n'));
    }
    if spec.csv.quote == 0 {
        b.quoting(false);
    } else {
        b.quote(spec.csv.quote);
    }
    let rdr = b.from_reader(Box::new(dec) as Box<dyn Read + Send>);
    Ok(Box::new(CsvRows {
        rdr,
        rec: csv::StringRecord::new(),
        read,
        size,
        strip_cr,
    }))
}

fn open_sheet(spec: &SourceSpec) -> Result<Box<dyn RowSource>, String> {
    let mut wb = calamine::open_workbook_auto(&spec.path).map_err(|e| e.to_string())?;
    let name = match &spec.sheet {
        Some(s) => s.clone(),
        None => wb
            .sheet_names()
            .first()
            .cloned()
            .ok_or("the workbook has no sheet")?,
    };
    let range = wb.worksheet_range(&name).map_err(|e| e.to_string())?;
    let first_line = range.start().map(|(r, _)| r as u64 + 1).unwrap_or(1);
    let rows: Vec<Vec<String>> = range
        .rows()
        .map(|r| r.iter().map(cell_text).collect())
        .collect();
    let total = rows.len();
    Ok(Box::new(SheetRows {
        rows: rows.into_iter(),
        first_line,
        next: 0,
        total,
        done: 0,
    }))
}

/// The first rows for the wizard's preview, and the number of columns.
pub struct Preview {
    pub header: Option<Vec<String>>,
    pub rows: Vec<Vec<String>>,
    pub ncols: usize,
}

pub fn preview(spec: &SourceSpec, max_rows: usize) -> Result<Preview, String> {
    let mut o = open(spec)?;
    let mut rows = Vec::new();
    let mut row = Vec::new();
    while rows.len() < max_rows {
        if o.rows.next_row(&mut row)?.is_none() {
            break;
        }
        rows.push(row.clone());
    }
    let ncols = rows
        .iter()
        .map(Vec::len)
        .chain(o.header.iter().map(Vec::len))
        .max()
        .unwrap_or(0);
    Ok(Preview {
        header: o.header,
        rows,
        ncols,
    })
}

/// Column names for the mapping: the header, or `col1…`; empty and repeated
/// names get a number.
pub fn column_names(header: &Option<Vec<String>>, ncols: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(ncols);
    for i in 0..ncols {
        let mut n = header
            .as_ref()
            .and_then(|h| h.get(i))
            .cloned()
            .unwrap_or_default();
        if n.is_empty() {
            n = format!("col{}", i + 1);
        }
        let base = n.clone();
        let mut k = 2;
        while out.iter().any(|o| o.eq_ignore_ascii_case(&n)) {
            n = format!("{base}_{k}");
            k += 1;
        }
        out.push(n);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_encodings() {
        assert_eq!(sniff_encoding(b"\xEF\xBB\xBFa,b\n"), "UTF-8");
        assert_eq!(sniff_encoding(b"\xFF\xFEa\0,\0b\0"), "UTF-16LE");
        assert_eq!(sniff_encoding("姓名,年齡\n王小明,20\n".as_bytes()), "UTF-8");
        let (big5, _, _) = encoding_rs::BIG5
            .encode("姓名,年齡,地址\n王小明,20,台北市中正區\n李大華,31,新北市板橋區\n");
        assert_eq!(sniff_encoding(&big5), "Big5");
        // UTF-16 without BOM, built by hand (encoding_rs does not encode UTF-16).
        let raw: Vec<u8> = "id,name\n1,abc\n"
            .encode_utf16()
            .flat_map(|c| c.to_le_bytes())
            .collect();
        assert_eq!(sniff_encoding(&raw), "UTF-16LE");
    }

    #[test]
    fn detects_delimiters() {
        assert_eq!(sniff_delimiter("a,b,c\n1,2,3\n"), b',');
        assert_eq!(sniff_delimiter("a\tb\tc\n1\t2\t3\n"), b'\t');
        assert_eq!(sniff_delimiter("a;b;c\n1;2;3\n"), b';');
        assert_eq!(sniff_delimiter("a|b\n1|2\n"), b'|');
        assert_eq!(sniff_delimiter("one column\nnext\n"), b',');
    }

    #[test]
    fn names() {
        let h = Some(vec!["a".into(), "".into(), "A".into()]);
        assert_eq!(column_names(&h, 4), vec!["a", "col2", "A_2", "col4"]);
        assert_eq!(column_names(&None, 2), vec!["col1", "col2"]);
    }

    #[test]
    fn cells() {
        assert_eq!(cell_text(&Data::Float(3.0)), "3");
        assert_eq!(cell_text(&Data::Float(3.25)), "3.25");
        assert_eq!(cell_text(&Data::Bool(true)), "true");
        assert_eq!(cell_text(&Data::Empty), "");
    }

    /// Lines are the lines of the file, also for CRLF files (Excel) and fields with line breaks.
    #[test]
    fn csv_lines_are_file_lines() {
        for (name, text) in [
            ("lf", "id,v\n1,a\n2,b\n3,c\n"),
            ("crlf", "id,v\r\n1,a\r\n2,b\r\n3,c\r\n"),
            ("quoted-break", "id,v\r\n1,\"a\r\nb\"\r\n2,b\r\n3,c\r\n"),
        ] {
            let p =
                std::env::temp_dir().join(format!("wb-lines-{name}-{}.csv", std::process::id()));
            std::fs::write(&p, text).unwrap();
            let mut o = open(&SourceSpec::new(&p)).unwrap();
            let mut row = Vec::new();
            let mut lines = Vec::new();
            while let Some(l) = o.rows.next_row(&mut row).unwrap() {
                lines.push(l);
            }
            let _ = std::fs::remove_file(&p);
            let want: Vec<u64> = if name == "quoted-break" {
                vec![2, 4, 5]
            } else {
                vec![2, 3, 4]
            };
            assert_eq!(lines, want, "{name}");
        }
    }
}
