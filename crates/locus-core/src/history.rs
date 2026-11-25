//! Query history per connection: statement, time, duration, rows or error.
//! `%APPDATA%\Locus\history.json`, at most 1,000 entries per
//! connection, never result data. The app wraps them in a shared
//! entity (`app::history::History`) that the History tab shows.

use crate::settings;
use rust_i18n::t;
use serde::{Deserialize, Serialize};

pub const CAP_PER_CONNECTION: usize = 1000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub conn_id: String,
    pub sql: String,
    /// Unix seconds when the run started.
    pub at: u64,
    pub ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Values bound to the placeholders of `sql` (which is stored as the user
    /// wrote it). Empty for a plain statement and for entries from older files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<HistParam>,
    /// Sent by this MCP client (its name), not typed in a tab: an MCP badge in History.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_client: Option<String>,
}

/// One bound value of a statement with placeholders.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistParam {
    pub name: String,
    /// `text`, `number`, `date` or `null` (same as the Run dialog's type button).
    pub ty: String,
    pub value: String,
}

impl HistParam {
    pub fn from_value(name: &str, v: &crate::params::ParamValue) -> Self {
        use crate::params::ParamType::*;
        let ty = match v.ty {
            Text => "text",
            Number => "number",
            Date => "date",
            Null => "null",
        };
        HistParam {
            name: name.to_string(),
            ty: ty.into(),
            value: v.text.clone(),
        }
    }

    pub fn to_value(&self) -> crate::params::ParamValue {
        use crate::params::ParamType::*;
        let ty = match self.ty.as_str() {
            "number" => Number,
            "date" => Date,
            "null" => Null,
            _ => Text,
        };
        crate::params::ParamValue {
            ty,
            text: self.value.clone(),
        }
    }

    /// Whether the value should not show in the list: a secret-like name, or a
    /// long token-like value.
    pub fn looks_secret(&self) -> bool {
        let n = self.name.to_lowercase();
        if [
            "pass",
            "pwd",
            "secret",
            "token",
            "key",
            "auth",
            "credential",
        ]
        .iter()
        .any(|w| n.contains(w))
        {
            return true;
        }
        let v = self.value.trim();
        v.chars().count() >= 24
            && !v.contains(char::is_whitespace)
            && v.chars().any(|c| c.is_ascii_digit())
            && v.chars().any(|c| c.is_alphabetic())
    }

    /// `name=value` for a list row; a secret shows as `name=••••`, a long value is cut.
    pub fn masked(&self) -> String {
        if self.ty == "null" {
            return format!("{}=NULL", self.name);
        }
        if self.looks_secret() {
            return format!("{}=••••", self.name);
        }
        let v: String = self.value.chars().take(24).collect();
        let cut = if self.value.chars().count() > 24 {
            "…"
        } else {
            ""
        };
        format!("{}={v}{cut}", self.name)
    }
}

/// `a=1, b=••••` for the list row ("" without parameters).
pub fn params_summary(p: &[HistParam]) -> String {
    p.iter()
        .map(HistParam::masked)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The values as the Run dialog keeps them (name to value).
pub fn params_map(p: &[HistParam]) -> std::collections::HashMap<String, crate::params::ParamValue> {
    p.iter().map(|x| (x.name.clone(), x.to_value())).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Failed,
    Cancelled,
}

impl Entry {
    /// A cancelled run is stored as an error whose text is the localized
    /// "Cancelled" (either language: the file outlives a language switch).
    pub fn status(&self) -> Status {
        match &self.error {
            None => Status::Ok,
            Some(e) if is_cancel_text(e) => Status::Cancelled,
            Some(_) => Status::Failed,
        }
    }
}

/// The error text that marks a cancelled run (in the current language).
pub fn cancelled_text() -> String {
    t!("state.cancelled").into_owned()
}

fn is_cancel_text(s: &str) -> bool {
    crate::settings::LANGUAGES
        .iter()
        .any(|l| s == t!("state.cancelled", locale = *l))
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Keeps the newest `CAP_PER_CONNECTION` entries of each connection.
pub fn push_capped(list: &mut Vec<Entry>, e: Entry) {
    list.push(e);
    let conn = list.last().unwrap().conn_id.clone();
    let n = list.iter().filter(|x| x.conn_id == conn).count();
    if n > CAP_PER_CONNECTION {
        let mut drop = n - CAP_PER_CONNECTION;
        list.retain(|x| {
            if drop > 0 && x.conn_id == conn {
                drop -= 1;
                false
            } else {
                true
            }
        });
    }
}

/// Reads `history.json` (empty when it is missing or unreadable).
pub fn load() -> Vec<Entry> {
    crate::datafile::load_named("history.json").unwrap_or_default()
}

/// Writes `history.json` (a JSON array of entries); the next added entry
/// writes it again, so the caller only logs a failure.
pub fn save(snapshot: &[u8]) -> std::io::Result<()> {
    let d = settings::app_dir().ok_or_else(|| std::io::Error::other("APPDATA is not set"))?;
    std::fs::create_dir_all(&d)?;
    settings::write_atomic(&d.join("history.json"), snapshot)
}

// ---- filtering (pure, runs off the render path) ---------------------------------

/// What the History tab filters on, precomputed once per change of the store
/// so a keystroke only runs a substring test per row.
#[derive(Clone, Debug)]
pub struct Row {
    /// Index into `History::entries`.
    pub idx: u32,
    pub conn: std::sync::Arc<str>,
    pub status: Status,
    /// Local day number (days since 1970-01-01 in local time).
    pub day: i64,
    /// Lower-cased SQL.
    pub lower: Box<str>,
}

/// Rows newest first.
pub fn build_rows(entries: &[Entry], tz: &TimeZone) -> Vec<Row> {
    let mut conns: Vec<std::sync::Arc<str>> = Vec::new();
    let mut out = Vec::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate().rev() {
        let conn = match conns.iter().find(|c| &***c == e.conn_id.as_str()) {
            Some(c) => c.clone(),
            None => {
                let c: std::sync::Arc<str> = e.conn_id.as_str().into();
                conns.push(c.clone());
                c
            }
        };
        out.push(Row {
            idx: i as u32,
            conn,
            status: e.status(),
            day: day_in(e.at, tz),
            lower: e.sql.to_lowercase().into_boxed_str(),
        });
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    /// Typed text (any case).
    pub text: String,
    pub conn: Option<String>,
    pub status: Option<Status>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    /// Day heading (local day number).
    Day(i64),
    /// An entry (index into `History::entries`).
    Entry(u32),
}

/// Rows passing the filter, grouped by day: a `Day` heading before the first
/// entry of each day. Returns the items and the number of entries.
pub fn filter_items(rows: &[Row], f: &Filter) -> (Vec<Item>, usize) {
    let q = f.text.trim().to_lowercase();
    let mut items = Vec::new();
    let mut n = 0;
    let mut last_day = None;
    for r in rows {
        if let Some(c) = &f.conn
            && &*r.conn != c.as_str()
        {
            continue;
        }
        if let Some(s) = f.status
            && r.status != s
        {
            continue;
        }
        if !q.is_empty() && !r.lower.contains(q.as_str()) {
            continue;
        }
        if last_day != Some(r.day) {
            items.push(Item::Day(r.day));
            last_day = Some(r.day);
        }
        items.push(Item::Entry(r.idx));
        n += 1;
    }
    (items, n)
}

// ---- time ---------------------------------------------------------------------------

pub use jiff::tz::TimeZone;
use jiff::{Timestamp, civil::Date};

/// The system time zone (daylight saving rules included).
pub fn local_tz() -> TimeZone {
    TimeZone::system()
}

fn zoned(secs: u64, tz: &TimeZone) -> jiff::ZonedDateTime {
    Timestamp::from_second(secs as i64)
        .unwrap_or(Timestamp::UNIX_EPOCH)
        .to_zoned_datetime(tz.clone())
}

/// Offset from UTC in seconds that `tz` has at the instant `secs`.
pub fn offset_secs_at(secs: u64, tz: &TimeZone) -> i64 {
    zoned(secs, tz).offset().seconds() as i64
}

/// Local day number (days since 1970-01-01 of the local calendar date) of
/// the instant `secs` in `tz`.
pub fn day_in(secs: u64, tz: &TimeZone) -> i64 {
    (secs as i64 + offset_secs_at(secs, tz)).div_euclid(86_400)
}

/// `hh:mm` local.
pub fn format_clock(secs: u64) -> String {
    format_clock_in(secs, &local_tz())
}

pub fn format_clock_in(secs: u64, tz: &TimeZone) -> String {
    zoned(secs, tz).strftime("%H:%M").to_string()
}

/// `yyyy-mm-dd hh:mm:ss` local.
pub fn format_time(secs: u64) -> String {
    format_time_in(secs, &local_tz())
}

pub fn format_time_in(secs: u64, tz: &TimeZone) -> String {
    zoned(secs, tz).strftime("%Y-%m-%d %H:%M:%S").to_string()
}

fn date_of(day: i64) -> Date {
    let epoch = Date::constant(1970, 1, 1);
    i32::try_from(day)
        .ok()
        .and_then(|d| epoch.checked_add(jiff::Span::new().days(d)).ok())
        .unwrap_or(epoch)
}

/// `yyyy-mm-dd` of a local day number.
pub fn format_day(day: i64) -> String {
    date_of(day).strftime("%Y-%m-%d").to_string()
}

/// 0 = Sunday.
pub fn weekday(day: i64) -> usize {
    date_of(day).weekday().to_sunday_zero_offset() as usize
}

pub fn today() -> i64 {
    day_in(now_secs(), &local_tz())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(conn: &str, sql: &str, at: u64, error: Option<&str>) -> Entry {
        Entry {
            conn_id: conn.into(),
            sql: sql.into(),
            at,
            ms: 1,
            rows: error.is_none().then_some(1),
            error: error.map(String::from),
            params: Vec::new(),
            mcp_client: None,
        }
    }

    #[::core::prelude::v1::test]
    fn params_round_trip_and_old_entries() {
        use crate::params::{ParamType, ParamValue};
        let mut en = e(
            "a",
            "SELECT * FROM t WHERE id = :id AND tok = :api_token",
            1,
            None,
        );
        en.params = vec![
            HistParam::from_value(
                "id",
                &ParamValue {
                    ty: ParamType::Number,
                    text: "7".into(),
                },
            ),
            HistParam::from_value(
                "api_token",
                &ParamValue {
                    ty: ParamType::Text,
                    text: "abc".into(),
                },
            ),
        ];
        let json = serde_json::to_vec(&vec![en.clone()]).unwrap();
        let back: Vec<Entry> = serde_json::from_slice(&json).unwrap();
        assert_eq!(back[0].sql, en.sql);
        assert_eq!(back[0].params, en.params);
        let m = params_map(&back[0].params);
        assert_eq!(
            m["id"],
            ParamValue {
                ty: ParamType::Number,
                text: "7".into()
            }
        );
        assert_eq!(params_summary(&back[0].params), "id=7, api_token=••••");
        // Old file: no `params` key.
        let old = br#"[{"conn_id":"a","sql":"select 1","at":1,"ms":2,"rows":1}]"#;
        let v: Vec<Entry> = serde_json::from_slice(old).unwrap();
        assert!(v[0].params.is_empty());
        // A plain entry does not write the key.
        assert!(
            !String::from_utf8(serde_json::to_vec(&e("a", "x", 0, None)).unwrap())
                .unwrap()
                .contains("params")
        );
        let long = HistParam {
            name: "x".into(),
            ty: "text".into(),
            value: "a1b2c3d4e5f6a7b8c9d0e1f2a3".into(),
        };
        assert!(long.looks_secret());
        assert_eq!(
            HistParam {
                name: "d".into(),
                ty: "null".into(),
                value: String::new()
            }
            .masked(),
            "d=NULL"
        );
    }

    #[::core::prelude::v1::test]
    fn cap_per_connection() {
        let mut v = Vec::new();
        for i in 0..1005u64 {
            push_capped(&mut v, e("a", &format!("{i}"), i, None));
        }
        push_capped(&mut v, e("b", "x", 0, None));
        assert_eq!(v.iter().filter(|e| e.conn_id == "a").count(), 1000);
        assert_eq!(v[0].sql, "5");
        assert_eq!(v.len(), 1001);
        assert_eq!(format_day(0), "1970-01-01");
        assert_eq!(format_day(20_407), "2025-11-15");
        assert_eq!(weekday(0), 4); // 1970-01-01 was a Thursday
    }

    #[::core::prelude::v1::test]
    fn status_of_entries() {
        assert_eq!(e("a", "x", 0, None).status(), Status::Ok);
        assert_eq!(
            e("a", "x", 0, Some("[1146] no such table")).status(),
            Status::Failed
        );
        assert_eq!(
            e("a", "x", 0, Some("Cancelled")).status(),
            Status::Cancelled
        );
        assert_eq!(e("a", "x", 0, Some("已取消")).status(), Status::Cancelled);
    }

    fn sample() -> Vec<Entry> {
        vec![
            e("a", "SELECT * FROM users", 100, None),
            e("b", "select 1", 200, None),
            e("a", "DELETE FROM users", 300, Some("[1142] denied")),
            e("a", "SELECT sleep(9)", 86_400 * 3 + 10, Some("Cancelled")),
            e(
                "b",
                "SELECT name FROM Users WHERE id = 1",
                86_400 * 3 + 20,
                None,
            ),
        ]
    }

    #[::core::prelude::v1::test]
    fn filter_text_conn_status_and_grouping() {
        let rows = build_rows(&sample(), &TimeZone::UTC);
        // Newest first.
        assert_eq!(rows[0].idx, 4);
        let (items, n) = filter_items(&rows, &Filter::default());
        assert_eq!(n, 5);
        assert_eq!(items[0], Item::Day(3));
        assert_eq!(
            items.iter().filter(|i| matches!(i, Item::Day(_))).count(),
            2
        );
        assert_eq!(items[items.len() - 1], Item::Entry(0));
        // Text is case-insensitive over the SQL.
        let (items, n) = filter_items(
            &rows,
            &Filter {
                text: " USERS ".into(),
                ..Default::default()
            },
        );
        assert_eq!(n, 3);
        assert!(
            items.contains(&Item::Entry(4))
                && items.contains(&Item::Entry(0))
                && items.contains(&Item::Entry(2))
        );
        // Connection and status combine.
        let f = Filter {
            conn: Some("a".into()),
            status: Some(Status::Failed),
            ..Default::default()
        };
        let (items, n) = filter_items(&rows, &f);
        assert_eq!((n, items), (1, vec![Item::Day(0), Item::Entry(2)]));
        let f = Filter {
            status: Some(Status::Cancelled),
            ..Default::default()
        };
        assert_eq!(filter_items(&rows, &f).1, 1);
        let f = Filter {
            text: "nothing like it".into(),
            ..Default::default()
        };
        assert_eq!(filter_items(&rows, &f), (vec![], 0));
    }

    /// Test helper for the smoke run: `LOCUS_DATA_DIR=<scratch> cargo test
    /// --release -p app generate_history_fixture -- --ignored` writes 1,000
    /// entries for each of the connections `bench`, `bench-2` and `gone` (a
    /// removed connection), spread over 12 days, about one in nine failed and
    /// one in twenty cancelled.
    #[::core::prelude::v1::test]
    #[ignore]
    fn generate_history_fixture() {
        // Only ever into an explicit test folder: app_dir() falls back to the real
        // %APPDATA%\Locus, whose history this would overwrite.
        let Some(dir) = std::env::var_os("LOCUS_DATA_DIR").map(std::path::PathBuf::from) else {
            panic!(
                "set LOCUS_DATA_DIR to a throwaway folder; this test overwrites history.json there"
            )
        };
        let now = now_secs();
        let tables = ["student_real", "wb_edit_comp", "orders", "users", "wb_r_a"];
        let mut v = Vec::new();
        for i in 0..CAP_PER_CONNECTION as u64 {
            for (c, conn) in ["bench", "bench-2", "gone"].into_iter().enumerate() {
                let n = i * 3 + c as u64;
                let t = tables[(n % 5) as usize];
                let sql = match n % 4 {
                    0 => format!("SELECT * FROM {t} WHERE id = {n}"),
                    1 => format!(
                        "SELECT COUNT(*) FROM {t} GROUP BY grade ORDER BY 1 LIMIT {}",
                        n % 50 + 1
                    ),
                    2 => format!("UPDATE {t} SET note = 'row {n}' WHERE id = {n}"),
                    _ => format!(
                        "SELECT a.*, b.name\nFROM {t} a\nLEFT JOIN users b ON b.id = a.id\nWHERE a.id > {n} -- run {n}"
                    ),
                };
                let error = if n % 9 == 4 {
                    Some(format!("[1146] Table 'bench.{t}_old' doesn't exist"))
                } else if n % 20 == 7 {
                    Some("Cancelled".to_string())
                } else {
                    None
                };
                v.push(Entry {
                    conn_id: conn.into(),
                    sql,
                    at: now - (n * 12 * 86_400 / 3000),
                    ms: 3 + n * 7 % 4000,
                    rows: error.is_none().then_some(n * 13 % 5000),
                    error,
                    params: Vec::new(),
                    mcp_client: None,
                });
            }
        }
        v.sort_by_key(|e| e.at);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("history.json"), serde_json::to_vec(&v).unwrap()).unwrap();
    }
}

#[cfg(test)]
mod dst_tests {
    use super::*;

    fn ts(s: &str) -> u64 {
        s.parse::<jiff::Timestamp>().unwrap().as_second() as u64
    }

    /// New York switches to daylight time on 2025-03-09; 04:30 UTC on the
    /// 10th is 00:30 EDT (a fixed standard-time offset would say 23:30 on
    /// the 9th).
    #[::core::prelude::v1::test]
    fn dst_instant_groups_into_the_right_local_day() {
        let ny = TimeZone::get("America/New_York").unwrap();
        let at = ts("2025-03-10T04:30:00Z");
        assert_eq!(offset_secs_at(at, &ny), -4 * 3600);
        assert_eq!(format_day(day_in(at, &ny)), "2025-03-10");
        assert_eq!(format_time_in(at, &ny), "2025-03-10 00:30:00");
        assert_eq!(format_clock_in(at, &ny), "00:30");
        // Same clock time in winter is on the 9th: offset is -5 there.
        let winter = ts("2025-01-10T04:30:00Z");
        assert_eq!(offset_secs_at(winter, &ny), -5 * 3600);
        assert_eq!(format_day(day_in(winter, &ny)), "2025-01-09");
    }
}
