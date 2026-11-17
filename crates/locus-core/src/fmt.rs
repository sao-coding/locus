//! The one place that turns numbers and durations into user-visible text, so
//! both can follow the current language.

use rust_i18n::t;
use std::time::Duration;

/// `40450` -> `40,450` (separator from the locale file).
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let sep = t!("fmt.group_separator");
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 * sep.len());
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push_str(&sep);
        }
        out.push(ch);
    }
    out
}

pub fn rows(n: u64) -> String {
    if n == 1 {
        t!("fmt.row_one").into_owned()
    } else {
        t!("fmt.rows", n = count(n)).into_owned()
    }
}

/// Short duration for the status bar: `850 ms`, `1.25 s`, `2 min 05 s`.
pub fn duration(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 1000.0 {
        t!("fmt.ms", v = format!("{ms:.0}")).into_owned()
    } else if ms < 60_000.0 {
        t!("fmt.seconds", v = format!("{:.2}", ms / 1000.0)).into_owned()
    } else {
        let s = d.as_secs();
        t!(
            "fmt.minutes",
            m = (s / 60).to_string(),
            s = format!("{:02}", s % 60)
        )
        .into_owned()
    }
}
