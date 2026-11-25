//! Compact text for tool results: a TSV table with the header once, cells cut
//! at [`CELL_MAX`] chars, and a footer with the row count. Never JSON per row:
//! a client pays for every token it reads.

use db_core::Cell;

/// Rows a result shows unless the client asks for another number.
pub const DEFAULT_ROWS: usize = 200;
/// Most rows a client can ask for.
pub const MAX_ROWS: usize = 5000;
/// Chars of one cell before it is cut.
pub const CELL_MAX: usize = 200;
/// Rows counted past the shown ones before counting stops (the footer then says `N+`).
pub const COUNT_CAP: u64 = 100_000;

/// The row cap a call asked for, clamped to `1..=MAX_ROWS`.
pub fn row_cap(asked: Option<u64>) -> usize {
    match asked {
        None => DEFAULT_ROWS,
        Some(n) => (n as usize).clamp(1, MAX_ROWS),
    }
}

/// `1234567` → `1,234,567`.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// One cell as TSV text: tabs and line breaks escaped, NULL as `NULL`,
/// bytes as `0x…` hex, cut at [`CELL_MAX`] chars with `…[+N]` (N chars more).
pub fn cell_text(c: Cell<'_>) -> String {
    match c {
        Cell::Null => "NULL".into(),
        Cell::Text(s) => cut(&escape(s)),
        Cell::Bytes(b) => {
            // Never hex more than needed for the cut.
            let take = b.len().min(CELL_MAX / 2 + 1);
            let mut s = String::with_capacity(2 + take * 2);
            s.push_str("0x");
            for x in &b[..take] {
                s.push_str(&format!("{x:02x}"));
            }
            let full = 2 + b.len() * 2;
            if full > CELL_MAX {
                let shown: String = s.chars().take(CELL_MAX).collect();
                format!("{shown}…[+{}]", full - CELL_MAX)
            } else {
                s
            }
        }
    }
}

/// Plain text cut to [`CELL_MAX`] chars (already escaped).
pub fn cut(s: &str) -> String {
    let n = s.chars().count();
    if n <= CELL_MAX {
        return s.to_string();
    }
    let shown: String = s.chars().take(CELL_MAX).collect();
    format!("{shown}…[+{}]", n - CELL_MAX)
}

fn escape(s: &str) -> String {
    if !s.contains(['\t', '\n', '\r', '\\']) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out
}

/// One result set as it is read: the header, the first `cap` rows as text,
/// and how many rows came in all.
#[derive(Debug, Default)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub cap: usize,
    /// Rows seen (shown or not).
    pub total: u64,
    /// Counting stopped at [`COUNT_CAP`] past the shown rows: `total` is a lower bound.
    pub more: bool,
}

impl Table {
    pub fn new(columns: Vec<String>, cap: usize) -> Self {
        Table {
            columns,
            cap,
            ..Default::default()
        }
    }

    /// Whether the reader can stop: the shown rows are full and counting reached its cap.
    pub fn done(&self) -> bool {
        self.rows.len() >= self.cap && self.total >= self.cap as u64 + COUNT_CAP
    }

    /// Adds the rows of one batch; past the cap they are only counted.
    pub fn push_batch(&mut self, b: &db_core::Batch) {
        for r in 0..b.rows() {
            if self.rows.len() < self.cap {
                self.rows
                    .push((0..b.columns()).map(|c| cell_text(b.cell(r, c))).collect());
            }
            self.total += 1;
        }
        if self.total >= self.cap as u64 + COUNT_CAP {
            self.more = true;
        }
    }

    /// TSV with the header once and a footer:
    /// `(3 rows)`, or `(200 of 18,342 rows shown, truncated)`.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(
            &self
                .columns
                .iter()
                .map(|c| cut(&escape(c)))
                .collect::<Vec<_>>()
                .join("\t"),
        );
        out.push('\n');
        for r in &self.rows {
            out.push_str(&r.join("\t"));
            out.push('\n');
        }
        out.push_str(&self.footer());
        out
    }

    pub fn footer(&self) -> String {
        let shown = self.rows.len() as u64;
        if shown < self.total || self.more {
            let plus = if self.more { "+" } else { "" };
            format!(
                "({} of {}{plus} rows shown, truncated)",
                thousands(shown),
                thousands(self.total)
            )
        } else if self.total == 1 {
            "(1 row)".into()
        } else {
            format!("({} rows)", thousands(self.total))
        }
    }
}

/// `N rows affected`.
pub fn affected(n: u64) -> String {
    if n == 1 {
        "1 row affected".into()
    } else {
        format!("{} rows affected", thousands(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use db_core::BatchBuilder;

    fn batch(rows: &[&[Option<&str>]]) -> db_core::Batch {
        let mut b = BatchBuilder::new(rows.first().map_or(0, |r| r.len()));
        for r in rows {
            for c in *r {
                b.push(c.map(str::as_bytes));
            }
            b.end_row();
        }
        b.finish()
    }

    #[test]
    fn row_cap_defaults_and_clamps() {
        assert_eq!(row_cap(None), 200);
        assert_eq!(row_cap(Some(0)), 1);
        assert_eq!(row_cap(Some(10)), 10);
        assert_eq!(row_cap(Some(5000)), 5000);
        assert_eq!(row_cap(Some(1_000_000)), 5000);
    }

    #[test]
    fn thousands_separator() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(18342), "18,342");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn long_cells_are_cut_with_a_marker() {
        let s = "x".repeat(250);
        let c = cell_text(Cell::Text(&s));
        assert!(c.starts_with(&"x".repeat(200)));
        assert!(c.ends_with("…[+50]"), "{c}");
        assert_eq!(c.chars().count(), 200 + "…[+50]".chars().count());
        // Exactly the limit: untouched.
        let s = "é".repeat(200);
        assert_eq!(cell_text(Cell::Text(&s)), s);
        // Chars, not bytes.
        let s = "測".repeat(201);
        assert!(cell_text(Cell::Text(&s)).ends_with("…[+1]"));
    }

    #[test]
    fn cells_escape_tabs_newlines_and_show_null_and_bytes() {
        assert_eq!(cell_text(Cell::Null), "NULL");
        assert_eq!(cell_text(Cell::Text("a\tb\nc\\")), "a\\tb\\nc\\\\");
        assert_eq!(cell_text(Cell::Text("")), "");
        assert_eq!(cell_text(Cell::Bytes(&[0xde, 0xad])), "0xdead");
        let big = vec![0xabu8; 500];
        let c = cell_text(Cell::Bytes(&big));
        assert!(c.starts_with("0xabab") && c.ends_with("…[+802]"), "{c}");
    }

    #[test]
    fn table_is_tsv_with_header_once_and_footer() {
        let mut t = Table::new(vec!["id".into(), "name".into()], 200);
        t.push_batch(&batch(&[&[Some("1"), Some("ann")], &[Some("2"), None]]));
        assert_eq!(t.render(), "id\tname\n1\tann\n2\tNULL\n(2 rows)");
        let mut one = Table::new(vec!["n".into()], 200);
        one.push_batch(&batch(&[&[Some("7")]]));
        assert_eq!(one.render(), "n\n7\n(1 row)");
        let empty = Table::new(vec!["n".into()], 200);
        assert_eq!(empty.render(), "n\n(0 rows)");
    }

    #[test]
    fn row_cap_truncates_and_counts_the_rest() {
        let mut t = Table::new(vec!["n".into()], 2);
        let rows: Vec<Vec<Option<String>>> = (0..5).map(|i| vec![Some(i.to_string())]).collect();
        let refs: Vec<Vec<Option<&str>>> = rows
            .iter()
            .map(|r| r.iter().map(|c| c.as_deref()).collect())
            .collect();
        let slices: Vec<&[Option<&str>]> = refs.iter().map(|r| r.as_slice()).collect();
        t.push_batch(&batch(&slices));
        t.push_batch(&batch(&slices));
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.total, 10);
        assert_eq!(t.render(), "n\n0\n1\n(2 of 10 rows shown, truncated)");
        assert!(!t.done());
    }

    #[test]
    fn counting_stops_at_the_cap() {
        let mut t = Table::new(vec!["n".into()], 1);
        t.rows.push(vec!["0".into()]);
        t.total = 1 + COUNT_CAP;
        t.more = true;
        assert!(t.done());
        assert_eq!(t.footer(), "(1 of 100,001+ rows shown, truncated)");
    }

    #[test]
    fn affected_text() {
        assert_eq!(affected(1), "1 row affected");
        assert_eq!(affected(0), "0 rows affected");
        assert_eq!(affected(12000), "12,000 rows affected");
    }
}
