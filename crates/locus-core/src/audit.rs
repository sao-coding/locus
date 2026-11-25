//! `audit.log` in the app data directory: one JSON object per line for every
//! unlock, re-lock, blocked write and confirmed-and-sent write on a connection.
//! Passwords are never part of an entry (they are not known here).

use serde::Serialize;
use std::io::Write as _;

#[derive(Serialize)]
pub struct Entry<'a> {
    pub time: String,
    pub connection: &'a str,
    pub env: &'a str,
    /// `unlock`, `relock`, `blocked`, `confirmed_sent`.
    pub event: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql: Option<&'a str>,
    /// Who sent it when it was not the user: `mcp`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'a str>,
    /// The MCP client name (from its `initialize`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<&'a str>,
}

impl<'a> Entry<'a> {
    pub fn new(connection: &'a str, env: &'a str, event: &'a str) -> Self {
        Entry {
            time: now_rfc3339(),
            connection,
            env,
            event,
            cause: None,
            duration: None,
            reason: None,
            keywords: Vec::new(),
            sql: None,
            source: None,
            client: None,
        }
    }
}

/// Appends one line. Failures go to stderr; they never stop a run.
pub fn append(e: &Entry) {
    let Some(dir) = crate::settings::app_dir() else {
        return;
    };
    if let Err(err) = write_line(&dir, e) {
        eprintln!("audit.log: {err}");
    }
}

pub(crate) fn write_line(dir: &std::path::Path, e: &Entry) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut line = serde_json::to_string(e).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("audit.log"))?;
    f.write_all(line.as_bytes())
}

/// UTC time as `2026-11-16T03:04:05.123Z`.
pub fn now_rfc3339() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    rfc3339(d.as_secs(), d.subsec_millis())
}

fn rfc3339(secs: u64, ms: u32) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_format() {
        assert_eq!(rfc3339(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(951_782_400, 0), "2000-02-29T00:00:00.000Z");
        assert_eq!(rfc3339(1_000_000_007, 7), "2001-09-09T01:46:47.007Z");
    }

    #[test]
    fn lines_are_json_without_secrets() {
        let dir = std::env::temp_dir().join(format!("wb-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut e = Entry::new("prod-db", "prod", "confirmed_sent");
        e.reason = Some("JIRA-1");
        e.keywords = vec!["DELETE".into()];
        e.sql = Some("DELETE FROM t WHERE id = 1");
        write_line(&dir, &e).unwrap();
        write_line(&dir, &Entry::new("prod-db", "prod", "relock")).unwrap();
        let text = std::fs::read_to_string(dir.join("audit.log")).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["event"], "confirmed_sent");
        assert_eq!(v["sql"], "DELETE FROM t WHERE id = 1");
        assert_eq!(v["keywords"][0], "DELETE");
        assert!(!lines[0].to_lowercase().contains("password"));
        let v2: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert!(v2.get("sql").is_none() && v2.get("reason").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
