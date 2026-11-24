//! Which result tabs fit the strip, and how wide each is (Chrome-style): tabs share the room
//! evenly down to `MIN_W`, the active tab keeps its full name up to `ACTIVE_MAX`, and what still
//! does not fit goes behind an overflow button.

/// Narrowest a tab gets.
pub const MIN_W: f32 = 96.0;
/// Widest any tab (the active one included) gets.
pub const ACTIVE_MAX: f32 = 220.0;
/// Room kept for the overflow button once some tabs are hidden.
pub const OVERFLOW_W: f32 = 48.0;

/// Rough width in px of `s` at the tab font size (CJK glyphs are about twice as wide).
pub fn text_w(s: &str) -> f32 {
    s.chars()
        .map(|c| if (c as u32) >= 0x2E80 { 14.0 } else { 7.2 })
        .sum()
}

#[derive(Debug, PartialEq)]
pub struct Fit {
    /// Result indices in display order with their widths.
    pub visible: Vec<(usize, f32)>,
    /// Results behind the overflow button.
    pub hidden: usize,
}

/// `natural[i]` is the full width of tab `i` (padding, label and count). `avail` is the room
/// for the tabs and the overflow button.
/// `active_min` is the least the active tab gets when it has to give way (its first dozen
/// characters and the count).
pub fn fit(natural: &[f32], active: Option<usize>, active_min: f32, avail: f32) -> Fit {
    let n = natural.len();
    if n == 0 {
        return Fit {
            visible: vec![],
            hidden: 0,
        };
    }
    let cap = |i: usize| natural[i].min(ACTIVE_MAX);
    if (0..n).map(cap).sum::<f32>() <= avail {
        return Fit {
            visible: (0..n).map(|i| (i, cap(i))).collect(),
            hidden: 0,
        };
    }
    let a = active.unwrap_or(0).min(n - 1);
    let aw = cap(a).max(MIN_W);
    // The most tabs that fit at MIN_W (the active one at full width); all of them need no button.
    let k = if aw + (n - 1) as f32 * MIN_W <= avail {
        n
    } else {
        (1..n)
            .rev()
            .find(|k| aw + (*k - 1) as f32 * MIN_W + OVERFLOW_W <= avail)
            .unwrap_or(1)
    };
    let room = avail - if k < n { OVERFLOW_W } else { 0.0 } - aw;
    let share = if k > 1 {
        (room / (k - 1) as f32).max(MIN_W)
    } else {
        0.0
    };
    let mut idx: Vec<usize> = (0..k).collect();
    if a >= k {
        idx[k - 1] = a;
    }
    let visible = idx
        .into_iter()
        .map(|i| {
            let w = if i == a { aw } else { cap(i).min(share) };
            // A lone tab next to the button gives up width before the button is clipped.
            (
                i,
                if k == 1 && n > 1 {
                    w.min((avail - OVERFLOW_W).max(active_min.min(w)))
                } else {
                    w
                },
            )
        })
        .collect();
    Fit {
        visible,
        hidden: n - k,
    }
}

/// True when the two names start with the same four characters (so an end cut would make them
/// look alike).
pub fn shares_prefix(a: &str, b: &str) -> bool {
    let n = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    n >= 4
}

/// Cuts `s` in the middle so it takes about `avail` px (see [`text_w`]): head and tail stay, e.g.
/// `wb_te…t6`. A name that already fits is returned as is.
pub fn elide_middle(s: &str, avail: f32) -> String {
    if text_w(s) <= avail {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    // Drop characters from the middle until head + ellipsis + tail fit.
    let mut keep = chars.len();
    while keep > 3 {
        keep -= 1;
        let tail = ((keep as f32) * 0.3).round().max(2.0) as usize;
        let head = keep.saturating_sub(tail);
        let cand: String = chars[..head].iter().collect::<String>()
            + "…"
            + &chars[chars.len() - tail..].iter().collect::<String>();
        if text_w(&cand) <= avail {
            return cand;
        }
    }
    "…".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_fits_at_natural_width() {
        let f = fit(&[100.0, 150.0, 300.0], Some(1), 100.0, 600.0);
        assert_eq!(f.hidden, 0);
        assert_eq!(f.visible, vec![(0, 100.0), (1, 150.0), (2, ACTIVE_MAX)]);
    }

    #[test]
    fn tabs_share_evenly_down_to_the_minimum() {
        let nat = [200.0; 4];
        let f = fit(&nat, Some(0), 100.0, 600.0);
        assert_eq!(f.hidden, 0);
        let w: Vec<f32> = f.visible.iter().map(|v| v.1).collect();
        assert_eq!(w[0], 200.0, "the active tab keeps its full name");
        assert!(w[1..].iter().all(|x| (*x - 400.0 / 3.0).abs() < 0.01));
        assert!(w.iter().sum::<f32>() <= 600.0 + 0.01);
    }

    #[test]
    fn what_does_not_fit_goes_behind_the_button() {
        let nat = [200.0; 10];
        let f = fit(&nat, Some(0), 100.0, 600.0);
        // 200 + 2 * 96 + 48 = 440 <= 600 < 200 + 3 * 96 + 48.
        assert_eq!(f.visible.len(), 4, "{f:?}");
        assert_eq!(f.hidden, 6);
        let total: f32 = f.visible.iter().map(|v| v.1).sum();
        assert!(total + OVERFLOW_W <= 600.01, "{total}");
        assert!(f.visible.iter().all(|v| v.1 >= MIN_W));
    }

    #[test]
    fn a_hidden_active_tab_takes_the_last_visible_slot() {
        let nat = [200.0; 10];
        let f = fit(&nat, Some(8), 100.0, 600.0);
        let idx: Vec<usize> = f.visible.iter().map(|v| v.0).collect();
        assert_eq!(idx, vec![0, 1, 2, 8]);
        assert_eq!(f.visible[3].1, 200.0);
        assert_eq!(f.hidden, 6);
    }

    #[test]
    fn very_narrow_keeps_the_active_tab_alone() {
        let f = fit(&[200.0; 5], Some(3), 100.0, 200.0);
        assert_eq!(f.visible.len(), 1);
        assert_eq!(f.visible[0].0, 3);
        assert!(f.visible[0].1 + OVERFLOW_W <= 200.01, "room for the button");
        assert_eq!(f.hidden, 4);
        assert_eq!(fit(&[], None, 100.0, 100.0).visible.len(), 0);
    }

    #[test]
    fn long_active_tab_keeps_its_dozen_characters() {
        let f = fit(&[220.0; 4], Some(0), 140.0, 200.0);
        assert_eq!(f.visible.len(), 1);
        assert_eq!(f.visible[0].1, 152.0);
        let f = fit(&[220.0; 4], Some(0), 170.0, 200.0);
        assert_eq!(f.visible[0].1, 170.0, "never below the dozen characters");
    }

    #[test]
    fn middle_elision_keeps_head_and_tail() {
        assert_eq!(elide_middle("short", 100.0), "short");
        let e = elide_middle("wb_test_t6_extra_long", 70.0);
        assert!(
            e.starts_with("wb_t") && e.ends_with("ng") && e.contains('…'),
            "{e}"
        );
        assert!(text_w(&e) <= 70.0);
        assert!(shares_prefix("wb_test_t6", "wb_test_t7"));
        assert!(!shares_prefix("student", "booking"));
    }

    #[test]
    fn text_width_counts_cjk_wider() {
        assert!(text_w("學生") > text_w("ab") * 1.5);
    }
}
