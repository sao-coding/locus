//! Columnar result storage. Each column of a batch is one byte buffer plus
//! end offsets and a null bitmap, so a 40,000-row result is a few dozen
//! allocations instead of one `String` per cell.

use crate::ColumnMeta;

/// One cell, borrowed from its batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell<'a> {
    Null,
    Text(&'a str),
    /// Value that is not valid UTF-8 (BLOB, binary).
    Bytes(&'a [u8]),
}

#[derive(Debug, Default)]
struct Column {
    data: Vec<u8>,
    /// `ends[r]` is the end offset of row `r` in `data`.
    ends: Vec<u32>,
    /// Bit set = NULL.
    nulls: Vec<u64>,
}

/// An immutable block of rows.
#[derive(Debug)]
pub struct Batch {
    rows: usize,
    cols: Vec<Column>,
}

impl Batch {
    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn columns(&self) -> usize {
        self.cols.len()
    }

    /// Bytes held by the cell data and indexes.
    pub fn bytes(&self) -> usize {
        self.cols
            .iter()
            .map(|c| c.data.capacity() + c.ends.capacity() * 4 + c.nulls.capacity() * 8)
            .sum()
    }

    pub fn cell(&self, row: usize, col: usize) -> Cell<'_> {
        let c = &self.cols[col];
        if c.nulls[row / 64] >> (row % 64) & 1 == 1 {
            return Cell::Null;
        }
        let start = if row == 0 {
            0
        } else {
            c.ends[row - 1] as usize
        };
        let raw = &c.data[start..c.ends[row] as usize];
        match std::str::from_utf8(raw) {
            Ok(s) => Cell::Text(s),
            Err(_) => Cell::Bytes(raw),
        }
    }
}

/// Builds a [`Batch`] row by row: call [`push`](Self::push) once per column,
/// in column order, then [`end_row`](Self::end_row).
pub struct BatchBuilder {
    rows: usize,
    next_col: usize,
    cols: Vec<Column>,
}

impl BatchBuilder {
    pub fn new(columns: usize) -> Self {
        Self {
            rows: 0,
            next_col: 0,
            cols: (0..columns).map(|_| Column::default()).collect(),
        }
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn push(&mut self, value: Option<&[u8]>) {
        let row = self.rows;
        let c = &mut self.cols[self.next_col];
        if row.is_multiple_of(64) {
            c.nulls.push(0);
        }
        match value {
            Some(v) => c.data.extend_from_slice(v),
            None => c.nulls[row / 64] |= 1 << (row % 64),
        }
        c.ends
            .push(u32::try_from(c.data.len()).expect("a batch column is limited to 4 GiB"));
        self.next_col += 1;
    }

    pub fn end_row(&mut self) {
        assert_eq!(
            self.next_col,
            self.cols.len(),
            "row has the wrong number of cells"
        );
        self.next_col = 0;
        self.rows += 1;
    }

    /// Returns the rows built so far and leaves the builder empty.
    pub fn finish(&mut self) -> Batch {
        assert_eq!(self.next_col, 0, "finish called in the middle of a row");
        let n = self.cols.len();
        let cols = std::mem::replace(&mut self.cols, (0..n).map(|_| Column::default()).collect());
        let mut cols = cols;
        for c in &mut cols {
            c.data.shrink_to_fit();
            c.ends.shrink_to_fit();
        }
        Batch {
            rows: std::mem::take(&mut self.rows),
            cols,
        }
    }
}

/// A growing result: the batches of one query in arrival order, addressed by
/// absolute row number.
#[derive(Debug, Default)]
pub struct ResultSet {
    columns: Vec<ColumnMeta>,
    batches: Vec<Batch>,
    /// `starts[i]` is the absolute row number of the first row of batch `i`.
    starts: Vec<usize>,
    rows: usize,
}

impl ResultSet {
    pub fn new(columns: Vec<ColumnMeta>) -> Self {
        Self {
            columns,
            ..Default::default()
        }
    }

    pub fn columns(&self) -> &[ColumnMeta] {
        &self.columns
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn bytes(&self) -> usize {
        self.batches.iter().map(Batch::bytes).sum()
    }

    pub fn push(&mut self, batch: Batch) {
        if batch.rows() == 0 {
            return;
        }
        self.starts.push(self.rows);
        self.rows += batch.rows();
        self.batches.push(batch);
    }

    pub fn cell(&self, row: usize, col: usize) -> Cell<'_> {
        let i = self.starts.partition_point(|s| *s <= row) - 1;
        self.batches[i].cell(row - self.starts[i], col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_round_trip_across_batches() {
        let mut b = BatchBuilder::new(2);
        let mut rs = ResultSet::new(vec![]);
        for r in 0..150usize {
            let text = format!("列{r}");
            b.push(if r % 7 == 0 {
                None
            } else {
                Some(text.as_bytes())
            });
            b.push(Some(&[0xff, r as u8]));
            b.end_row();
            if b.rows() == 64 {
                rs.push(b.finish());
            }
        }
        rs.push(b.finish());
        assert_eq!(rs.rows(), 150);
        for r in 0..150usize {
            let want = format!("列{r}");
            let want = if r % 7 == 0 {
                Cell::Null
            } else {
                Cell::Text(&want)
            };
            assert_eq!(rs.cell(r, 0), want, "row {r}");
            assert_eq!(rs.cell(r, 1), Cell::Bytes(&[0xff, r as u8]));
        }
    }

    #[test]
    fn empty_string_is_not_null() {
        let mut b = BatchBuilder::new(1);
        b.push(Some(b""));
        b.end_row();
        assert_eq!(b.finish().cell(0, 0), Cell::Text(""));
    }
}
