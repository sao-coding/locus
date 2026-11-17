//! Value conversion of the import: text cell → the canonical text a bound
//! parameter carries (numbers with thousands separators, booleans, dates).
//! Pure, no I/O.

/// What a target column holds, derived from its SQL type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    Integer,
    Number,
    Bool,
    Date,
    DateTime,
    Text,
}

impl TargetKind {
    /// Classifies a type as the server writes it (`varchar(191)`, `NUMBER(10,2)`,
    /// `timestamp without time zone`, `tinyint(1)` …).
    pub fn of_type(type_name: &str) -> TargetKind {
        let t = type_name.to_ascii_lowercase();
        let base = t.split('(').next().unwrap_or("").trim();
        if t.starts_with("tinyint(1)") || base == "bool" || base == "boolean" || base == "bit" {
            return TargetKind::Bool;
        }
        if base.starts_with("timestamp") || base.starts_with("datetime") {
            return TargetKind::DateTime;
        }
        if base == "date" {
            // Oracle DATE carries a time; the Oracle caller maps it itself.
            return TargetKind::Date;
        }
        if base.contains("int") || base.contains("serial") {
            return TargetKind::Integer;
        }
        if base.starts_with("number") {
            // NUMBER(10) / NUMBER(10,0) hold integers, everything else decimals.
            if let Some(args) = t.split('(').nth(1) {
                let args = args.trim_end_matches(')');
                let mut it = args.split(',');
                let _p = it.next();
                return match it.next().map(|s| s.trim()) {
                    None | Some("0") => TargetKind::Integer,
                    _ => TargetKind::Number,
                };
            }
            return TargetKind::Number;
        }
        if ["decimal", "numeric", "float", "double", "real", "money"]
            .iter()
            .any(|k| base.starts_with(k))
        {
            return TargetKind::Number;
        }
        TargetKind::Text
    }
}

/// How dates in the file are written.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum DateFormat {
    /// `2024-03-05`, `2024/3/5`, `2024.03.05`, `20240305`, with an optional
    /// time (`T` or space) after it.
    #[default]
    Auto,
    /// `%Y %y %m %d %H %M %S` and literal characters, e.g. `%d/%m/%Y`.
    Custom(String),
}

#[derive(Clone, Debug)]
pub struct ConvertOptions {
    pub empty_as_null: bool,
    pub trim: bool,
    pub date_format: DateFormat,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            empty_as_null: true,
            trim: true,
            date_format: DateFormat::Auto,
        }
    }
}

/// Converts one cell. `Ok(None)` is SQL NULL.
pub fn convert(raw: &str, kind: TargetKind, o: &ConvertOptions) -> Result<Option<String>, String> {
    let v = if o.trim { raw.trim() } else { raw };
    if v.trim().is_empty() {
        // An empty or blank cell is NULL for every non-text column (nothing
        // else parses); text keeps it unless the option says NULL.
        return if o.empty_as_null || kind != TargetKind::Text {
            Ok(None)
        } else {
            Ok(Some(v.to_string()))
        };
    }
    match kind {
        TargetKind::Text => Ok(Some(v.to_string())),
        TargetKind::Number => parse_number(v).map(Some),
        TargetKind::Integer => {
            let n = parse_number(v)?;
            integral(&n)
                .map(Some)
                .ok_or_else(|| format!("not an integer: {v}"))
        }
        TargetKind::Bool => parse_bool(v).map(|b| Some(if b { "1" } else { "0" }.to_string())),
        TargetKind::Date => parse_datetime(v, &o.date_format).map(|d| Some(d.date_text())),
        TargetKind::DateTime => parse_datetime(v, &o.date_format).map(|d| Some(d.text())),
    }
}

/// `"12.00"` → `"12"`, `"12.5"` → None.
fn integral(n: &str) -> Option<String> {
    match n.split_once('.') {
        None => Some(n.to_string()),
        Some((i, f)) if f.chars().all(|c| c == '0') => Some(i.to_string()),
        _ => None,
    }
}

/// A number with optional sign, thousands separators (`1,234.5`, `1.234,5`,
/// `1 234,5`, `1'234`) and exponent → canonical `-1234.5`.
pub fn parse_number(s: &str) -> Result<String, String> {
    let bad = || format!("not a number: {s}");
    let mut t: String = s
        .trim()
        .chars()
        .filter(|c| !matches!(c, ' ' | '\u{a0}' | '\'' | '_'))
        .collect();
    let mut neg = false;
    if t.starts_with('(') && t.ends_with(')') {
        neg = true;
        t = t[1..t.len() - 1].to_string();
    }
    if let Some(r) = t.strip_prefix('-') {
        neg = !neg;
        t = r.to_string();
    } else if let Some(r) = t.strip_prefix('+') {
        t = r.to_string();
    }
    if t.is_empty() {
        return Err(bad());
    }
    let (mant, exp) = match t.find(['e', 'E']) {
        Some(i) => (t[..i].to_string(), Some(t[i + 1..].to_string())),
        None => (t.clone(), None),
    };
    let commas = mant.matches(',').count();
    let dots = mant.matches('.').count();
    let (int_part, frac_part) = if commas > 0 && dots > 0 {
        // Both present: the last one is the decimal separator.
        let (dec, th) = if mant.rfind(',') > mant.rfind('.') {
            (',', '.')
        } else {
            ('.', ',')
        };
        let cleaned: String = mant.chars().filter(|&c| c != th).collect();
        if cleaned.matches(dec).count() != 1 {
            return Err(bad());
        }
        let (i, f) = cleaned.split_once(dec).unwrap();
        (i.to_string(), f.to_string())
    } else if commas > 0 {
        let groups: Vec<&str> = mant.split(',').collect();
        let thousands = groups[0].len() <= 3
            && !groups[0].is_empty()
            && groups[1..].iter().all(|g| g.len() == 3);
        if thousands && (commas > 1 || groups[1].len() == 3) {
            (groups.concat(), String::new())
        } else if commas == 1 {
            let (i, f) = mant.split_once(',').unwrap();
            (i.to_string(), f.to_string())
        } else {
            return Err(bad());
        }
    } else if dots > 1 {
        // 1.234.567 : dots as thousands separators.
        let groups: Vec<&str> = mant.split('.').collect();
        if groups[0].len() <= 3 && !groups[0].is_empty() && groups[1..].iter().all(|g| g.len() == 3)
        {
            (groups.concat(), String::new())
        } else {
            return Err(bad());
        }
    } else if let Some((i, f)) = mant.split_once('.') {
        (i.to_string(), f.to_string())
    } else {
        (mant.clone(), String::new())
    };
    if !int_part.chars().all(|c| c.is_ascii_digit())
        || !frac_part.chars().all(|c| c.is_ascii_digit())
        || (int_part.is_empty() && frac_part.is_empty())
    {
        return Err(bad());
    }
    let int_part = if int_part.is_empty() {
        "0".to_string()
    } else {
        int_part
    };
    let mut out = if frac_part.is_empty() {
        int_part
    } else {
        format!("{int_part}.{frac_part}")
    };
    if let Some(e) = exp {
        let ev = e.strip_prefix('+').unwrap_or(&e);
        if ev.is_empty()
            || !ev
                .trim_start_matches('-')
                .chars()
                .all(|c| c.is_ascii_digit())
            || ev.trim_start_matches('-').is_empty()
        {
            return Err(bad());
        }
        out = format!("{out}e{ev}");
    }
    let zero = out.chars().all(|c| matches!(c, '0' | '.'));
    if neg && !zero {
        out.insert(0, '-');
    }
    Ok(out)
}

pub fn parse_bool(s: &str) -> Result<bool, String> {
    match s.trim().to_lowercase().as_str() {
        "1" | "true" | "t" | "yes" | "y" | "on" | "是" | "真" | "有" => Ok(true),
        "0" | "false" | "f" | "no" | "n" | "off" | "否" | "假" | "無" | "无" => Ok(false),
        _ => Err(format!("not a boolean: {s}")),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub y: i32,
    pub mo: u32,
    pub d: u32,
    pub h: u32,
    pub mi: u32,
    pub s: u32,
    pub has_time: bool,
}

impl Stamp {
    pub fn date_text(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.y, self.mo, self.d)
    }
    pub fn text(&self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.y, self.mo, self.d, self.h, self.mi, self.s
        )
    }
}

pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

fn stamp(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32, has_time: bool) -> Option<Stamp> {
    if !(1..=12).contains(&mo) || d == 0 || d > days_in_month(y, mo) || h > 23 || mi > 59 || s > 59
    {
        return None;
    }
    Some(Stamp {
        y,
        mo,
        d,
        h,
        mi,
        s,
        has_time,
    })
}

pub fn parse_datetime(s: &str, f: &DateFormat) -> Result<Stamp, String> {
    let r = match f {
        DateFormat::Auto => parse_auto(s),
        DateFormat::Custom(p) => parse_pattern(s, p),
    };
    r.ok_or_else(|| format!("not a date: {s}"))
}

/// Reads up to `max` digits (at least `min`); returns value and the rest.
fn digits(s: &str, min: usize, max: usize) -> Option<(u32, &str)> {
    let n = s
        .bytes()
        .take(max)
        .take_while(|b| b.is_ascii_digit())
        .count();
    if n < min {
        return None;
    }
    Some((s[..n].parse().ok()?, &s[n..]))
}

fn parse_auto(s: &str) -> Option<Stamp> {
    let s = s.trim();
    // Compact: 20240305 or 20240305123000.
    if s.bytes().all(|b| b.is_ascii_digit()) {
        return match s.len() {
            8 => stamp(
                s[..4].parse().ok()?,
                s[4..6].parse().ok()?,
                s[6..8].parse().ok()?,
                0,
                0,
                0,
                false,
            ),
            14 => stamp(
                s[..4].parse().ok()?,
                s[4..6].parse().ok()?,
                s[6..8].parse().ok()?,
                s[8..10].parse().ok()?,
                s[10..12].parse().ok()?,
                s[12..14].parse().ok()?,
                true,
            ),
            _ => None,
        };
    }
    let (y, r) = digits(s, 4, 4)?;
    let sep = r.chars().next()?;
    if !matches!(sep, '-' | '/' | '.') {
        return None;
    }
    let (mo, r) = digits(&r[1..], 1, 2)?;
    let r = r.strip_prefix(sep)?;
    let (d, r) = digits(r, 1, 2)?;
    let r = r.trim_start_matches(['T', ' ']);
    if r.is_empty() {
        return stamp(y as i32, mo, d, 0, 0, 0, false);
    }
    let r = r.strip_suffix('Z').unwrap_or(r);
    let (h, r) = digits(r, 1, 2)?;
    let r = r.strip_prefix(':')?;
    let (mi, r) = digits(r, 2, 2)?;
    let (sec, r) = match r.strip_prefix(':') {
        Some(r) => digits(r, 2, 2)?,
        None => (0, r),
    };
    // Fraction and zone offset are dropped.
    let r = r
        .strip_prefix('.')
        .map(|r| r.trim_start_matches(|c: char| c.is_ascii_digit()))
        .unwrap_or(r);
    if !(r.is_empty() || r.starts_with('+') || r.starts_with('-')) {
        return None;
    }
    stamp(y as i32, mo, d, h, mi, sec, true)
}

fn parse_pattern(s: &str, p: &str) -> Option<Stamp> {
    let mut s = s.trim();
    let (mut y, mut mo, mut d, mut h, mut mi, mut sec) = (None, None, None, 0u32, 0u32, 0u32);
    let mut has_time = false;
    let mut it = p.chars().peekable();
    while let Some(c) = it.next() {
        if c == '%' {
            let spec = it.next()?;
            match spec {
                'Y' => {
                    let (v, r) = digits(s, 4, 4)?;
                    y = Some(v as i32);
                    s = r;
                }
                'y' => {
                    let (v, r) = digits(s, 2, 2)?;
                    y = Some(if v < 70 {
                        2000 + v as i32
                    } else {
                        1900 + v as i32
                    });
                    s = r;
                }
                'm' => {
                    let (v, r) = digits(s, 1, 2)?;
                    mo = Some(v);
                    s = r;
                }
                'd' => {
                    let (v, r) = digits(s, 1, 2)?;
                    d = Some(v);
                    s = r;
                }
                'H' => {
                    let (v, r) = digits(s, 1, 2)?;
                    h = v;
                    has_time = true;
                    s = r;
                }
                'M' => {
                    let (v, r) = digits(s, 1, 2)?;
                    mi = v;
                    s = r;
                }
                'S' => {
                    let (v, r) = digits(s, 1, 2)?;
                    sec = v;
                    s = r;
                }
                '%' => s = s.strip_prefix('%')?,
                _ => return None,
            }
        } else {
            s = s.strip_prefix(c)?;
        }
    }
    if !s.is_empty() {
        return None;
    }
    stamp(y?, mo?, d?, h, mi, sec, has_time)
}

/// Excel serial day number (1900 system, or 1904) → date and time.
pub fn excel_serial(v: f64, is_1904: bool) -> Option<Stamp> {
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    let days = v.floor() as i64;
    let mut secs = ((v - v.floor()) * 86400.0).round() as i64;
    let mut days = days;
    if secs >= 86400 {
        secs -= 86400;
        days += 1;
    }
    // Days since 1970-01-01. 1900 system: serial 25569 = 1970-01-01 (the
    // fictitious 1900-02-29 is already inside that offset); 1904: 24107.
    let unix = days - if is_1904 { 24107 } else { 25569 };
    let (y, m, d) = civil(unix);
    stamp(
        y as i32,
        m,
        d,
        (secs / 3600) as u32,
        (secs % 3600 / 60) as u32,
        (secs % 60) as u32,
        secs != 0,
    )
}

/// Days since 1970-01-01 → (year, month, day).
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> String {
        parse_number(s).unwrap()
    }

    #[test]
    fn numbers() {
        assert_eq!(n("1,234.56"), "1234.56");
        assert_eq!(n("1.234,56"), "1234.56");
        assert_eq!(n("1,234,567"), "1234567");
        assert_eq!(n("1.234.567"), "1234567");
        assert_eq!(n("1,5"), "1.5");
        assert_eq!(n("-12.5"), "-12.5");
        assert_eq!(n("(12.5)"), "-12.5");
        assert_eq!(n("+7"), "7");
        assert_eq!(n("1 234"), "1234");
        assert_eq!(n(".5"), "0.5");
        assert_eq!(n("1e3"), "1e3");
        assert_eq!(n("-0"), "0");
        assert!(parse_number("12a").is_err());
        assert!(parse_number("").is_err());
        assert!(parse_number("1,23,4").is_err());
        assert!(parse_number("-").is_err());
        assert!(parse_number("1e").is_err());
    }

    #[test]
    fn booleans() {
        assert!(parse_bool("Yes").unwrap());
        assert!(!parse_bool("N").unwrap());
        assert!(parse_bool("是").unwrap());
        assert!(parse_bool("maybe").is_err());
    }

    #[test]
    fn dates_auto() {
        let a = DateFormat::Auto;
        assert_eq!(
            parse_datetime("2024-03-05", &a).unwrap().date_text(),
            "2024-03-05"
        );
        assert_eq!(
            parse_datetime("2024/3/5", &a).unwrap().date_text(),
            "2024-03-05"
        );
        assert_eq!(
            parse_datetime("2024.03.05 14:07", &a).unwrap().text(),
            "2024-03-05 14:07:00"
        );
        assert_eq!(
            parse_datetime("2024-03-05T14:07:09.123Z", &a)
                .unwrap()
                .text(),
            "2024-03-05 14:07:09"
        );
        assert_eq!(
            parse_datetime("20240305", &a).unwrap().date_text(),
            "2024-03-05"
        );
        assert_eq!(
            parse_datetime("2024-03-05 14:07:09+08:00", &a)
                .unwrap()
                .text(),
            "2024-03-05 14:07:09"
        );
        assert!(parse_datetime("2023-02-29", &a).is_err());
        assert!(parse_datetime("2024-13-01", &a).is_err());
        assert!(parse_datetime("05/03/2024", &a).is_err());
        assert!(parse_datetime("hello", &a).is_err());
    }

    #[test]
    fn dates_custom() {
        let f = DateFormat::Custom("%d/%m/%Y".into());
        assert_eq!(
            parse_datetime("05/03/2024", &f).unwrap().date_text(),
            "2024-03-05"
        );
        let f = DateFormat::Custom("%m/%d/%y %H:%M".into());
        assert_eq!(
            parse_datetime("3/5/24 9:07", &f).unwrap().text(),
            "2024-03-05 09:07:00"
        );
        assert!(parse_datetime("2024-03-05", &f).is_err());
        assert!(parse_datetime("31/04/2024", &DateFormat::Custom("%d/%m/%Y".into())).is_err());
    }

    #[test]
    fn excel_dates() {
        assert_eq!(
            excel_serial(45000.0, false).unwrap().date_text(),
            "2023-03-15"
        );
        assert_eq!(
            excel_serial(25569.5, false).unwrap().text(),
            "1970-01-01 12:00:00"
        );
        assert_eq!(
            excel_serial(43831.0, false).unwrap().date_text(),
            "2020-01-01"
        );
        assert_eq!(
            excel_serial(24107.0, true).unwrap().date_text(),
            "1970-01-01"
        );
    }

    #[test]
    fn kinds() {
        use TargetKind::*;
        assert_eq!(TargetKind::of_type("varchar(191)"), Text);
        assert_eq!(TargetKind::of_type("bigint(20) unsigned"), Integer);
        assert_eq!(TargetKind::of_type("tinyint(1)"), Bool);
        assert_eq!(TargetKind::of_type("tinyint(4)"), Integer);
        assert_eq!(TargetKind::of_type("decimal(10,2)"), Number);
        assert_eq!(TargetKind::of_type("NUMBER(10)"), Integer);
        assert_eq!(TargetKind::of_type("NUMBER(10,2)"), Number);
        assert_eq!(TargetKind::of_type("NUMBER"), Number);
        assert_eq!(TargetKind::of_type("timestamp without time zone"), DateTime);
        assert_eq!(TargetKind::of_type("datetime"), DateTime);
        assert_eq!(TargetKind::of_type("date"), Date);
        assert_eq!(TargetKind::of_type("boolean"), Bool);
        assert_eq!(TargetKind::of_type("double precision"), Number);
        assert_eq!(TargetKind::of_type("integer"), Integer);
    }

    #[test]
    fn convert_cells() {
        let o = ConvertOptions::default();
        assert_eq!(
            convert(" 12 ", TargetKind::Integer, &o).unwrap(),
            Some("12".into())
        );
        assert_eq!(
            convert("12.00", TargetKind::Integer, &o).unwrap(),
            Some("12".into())
        );
        assert!(convert("12.5", TargetKind::Integer, &o).is_err());
        assert_eq!(convert("", TargetKind::Text, &o).unwrap(), None);
        assert_eq!(
            convert(
                "",
                TargetKind::Number,
                &ConvertOptions {
                    empty_as_null: false,
                    ..o.clone()
                }
            )
            .unwrap(),
            None
        );
        assert_eq!(
            convert(
                "",
                TargetKind::Text,
                &ConvertOptions {
                    empty_as_null: false,
                    ..o.clone()
                }
            )
            .unwrap(),
            Some(String::new())
        );
        assert_eq!(
            convert(
                "  x ",
                TargetKind::Text,
                &ConvertOptions {
                    trim: false,
                    ..o.clone()
                }
            )
            .unwrap(),
            Some("  x ".into())
        );
        assert_eq!(
            convert("yes", TargetKind::Bool, &o).unwrap(),
            Some("1".into())
        );
        assert_eq!(
            convert("2024/3/5 1:02", TargetKind::Date, &o).unwrap(),
            Some("2024-03-05".into())
        );
        assert_eq!(
            convert("2024-03-05", TargetKind::DateTime, &o).unwrap(),
            Some("2024-03-05 00:00:00".into())
        );
    }
}
