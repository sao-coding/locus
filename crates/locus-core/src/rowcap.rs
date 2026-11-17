//! The per-query memory cap (`Settings::rows_cap`): decides how many rows of
//! each incoming batch are kept and whether the result was really cut off.
//! A result of exactly `cap` rows is complete; it is only truncated when at
//! least one more row arrived after the cap was reached.

/// What to do with one incoming batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Admit {
    /// Rows of the batch to keep (a prefix).
    pub keep: u64,
    /// This batch proved the result is cut off: stop the query on the server.
    pub truncated_now: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct RowCap {
    cap: u64,
    total: u64,
    truncated: bool,
}

impl RowCap {
    /// `cap == u64::MAX` means no cap.
    pub fn new(cap: u64) -> Self {
        Self {
            cap,
            total: 0,
            truncated: false,
        }
    }

    /// Whether rows beyond the cap were seen (the banner condition).
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Counts a batch of `rows`. No extra work per row: one comparison per batch.
    pub fn admit(&mut self, rows: u64) -> Admit {
        if self.truncated || rows == 0 {
            return Admit {
                keep: 0,
                truncated_now: false,
            };
        }
        let left = self.cap.saturating_sub(self.total);
        if rows <= left {
            self.total += rows;
            return Admit {
                keep: rows,
                truncated_now: false,
            };
        }
        // More rows than the cap allows: a real extra row exists.
        self.total += left;
        self.truncated = true;
        Admit {
            keep: left,
            truncated_now: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_cap_rows_is_not_truncated() {
        let mut c = RowCap::new(100);
        assert_eq!(
            c.admit(60),
            Admit {
                keep: 60,
                truncated_now: false
            }
        );
        assert_eq!(
            c.admit(40),
            Admit {
                keep: 40,
                truncated_now: false
            }
        );
        assert!(!c.truncated());
    }

    #[test]
    fn one_more_row_than_cap_is_truncated_and_keeps_cap_rows() {
        let mut c = RowCap::new(100);
        assert_eq!(
            c.admit(100),
            Admit {
                keep: 100,
                truncated_now: false
            }
        );
        assert!(!c.truncated());
        assert_eq!(
            c.admit(1),
            Admit {
                keep: 0,
                truncated_now: true
            }
        );
        assert!(c.truncated());
        // Later batches are dropped.
        assert_eq!(
            c.admit(5),
            Admit {
                keep: 0,
                truncated_now: false
            }
        );
    }

    #[test]
    fn batch_straddling_the_cap_is_cut_and_truncated() {
        let mut c = RowCap::new(100);
        assert_eq!(c.admit(70).keep, 70);
        assert_eq!(
            c.admit(31),
            Admit {
                keep: 30,
                truncated_now: true
            }
        );
        assert!(c.truncated());
    }

    #[test]
    fn no_cap_never_truncates() {
        let mut c = RowCap::new(u64::MAX);
        for _ in 0..1000 {
            assert_eq!(c.admit(1_000_000).keep, 1_000_000);
        }
        assert!(!c.truncated());
    }

    #[test]
    fn empty_batches_change_nothing() {
        let mut c = RowCap::new(10);
        assert_eq!(c.admit(10).keep, 10);
        assert_eq!(
            c.admit(0),
            Admit {
                keep: 0,
                truncated_now: false
            }
        );
        assert!(!c.truncated());
    }
}
