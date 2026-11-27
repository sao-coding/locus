//! Tab stops of an accepted snippet: which byte ranges Tab / Shift+Tab visit,
//! and how those ranges move while the user types. Pure bookkeeping on byte
//! offsets; the editor only feeds it its edits and selects what it returns.
//!
//! Not done: mirrored stops. The same number used twice is visited once (its
//! first place); the other places keep their default text.

use std::ops::Range;

/// Where the caret goes after a step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Select this range (an empty range is a caret) and stay in snippet mode.
    Select(Range<usize>),
    /// Put the caret here and leave snippet mode (`$0` / the end).
    Finish(usize),
}

/// A live snippet: its tab stops in visiting order plus the whole inserted text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    stops: Vec<Range<usize>>,
    whole: Range<usize>,
    current: usize,
}

/// Moves `r` for an edit that replaced `a..b` with `new_len` bytes. `inclusive`
/// ranges (the stop being typed in, the whole snippet) also grow when the edit
/// touches their ends. `None`: the edit cuts across the range.
fn shift(
    r: &Range<usize>,
    a: usize,
    b: usize,
    new_len: usize,
    inclusive: bool,
) -> Option<Range<usize>> {
    let grow = |x: usize| x + new_len - (b - a);
    if inclusive && r.start <= a && b <= r.end {
        return Some(r.start..grow(r.end));
    }
    if b <= r.start {
        return Some(grow(r.start)..grow(r.end));
    }
    if a >= r.end {
        return Some(r.clone());
    }
    (r.start <= a && b <= r.end).then(|| r.start..grow(r.end))
}

impl Session {
    /// Stops of a snippet inserted at byte `at` (numbers from
    /// `saved::expand_numbered`, offsets relative to the snippet text of
    /// `len` bytes). `None` when the snippet has no numbered stop.
    pub fn new(at: usize, len: usize, numbered: &[(u32, Range<usize>)]) -> Option<Session> {
        let mut stops: Vec<Range<usize>> = Vec::new();
        let mut seen: Vec<u32> = Vec::new();
        let mut last = None;
        for (n, r) in numbered {
            if seen.contains(n) {
                continue;
            }
            seen.push(*n);
            let r = at + r.start..at + r.end;
            if *n == 0 {
                last = Some(r);
            } else {
                stops.push(r);
            }
        }
        if stops.is_empty() {
            return None;
        }
        stops.push(last.unwrap_or(at + len..at + len));
        Some(Session {
            stops,
            whole: at..at + len,
            current: 0,
        })
    }

    /// The first stop, to select right after the snippet went in.
    pub fn first(&self) -> Range<usize> {
        self.stops[0].clone()
    }

    pub fn current(&self) -> Range<usize> {
        self.stops[self.current].clone()
    }

    /// Tab: the next stop; past the last one (`$0`) the snippet is over.
    pub fn advance(&mut self) -> Step {
        if self.current + 1 >= self.stops.len() {
            return Step::Finish(self.stops[self.stops.len() - 1].end);
        }
        self.current += 1;
        if self.current + 1 == self.stops.len() {
            // `$0` is the end: caret there and done.
            return Step::Finish(self.stops[self.current].end);
        }
        Step::Select(self.current())
    }

    /// Shift+Tab: the previous stop (stays on the first).
    pub fn retreat(&mut self) -> Range<usize> {
        self.current = self.current.saturating_sub(1);
        self.current()
    }

    /// Whether a caret / selection lies inside the snippet (ends included).
    pub fn contains(&self, sel: &Range<usize>) -> bool {
        self.whole.start <= sel.start && sel.end <= self.whole.end
    }

    /// The text changed: `a..b` was replaced by `new_len` bytes. Keeps every
    /// range right; `false` when the edit cut across a stop or the snippet
    /// (it is no longer one piece, so snippet mode ends).
    pub fn on_edit(&mut self, a: usize, b: usize, new_len: usize) -> bool {
        let Some(whole) = shift(&self.whole, a, b, new_len, true) else {
            return false;
        };
        let mut next = Vec::with_capacity(self.stops.len());
        for (i, r) in self.stops.iter().enumerate() {
            match shift(r, a, b, new_len, i == self.current) {
                Some(r) => next.push(r),
                None => return false,
            }
        }
        self.whole = whole;
        self.stops = next;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::saved::expand_numbered;

    fn session(at: usize, body: &str) -> (String, Session) {
        let (text, stops) = expand_numbered(body);
        let s = Session::new(at, text.len(), &stops).unwrap();
        (text, s)
    }

    #[test]
    fn walks_stops_then_finishes_at_zero() {
        let (text, mut s) = session(10, "SELECT ${1:*} FROM ${2:table} WHERE $0");
        assert_eq!(text, "SELECT * FROM table WHERE ");
        assert_eq!(s.first(), 17..18);
        assert_eq!(s.advance(), Step::Select(24..29));
        assert_eq!(s.advance(), Step::Finish(10 + text.len()));
    }

    #[test]
    fn prev_goes_back_and_stops_at_first() {
        let (_, mut s) = session(0, "${1:a} ${2:b} ${3:c}");
        s.advance();
        s.advance();
        assert_eq!(s.retreat(), 2..3);
        assert_eq!(s.retreat(), 0..1);
        assert_eq!(s.retreat(), 0..1);
    }

    #[test]
    fn without_zero_the_end_is_the_last_stop() {
        let (text, mut s) = session(5, "a ${1:b} c");
        assert_eq!(s.advance(), Step::Finish(5 + text.len()));
    }

    #[test]
    fn empty_stop_is_a_caret() {
        let (_, s) = session(0, "f($1)");
        assert_eq!(s.first(), 2..2);
    }

    #[test]
    fn no_numbered_stop_is_no_session() {
        let (text, stops) = expand_numbered("SELECT 1$0");
        assert!(Session::new(0, text.len(), &stops).is_none());
        let (text, stops) = expand_numbered("plain");
        assert!(Session::new(0, text.len(), &stops).is_none());
    }

    #[test]
    fn mirrored_number_is_visited_once() {
        let (text, mut s) = session(0, "${1:x} = ${1:x} ${2:y}");
        assert_eq!(text, "x = x y");
        assert_eq!(s.first(), 0..1);
        assert_eq!(s.advance(), Step::Select(6..7));
    }

    #[test]
    fn typing_in_a_stop_shifts_the_later_ones() {
        // "SELECT * FROM table": the `*` is replaced by "id, name" (8 bytes for 1).
        let (_, mut s) = session(0, "SELECT ${1:*} FROM ${2:table}");
        assert!(s.on_edit(7, 8, 8));
        assert_eq!(s.current(), 7..15);
        assert_eq!(s.stops[1], 21..26);
    }

    #[test]
    fn typing_at_the_end_of_the_current_stop_extends_it() {
        let (_, mut s) = session(0, "${1:ab} ${2:cd}");
        assert!(s.on_edit(2, 2, 1));
        assert_eq!(s.current(), 0..3);
        assert_eq!(s.stops[1], 4..6);
    }

    #[test]
    fn typing_into_an_empty_stop_fills_it() {
        let (_, mut s) = session(0, "f($1, $2)");
        assert!(s.on_edit(2, 2, 3));
        assert_eq!(s.current(), 2..5);
        assert_eq!(s.stops[1], 7..7);
    }

    #[test]
    fn delete_and_insert_before_a_stop() {
        let (_, mut s) = session(10, "ab ${1:cd} ${2:ef}");
        // Text before the snippet grows by 4, then shrinks by 2.
        assert!(s.on_edit(0, 0, 4));
        assert_eq!(s.first(), 17..19);
        assert!(s.on_edit(0, 2, 0));
        assert_eq!(s.first(), 15..17);
        assert_eq!(s.stops[1], 18..20);
        assert!(s.contains(&(13..13)));
    }

    #[test]
    fn edit_after_everything_changes_nothing() {
        let (_, mut s) = session(0, "${1:a} ${2:b}");
        let before = s.clone();
        assert!(s.on_edit(50, 50, 3));
        assert_eq!(s, before);
    }

    #[test]
    fn deleting_inside_the_current_stop_shrinks_it() {
        let (_, mut s) = session(0, "${1:abcd} ${2:e}");
        assert!(s.on_edit(1, 3, 0));
        assert_eq!(s.current(), 0..2);
        assert_eq!(s.stops[1], 3..4);
    }

    #[test]
    fn cutting_across_a_stop_ends_the_snippet() {
        let (_, mut s) = session(0, "${1:abcd} ${2:e}");
        assert!(!s.on_edit(2, 6, 0));
    }

    #[test]
    fn cjk_uses_byte_offsets() {
        let (text, mut s) = session(0, "SELECT ${1:欄位} FROM ${2:表}");
        assert_eq!(text, "SELECT 欄位 FROM 表");
        assert_eq!(s.first(), 7..13);
        // Replace the six bytes with "名" (3 bytes): later stop moves left by 3.
        assert!(s.on_edit(7, 13, 3));
        assert_eq!(s.current(), 7..10);
        assert_eq!(s.stops[1], 16..19);
        assert_eq!(&text.replace("欄位", "名")[16..19], "表");
    }

    #[test]
    fn nested_ranges_follow_edits_inside_and_before_the_inner_one() {
        // An outer stop 0..10 holding an inner one at 4..8 (what a nested
        // default looks like once expanded), plus a stop after both.
        let mut s = Session {
            stops: vec![0..10, 4..8, 12..14],
            whole: 0..14,
            current: 1,
        };
        // Inner stop is current: typing in it grows it, the outer one and the rest.
        assert!(s.on_edit(6, 6, 2));
        assert_eq!(s.stops, vec![0..12, 4..10, 14..16]);
        // Typing between the outer start and the inner one moves only the inner and the later ones.
        s.current = 0;
        assert!(s.on_edit(2, 2, 1));
        assert_eq!(s.stops, vec![0..13, 5..11, 15..17]);
        assert_eq!(s.whole, 0..17);
    }

    #[test]
    fn caret_outside_the_snippet_is_detected() {
        let (_, s) = session(10, "a ${1:b}");
        assert!(s.contains(&(10..10)));
        assert!(s.contains(&(12..13)));
        assert!(s.contains(&(13..13)));
        assert!(!s.contains(&(9..9)));
        assert!(!s.contains(&(14..14)));
    }
}
