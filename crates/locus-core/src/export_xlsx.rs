//! Excel .xlsx writer on `rust_xlsxwriter` in constant-memory mode (rows go
//! to a temp file as they are written, so 10M rows do not sit in memory).
//!
//! Header row bold and frozen, column widths from the header and the first
//! rows, numbers as numbers, dates as dates, NULL as an empty cell. When a
//! sheet is full (`Spec::sheet_rows`, Excel's limit by default) the rows go
//! on in the next sheet with the same header.

use crate::copyfmt;
use crate::export::{ColKind, Row, Spec, date_parts};
use rust_xlsxwriter::{ExcelDateTime, Format, Workbook, XlsxError};

/// Rows held back until the column widths are known.
const SIZING_ROWS: usize = 200;
/// Longest text Excel keeps in one cell.
const CELL_MAX_CHARS: usize = 32_767;
/// Widest column the sizing produces, in characters.
const WIDTH_MAX: f64 = 60.0;

pub struct XlsxWriter {
    wb: Workbook,
    path: std::path::PathBuf,
    spec: Spec,
    /// Index of the sheet being filled.
    sheet: usize,
    /// Rows on the current sheet, header included.
    on_sheet: u32,
    widths: Vec<f64>,
    sized: bool,
    pending: Vec<Row>,
    bold: Format,
    date: Format,
    datetime: Format,
}

fn err(e: XlsxError) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// Text Excel accepts: no control characters XML cannot hold, at most 32,767 characters.
fn clean(s: &str) -> std::borrow::Cow<'_, str> {
    let bad = |c: char| (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r');
    if !s.chars().take(CELL_MAX_CHARS + 1).any(bad) && s.chars().nth(CELL_MAX_CHARS).is_none() {
        return s.into();
    }
    s.chars()
        .filter(|c| !bad(*c))
        .take(CELL_MAX_CHARS)
        .collect::<String>()
        .into()
}

/// A number Excel keeps exactly: plain decimal text of at most 15 digits.
fn exact_number(s: &str) -> Option<f64> {
    if !copyfmt::is_json_number(s) {
        return None;
    }
    let mantissa = s.split(['e', 'E']).next().unwrap_or(s);
    if mantissa.bytes().filter(u8::is_ascii_digit).count() > 15 {
        return None;
    }
    s.parse().ok()
}

/// Display width of a text in characters, CJK counted double.
fn text_width(s: &str) -> f64 {
    s.lines()
        .map(|l| {
            l.chars()
                .map(|c| if (c as u32) > 0x2E80 { 2.0 } else { 1.0 })
                .sum::<f64>()
        })
        .fold(0.0, f64::max)
}

fn excel_date(s: &str) -> Option<(ExcelDateTime, bool)> {
    let (_, has_time) = date_parts(s)?;
    // Excel keeps milliseconds: cut the fraction to three digits.
    let mut t = s.replacen(' ', "T", 1);
    if let Some(dot) = t.find('.') {
        t.truncate((dot + 4).min(t.len()));
    }
    ExcelDateTime::parse_from_str(&t)
        .ok()
        .map(|d| (d, has_time))
}

impl XlsxWriter {
    pub fn create(path: &std::path::Path, spec: Spec) -> std::io::Result<Self> {
        let mut wb = Workbook::new();
        wb.add_worksheet_with_constant_memory()
            .map_err(err)?
            .set_name("Result")
            .map_err(err)?;
        let widths = spec.columns.iter().map(|c| text_width(c) + 2.0).collect();
        // Fail early on an unwritable destination instead of after the last row.
        std::fs::File::create(path)?;
        Ok(Self {
            wb,
            path: path.to_path_buf(),
            spec,
            sheet: 0,
            on_sheet: 0,
            widths,
            sized: false,
            pending: Vec::new(),
            bold: Format::new().set_bold(),
            date: Format::new().set_num_format("yyyy-mm-dd"),
            datetime: Format::new().set_num_format("yyyy-mm-dd hh:mm:ss"),
        })
    }

    pub fn row(&mut self, row: &Row) -> std::io::Result<()> {
        if !self.sized {
            for (c, v) in row.iter().enumerate() {
                if let (Some(s), Some(w)) = (v, self.widths.get_mut(c)) {
                    *w = w.max(text_width(&s[..s.len().min(200)]) + 2.0);
                }
            }
            self.pending.push(row.clone());
            if self.pending.len() >= SIZING_ROWS {
                self.start_sheet()?;
            }
            return Ok(());
        }
        self.put(row)
    }

    /// Widths known: write header, freeze it and the rows held back.
    fn start_sheet(&mut self) -> std::io::Result<()> {
        self.sized = true;
        self.begin_sheet()?;
        for r in std::mem::take(&mut self.pending) {
            self.put(&r)?;
        }
        Ok(())
    }

    fn begin_sheet(&mut self) -> std::io::Result<()> {
        let ws = self.wb.worksheet_from_index(self.sheet).map_err(err)?;
        ws.set_freeze_panes(1, 0).map_err(err)?;
        for (c, w) in self.widths.iter().enumerate() {
            ws.set_column_width(c as u16, w.min(WIDTH_MAX))
                .map_err(err)?;
        }
        for (c, name) in self.spec.columns.iter().enumerate() {
            ws.write_string_with_format(0, c as u16, clean(name).as_ref(), &self.bold)
                .map_err(err)?;
        }
        self.on_sheet = 1;
        Ok(())
    }

    fn put(&mut self, row: &Row) -> std::io::Result<()> {
        if self.on_sheet >= self.spec.sheet_rows.max(2) {
            self.sheet += 1;
            self.wb
                .add_worksheet_with_constant_memory()
                .map_err(err)?
                .set_name(format!("Result {}", self.sheet + 1))
                .map_err(err)?;
            self.begin_sheet()?;
        }
        let r = self.on_sheet;
        let ws = self.wb.worksheet_from_index(self.sheet).map_err(err)?;
        for (c, v) in row.iter().enumerate() {
            let Some(s) = v else { continue };
            let col = c as u16;
            match self.spec.kinds.get(c).copied().unwrap_or_default() {
                ColKind::Number => match exact_number(s) {
                    Some(n) => ws.write_number(r, col, n),
                    None => ws.write_string(r, col, clean(s).as_ref()),
                },
                ColKind::Date => match excel_date(s) {
                    Some((d, true)) => ws.write_datetime_with_format(r, col, &d, &self.datetime),
                    Some((d, false)) => ws.write_datetime_with_format(r, col, &d, &self.date),
                    None => ws.write_string(r, col, clean(s).as_ref()),
                },
                _ => ws.write_string(r, col, clean(s).as_ref()),
            }
            .map_err(err)?;
        }
        self.on_sheet += 1;
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<()> {
        if !self.sized {
            self.start_sheet()?;
        }
        self.wb.save(&self.path).map_err(err)
    }
}
