//! Statistics of a cell selection (footer under the grid): count and, for
//! numeric cells, sum / average / min / max. Numbers are exact decimals
//! (`bigdecimal`), so a column of `DECIMAL(20,4)` strings adds up without the
//! rounding a float would bring. Cells are fed one at a time, so the caller
//! can slice a 100,000-row selection into short steps and copy nothing.

use bigdecimal::{BigDecimal, RoundingMode};
use std::str::FromStr;

/// Largest exponent / scale accepted; `1e999999999` is text, not a number.
const MAX_SCALE: i64 = 400;
/// Digits after the decimal point an average gets beyond the widest input.
const AVG_EXTRA: i64 = 4;

#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Non-NULL cells.
    pub count: u64,
    pub nulls: u64,
    /// Non-NULL cells that parsed as numbers.
    pub numeric: u64,
    sum: BigDecimal,
    min: Option<BigDecimal>,
    max: Option<BigDecimal>,
    scale: i64,
}

/// Finished numbers as text, ready for the footer and the clipboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub count: u64,
    pub nulls: u64,
    pub numeric: u64,
    /// `None` when no cell was numeric.
    pub sum: Option<String>,
    pub avg: Option<String>,
    pub min: Option<String>,
    pub max: Option<String>,
}

/// Exact number from cell text, or `None` when it is not a plain number.
pub fn parse_number(s: &str) -> Option<BigDecimal> {
    let s = s.trim();
    if s.is_empty()
        || !s.bytes().any(|b| b.is_ascii_digit())
        || s.bytes()
            .any(|b| b.is_ascii_alphabetic() && b != b'e' && b != b'E')
    {
        return None;
    }
    let v = BigDecimal::from_str(s).ok()?;
    let (_, scale) = v.as_bigint_and_exponent();
    (scale.abs() <= MAX_SCALE).then_some(v)
}

impl Stats {
    pub fn new() -> Self {
        Self::default()
    }

    /// One cell: `None` = NULL.
    pub fn add(&mut self, cell: Option<&str>) {
        let Some(s) = cell else {
            self.nulls += 1;
            return;
        };
        self.count += 1;
        if let Some(v) = parse_number(s) {
            self.numeric += 1;
            let (_, scale) = v.as_bigint_and_exponent();
            self.scale = self.scale.max(scale.max(0));
            self.sum += &v;
            if self.min.as_ref().is_none_or(|m| v < *m) {
                self.min = Some(v.clone());
            }
            if self.max.as_ref().is_none_or(|m| v > *m) {
                self.max = Some(v);
            }
        }
    }

    pub fn summary(&self) -> Summary {
        let some = self.numeric > 0;
        Summary {
            count: self.count,
            nulls: self.nulls,
            numeric: self.numeric,
            sum: some.then(|| self.sum.to_plain_string()),
            avg: some.then(|| self.average()),
            min: self.min.as_ref().map(BigDecimal::to_plain_string),
            max: self.max.as_ref().map(BigDecimal::to_plain_string),
        }
    }

    fn average(&self) -> String {
        let n = BigDecimal::from(self.numeric);
        let digits = self.scale + AVG_EXTRA;
        let text = (&self.sum / n)
            .with_scale_round(digits, RoundingMode::HalfUp)
            .to_plain_string();
        // Keep the widest input scale, drop zeros that only came from rounding.
        match text.split_once('.') {
            Some((int, frac)) => {
                let keep = frac
                    .trim_end_matches('0')
                    .len()
                    .max(self.scale as usize)
                    .min(frac.len());
                if keep == 0 {
                    int.to_string()
                } else {
                    format!("{int}.{}", &frac[..keep])
                }
            }
            None => text,
        }
    }
}

impl Summary {
    /// The text a click puts on the clipboard.
    pub fn clipboard(&self) -> String {
        let mut s = format!("count: {}", self.count);
        for (name, v) in [
            ("sum", &self.sum),
            ("avg", &self.avg),
            ("min", &self.min),
            ("max", &self.max),
        ] {
            if let Some(v) = v {
                s.push_str(&format!("\n{name}: {v}"));
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cells: &[Option<&str>]) -> Summary {
        let mut s = Stats::new();
        for c in cells {
            s.add(*c);
        }
        s.summary()
    }

    #[test]
    fn decimal_sum_is_exact() {
        let r = run(&[Some("0.1"), Some("0.2"), Some("0.30")]);
        assert_eq!(r.sum.as_deref(), Some("0.60"));
        assert_eq!(r.avg.as_deref(), Some("0.20"));
        let big = run(&[Some("12345678901234567890.1234"), Some("0.0001")]);
        assert_eq!(big.sum.as_deref(), Some("12345678901234567890.1235"));
    }

    #[test]
    fn null_not_counted_text_only_counts() {
        let r = run(&[Some("5"), None, Some("abc"), Some("-1"), Some("")]);
        assert_eq!((r.count, r.nulls, r.numeric), (4, 1, 2));
        assert_eq!(r.sum.as_deref(), Some("4"));
        assert_eq!(
            (r.min.as_deref(), r.max.as_deref()),
            (Some("-1"), Some("5"))
        );
    }

    #[test]
    fn no_numbers_means_no_numeric_stats() {
        let r = run(&[Some("a"), Some("b")]);
        assert_eq!(r.count, 2);
        assert!(r.sum.is_none() && r.avg.is_none() && r.min.is_none());
    }

    #[test]
    fn average_rounds_and_trims() {
        assert_eq!(run(&[Some("1"), Some("2")]).avg.as_deref(), Some("1.5"));
        assert_eq!(
            run(&[Some("1"), Some("1"), Some("2")]).avg.as_deref(),
            Some("1.3333")
        );
        assert_eq!(run(&[Some("2"), Some("4")]).avg.as_deref(), Some("3"));
    }

    #[test]
    fn rejects_non_numbers() {
        for s in ["NaN", "Infinity", "0x1F", "12abc", "1e999999", "-", "."] {
            assert!(parse_number(s).is_none(), "{s}");
        }
        assert!(parse_number(" 1e3 ").is_some());
        assert!(parse_number("+.5").is_some());
    }

    #[test]
    fn clipboard_text() {
        assert_eq!(
            run(&[Some("1"), Some("3")]).clipboard(),
            "count: 2\nsum: 4\navg: 2\nmin: 1\nmax: 3"
        );
        assert_eq!(run(&[Some("x")]).clipboard(), "count: 1");
    }
}
