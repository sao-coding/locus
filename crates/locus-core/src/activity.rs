//! Server activity, pure part: SQL per engine for the session list and the
//! server variables / status, row models, filter and sort, and the kill
//! statements (with the quoting rules). No UI, no connection here except
//! `fetch`, which only reads.

use crate::driver::AnyDriver;
use crate::store::DbKind;
use db_core::{Cell, Driver as _, QueryEvent};

/// A session older than this while running a statement is shown as long-running.
pub const LONG_SECS: u64 = 60;
/// Rows the session list asks the server for.
pub const SESSION_CAP: usize = 1000;
/// Rows of the variables / status lists (Oracle).
pub const LIST_CAP: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Engine {
    /// MySQL and MariaDB.
    Mysql,
    Postgres,
    Oracle,
    Redis,
}

impl Engine {
    /// `None` for SQLite: it has no server and no sessions.
    pub fn of(kind: DbKind) -> Option<Engine> {
        match kind {
            DbKind::Mysql => Some(Engine::Mysql),
            DbKind::Postgres => Some(Engine::Postgres),
            DbKind::Oracle => Some(Engine::Oracle),
            DbKind::Redis => Some(Engine::Redis),
            DbKind::Sqlite => None,
        }
    }

    /// Whether "kill query" (stop the statement, keep the session) exists.
    pub fn can_cancel(self) -> bool {
        matches!(self, Engine::Mysql | Engine::Postgres)
    }
}

/// One server session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    /// Oracle `SERIAL#`.
    pub serial: Option<String>,
    pub user: String,
    pub host: String,
    pub db: String,
    pub state: String,
    /// Seconds in the current state (running time of the statement when active).
    pub secs: Option<u64>,
    pub sql: String,
    /// The session that ran the list query itself.
    pub mine: bool,
    /// A statement is running now.
    pub active: bool,
}

impl Session {
    pub fn long_running(&self) -> bool {
        self.active && self.secs.is_some_and(|s| s >= LONG_SECS)
    }

    /// What dialogs call the session: `42`, Oracle `42,1017`.
    pub fn label(&self) -> String {
        match &self.serial {
            Some(s) => format!("{},{}", self.id, s),
            None => self.id.clone(),
        }
    }

    fn haystack_has(&self, needle_lower: &str) -> bool {
        [
            &self.id,
            &self.user,
            &self.host,
            &self.db,
            &self.state,
            &self.sql,
        ]
        .iter()
        .any(|f| f.to_lowercase().contains(needle_lower))
    }
}

// ---- SQL per engine ------------------------------------------------------------

/// The session list. Columns, in order: id, serial, user, host, db, state,
/// seconds, sql, mine. Redis has no SQL (`CLIENT LIST`, see `parse_client_list`).
pub fn sessions_sql(e: Engine) -> Option<String> {
    Some(match e {
        Engine::Mysql => format!(
            "SELECT CAST(ID AS CHAR), NULL, USER, HOST, DB, CONCAT_WS(' / ', COMMAND, NULLIF(STATE, '')), CAST(TIME AS CHAR), INFO, CAST(ID = CONNECTION_ID() AS CHAR) \
             FROM information_schema.PROCESSLIST ORDER BY ID LIMIT {SESSION_CAP}"
        ),
        Engine::Postgres => format!(
            "SELECT pid::text, NULL::text, usename::text, COALESCE(client_addr::text, ''), COALESCE(datname::text, ''), COALESCE(state, '')::text, \
             COALESCE(EXTRACT(EPOCH FROM (now() - CASE WHEN state = 'active' THEN query_start ELSE state_change END))::bigint, 0)::text, \
             COALESCE(query, ''), (pid = pg_backend_pid())::text \
             FROM pg_stat_activity WHERE backend_type = 'client backend' ORDER BY pid LIMIT {SESSION_CAP}"
        ),
        Engine::Oracle => format!(
            "SELECT * FROM (SELECT TO_CHAR(s.sid), TO_CHAR(s.serial#), s.username, s.machine, s.schemaname, s.status, TO_CHAR(s.last_call_et), q.sql_text, \
             CASE WHEN s.sid = TO_NUMBER(SYS_CONTEXT('USERENV', 'SID')) THEN '1' ELSE '0' END \
             FROM v$session s LEFT JOIN v$sql q ON q.sql_id = s.sql_id AND q.child_number = s.sql_child_number \
             WHERE s.type = 'USER' ORDER BY s.sid) WHERE ROWNUM <= {SESSION_CAP}"
        ),
        Engine::Redis => return None,
    })
}

/// Variables as `name, value` rows (Redis: `CONFIG GET *`, see the app).
pub fn variables_sql(e: Engine) -> Option<String> {
    Some(match e {
        Engine::Mysql => "SHOW GLOBAL VARIABLES".to_string(),
        Engine::Postgres => {
            "SELECT name, setting || COALESCE(' ' || unit, '') FROM pg_settings ORDER BY name"
                .to_string()
        }
        Engine::Oracle => format!(
            "SELECT * FROM (SELECT name, value FROM v$parameter ORDER BY name) WHERE ROWNUM <= {LIST_CAP}"
        ),
        Engine::Redis => return None,
    })
}

/// Status counters as `name, value` rows (Redis: `INFO`, see the app).
pub fn status_sql(e: Engine) -> Option<String> {
    Some(match e {
        Engine::Mysql => "SHOW GLOBAL STATUS".to_string(),
        Engine::Postgres => "SELECT d.datname || '.' || x.k, x.v FROM pg_stat_database d CROSS JOIN LATERAL (VALUES \
             ('numbackends', d.numbackends::text), ('xact_commit', d.xact_commit::text), ('xact_rollback', d.xact_rollback::text), \
             ('blks_read', d.blks_read::text), ('blks_hit', d.blks_hit::text), ('tup_returned', d.tup_returned::text), \
             ('tup_fetched', d.tup_fetched::text), ('tup_inserted', d.tup_inserted::text), ('tup_updated', d.tup_updated::text), \
             ('tup_deleted', d.tup_deleted::text), ('conflicts', d.conflicts::text), ('temp_files', d.temp_files::text), \
             ('temp_bytes', d.temp_bytes::text), ('deadlocks', d.deadlocks::text)) x(k, v) \
             WHERE d.datname IS NOT NULL ORDER BY 1"
            .to_string(),
        Engine::Oracle => format!("SELECT * FROM (SELECT name, TO_CHAR(value) FROM v$sysstat ORDER BY name) WHERE ROWNUM <= {LIST_CAP}"),
        Engine::Redis => return None,
    })
}

// ---- reading -------------------------------------------------------------------

pub type Rows = Vec<Vec<Option<String>>>;

/// Runs a read-only statement and returns every cell as text (NULL = `None`).
pub async fn fetch(driver: &AnyDriver, sql: String) -> Result<Rows, String> {
    let mut h = driver.query(sql, Default::default());
    let mut out: Rows = Vec::new();
    let mut err = None;
    while let Some(ev) = h.next().await {
        match ev {
            QueryEvent::Batch(b) => {
                for r in 0..b.rows() {
                    out.push(
                        (0..b.columns())
                            .map(|c| match b.cell(r, c) {
                                Cell::Null => None,
                                Cell::Text(s) => Some(s.to_string()),
                                Cell::Bytes(x) => Some(String::from_utf8_lossy(x).into_owned()),
                            })
                            .collect(),
                    );
                }
            }
            QueryEvent::Finished(db_core::Outcome::Failed(e)) => err = Some(e.message),
            _ => {}
        }
    }
    match err {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

fn cell(row: &[Option<String>], i: usize) -> String {
    row.get(i).cloned().flatten().unwrap_or_default()
}

fn truthy(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "t" | "true")
}

/// Rows of `sessions_sql` into sessions.
pub fn parse_sessions(e: Engine, rows: &Rows) -> Vec<Session> {
    rows.iter()
        .map(|r| {
            let state = cell(r, 5);
            let secs = cell(r, 6).trim().parse::<u64>().ok();
            let active = match e {
                Engine::Mysql => {
                    state.to_ascii_lowercase().starts_with("query")
                        || state.to_ascii_lowercase().starts_with("execute")
                }
                Engine::Postgres => state == "active",
                Engine::Oracle => state == "ACTIVE",
                Engine::Redis => false,
            };
            Session {
                id: cell(r, 0),
                serial: r.get(1).cloned().flatten().filter(|s| !s.is_empty()),
                user: cell(r, 2),
                host: cell(r, 3),
                db: cell(r, 4),
                state,
                secs,
                sql: cell(r, 7),
                mine: truthy(&cell(r, 8)),
                active,
            }
        })
        .collect()
}

/// `CLIENT LIST` text of Redis into sessions. The client that ran `CLIENT LIST`
/// (its `cmd` is `client|list`) is marked as ours.
pub fn parse_client_list(text: &str) -> Vec<Session> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let get = |k: &str| {
                line.split(' ')
                    .find_map(|kv| kv.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
                    .unwrap_or("")
                    .to_string()
            };
            let cmd = get("cmd");
            Session {
                id: get("id"),
                serial: None,
                user: get("user"),
                host: get("addr"),
                db: get("db"),
                state: format!("flags={}", get("flags")),
                secs: get("idle").parse().ok(),
                sql: cmd.clone(),
                mine: cmd.eq_ignore_ascii_case("client|list") || cmd.eq_ignore_ascii_case("client"),
                active: false,
            }
        })
        .collect()
}

/// `name, value` rows (first two columns).
pub fn parse_pairs(rows: &Rows) -> Vec<(String, String)> {
    rows.iter().map(|r| (cell(r, 0), cell(r, 1))).collect()
}

// ---- filter and sort -----------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Id,
    User,
    Host,
    Db,
    State,
    Time,
    Sql,
}

/// Indices of `sessions` that match `filter` (case-insensitive, any column), in
/// the order of `key` (`None` keeps the server order).
pub fn view(sessions: &[Session], filter: &str, sort: Option<(SortKey, bool)>) -> Vec<usize> {
    let needle = filter.trim().to_lowercase();
    let mut ix: Vec<usize> = (0..sessions.len())
        .filter(|&i| needle.is_empty() || sessions[i].haystack_has(&needle))
        .collect();
    if let Some((key, desc)) = sort {
        let num = |s: &str| s.parse::<u64>().unwrap_or(0);
        ix.sort_by(|&a, &b| {
            let (x, y) = (&sessions[a], &sessions[b]);
            let o = match key {
                SortKey::Id => num(&x.id).cmp(&num(&y.id)).then_with(|| x.id.cmp(&y.id)),
                SortKey::User => x.user.to_lowercase().cmp(&y.user.to_lowercase()),
                SortKey::Host => x.host.cmp(&y.host),
                SortKey::Db => x.db.to_lowercase().cmp(&y.db.to_lowercase()),
                SortKey::State => x.state.to_lowercase().cmp(&y.state.to_lowercase()),
                SortKey::Time => x.secs.cmp(&y.secs),
                SortKey::Sql => x.sql.to_lowercase().cmp(&y.sql.to_lowercase()),
            };
            if desc { o.reverse() } else { o }
        });
    }
    ix
}

/// Name / value rows matching `filter` in either column.
pub fn filter_pairs(rows: &[(String, String)], filter: &str) -> Vec<usize> {
    let needle = filter.trim().to_lowercase();
    (0..rows.len())
        .filter(|&i| {
            needle.is_empty()
                || rows[i].0.to_lowercase().contains(&needle)
                || rows[i].1.to_lowercase().contains(&needle)
        })
        .collect()
}

/// `45s`, `3m 05s`, `2h 03m`, `1d 02h`.
pub fn fmt_secs(s: u64) -> String {
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        3600..=86399 => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
        _ => format!("{}d {:02}h", s / 86400, s % 86400 / 3600),
    }
}

/// First `max` characters of the statement on one line, `…` when cut.
pub fn one_line(sql: &str, max: usize) -> String {
    let flat: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

// ---- kill ----------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillMode {
    /// Stop the running statement, keep the session.
    Query,
    /// End the session.
    Connection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KillCmd {
    /// A statement for the server.
    Sql(String),
    /// A Redis command (arguments).
    Redis(Vec<String>),
}

impl KillCmd {
    /// The text shown in dialogs, the audit log and the clipboard.
    pub fn text(&self) -> String {
        match self {
            KillCmd::Sql(s) => s.clone(),
            KillCmd::Redis(a) => a.join(" "),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KillError {
    /// The session is the one the page itself uses.
    Own,
    /// The id is not a number (never spliced into SQL).
    BadId,
    /// The engine has no such action.
    Unsupported,
}

fn num(s: &str) -> Result<u64, KillError> {
    let s = s.trim();
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(KillError::BadId);
    }
    s.parse().map_err(|_| KillError::BadId)
}

/// The statement that kills `s`. Ids are parsed as numbers, so nothing the
/// server sent can end up as SQL text.
pub fn kill_cmd(e: Engine, s: &Session, mode: KillMode) -> Result<KillCmd, KillError> {
    if s.mine {
        return Err(KillError::Own);
    }
    let id = num(&s.id)?;
    match (e, mode) {
        (Engine::Mysql, KillMode::Query) => Ok(KillCmd::Sql(format!("KILL QUERY {id}"))),
        (Engine::Mysql, KillMode::Connection) => Ok(KillCmd::Sql(format!("KILL {id}"))),
        (Engine::Postgres, KillMode::Query) => {
            Ok(KillCmd::Sql(format!("SELECT pg_cancel_backend({id})")))
        }
        (Engine::Postgres, KillMode::Connection) => {
            Ok(KillCmd::Sql(format!("SELECT pg_terminate_backend({id})")))
        }
        (Engine::Oracle, KillMode::Connection) => {
            let serial = num(s.serial.as_deref().unwrap_or(""))?;
            Ok(KillCmd::Sql(format!(
                "ALTER SYSTEM KILL SESSION '{id},{serial}' IMMEDIATE"
            )))
        }
        (Engine::Redis, KillMode::Connection) => Ok(KillCmd::Redis(vec![
            "CLIENT".into(),
            "KILL".into(),
            "ID".into(),
            id.to_string(),
        ])),
        _ => Err(KillError::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str) -> Session {
        Session {
            id: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn kill_statements() {
        assert_eq!(
            kill_cmd(Engine::Mysql, &s("42"), KillMode::Query)
                .unwrap()
                .text(),
            "KILL QUERY 42"
        );
        assert_eq!(
            kill_cmd(Engine::Mysql, &s("42"), KillMode::Connection)
                .unwrap()
                .text(),
            "KILL 42"
        );
        assert_eq!(
            kill_cmd(Engine::Postgres, &s("7"), KillMode::Query)
                .unwrap()
                .text(),
            "SELECT pg_cancel_backend(7)"
        );
        assert_eq!(
            kill_cmd(Engine::Postgres, &s("7"), KillMode::Connection)
                .unwrap()
                .text(),
            "SELECT pg_terminate_backend(7)"
        );
        let mut o = s("12");
        o.serial = Some("1017".into());
        assert_eq!(
            kill_cmd(Engine::Oracle, &o, KillMode::Connection)
                .unwrap()
                .text(),
            "ALTER SYSTEM KILL SESSION '12,1017' IMMEDIATE"
        );
        assert_eq!(
            kill_cmd(Engine::Oracle, &o, KillMode::Query),
            Err(KillError::Unsupported)
        );
        assert_eq!(
            kill_cmd(Engine::Redis, &s("3"), KillMode::Connection)
                .unwrap()
                .text(),
            "CLIENT KILL ID 3"
        );
    }

    #[test]
    fn kill_refuses_own_and_injection() {
        let mut me = s("5");
        me.mine = true;
        assert_eq!(
            kill_cmd(Engine::Mysql, &me, KillMode::Query),
            Err(KillError::Own)
        );
        assert_eq!(
            kill_cmd(Engine::Mysql, &s("5; DROP TABLE x"), KillMode::Query),
            Err(KillError::BadId)
        );
        assert_eq!(
            kill_cmd(Engine::Mysql, &s("-1"), KillMode::Query),
            Err(KillError::BadId)
        );
        let mut o = s("1");
        o.serial = Some("2' OR '1'='1".into());
        assert_eq!(
            kill_cmd(Engine::Oracle, &o, KillMode::Connection),
            Err(KillError::BadId)
        );
        o.serial = None;
        assert_eq!(
            kill_cmd(Engine::Oracle, &o, KillMode::Connection),
            Err(KillError::BadId)
        );
    }

    #[test]
    fn sessions_sql_is_bounded_and_read_only() {
        for e in [Engine::Mysql, Engine::Postgres, Engine::Oracle] {
            let q = sessions_sql(e).unwrap();
            assert!(q.starts_with("SELECT"), "{q}");
            assert!(q.contains("1000"), "{q}");
            assert!(!db_core::classify::classify(&q).writes, "{q}");
            assert!(!db_core::classify::classify(&variables_sql(e).unwrap()).writes);
            assert!(!db_core::classify::classify(&status_sql(e).unwrap()).writes);
        }
        assert!(
            sessions_sql(Engine::Oracle)
                .unwrap()
                .contains("ROWNUM <= 1000")
        );
        assert!(variables_sql(Engine::Oracle).unwrap().contains("ROWNUM"));
        assert!(sessions_sql(Engine::Redis).is_none());
        assert_eq!(Engine::of(DbKind::Sqlite), None);
    }

    fn row(v: &[&str]) -> Vec<Option<String>> {
        v.iter()
            .map(|x| {
                if x.is_empty() {
                    None
                } else {
                    Some(x.to_string())
                }
            })
            .collect()
    }

    #[test]
    fn parse_mysql_rows() {
        let rows = vec![
            row(&[
                "9",
                "",
                "root",
                "h:1",
                "",
                "Query",
                "75",
                "SELECT SLEEP(99)",
                "0",
            ]),
            row(&["10", "", "root", "h:2", "app", "Sleep", "3", "", "1"]),
        ];
        let v = parse_sessions(Engine::Mysql, &rows);
        assert_eq!(v[0].secs, Some(75));
        assert!(v[0].active && v[0].long_running() && !v[0].mine);
        assert!(v[1].mine && !v[1].active && !v[1].long_running());
        assert_eq!(v[1].db, "app");
        assert_eq!(v[0].serial, None);
    }

    #[test]
    fn parse_oracle_and_postgres_rows() {
        let o = vec![row(&[
            "12",
            "1017",
            "SCOTT",
            "pc",
            "SCOTT",
            "ACTIVE",
            "5",
            "select 1 from dual",
            "0",
        ])];
        let v = parse_sessions(Engine::Oracle, &o);
        assert_eq!(
            (
                v[0].label().as_str(),
                v[0].active,
                v[0].secs,
                v[0].sql.as_str()
            ),
            ("12,1017", true, Some(5), "select 1 from dual")
        );
        let p = vec![row(&[
            "77",
            "",
            "me",
            "10.0.0.1",
            "db",
            "active",
            "120",
            "select pg_sleep(200)",
            "true",
        ])];
        let v = parse_sessions(Engine::Postgres, &p);
        assert!(v[0].mine && v[0].long_running());
    }

    #[test]
    fn redis_client_list() {
        let t = "id=3 addr=127.0.0.1:5000 laddr=x fd=8 name= age=5 idle=2 flags=N db=0 cmd=client|list user=default\nid=4 addr=10.0.0.2:6000 age=9 idle=9 flags=N db=1 cmd=get user=app\n";
        let v = parse_client_list(t);
        assert_eq!(v.len(), 2);
        assert!(v[0].mine && !v[1].mine);
        assert_eq!(
            (
                v[1].host.as_str(),
                v[1].db.as_str(),
                v[1].user.as_str(),
                v[1].secs
            ),
            ("10.0.0.2:6000", "1", "app", Some(9))
        );
    }

    #[test]
    fn filter_and_sort() {
        let mut a = s("2");
        a.user = "zed".into();
        a.secs = Some(5);
        a.sql = "SELECT 1".into();
        let mut b = s("10");
        b.user = "Amy".into();
        b.secs = Some(50);
        let all = [a, b];
        assert_eq!(view(&all, "", Some((SortKey::Id, false))), vec![0, 1]);
        assert_eq!(view(&all, "", Some((SortKey::Id, true))), vec![1, 0]);
        assert_eq!(view(&all, "", Some((SortKey::User, false))), vec![1, 0]);
        assert_eq!(view(&all, "", Some((SortKey::Time, true))), vec![1, 0]);
        assert_eq!(view(&all, "select", None), vec![0]);
        assert_eq!(view(&all, "AMY", None), vec![1]);
        assert!(view(&all, "nope", None).is_empty());
        let pairs = vec![
            ("max_connections".to_string(), "151".to_string()),
            ("version".to_string(), "10.6".to_string()),
        ];
        assert_eq!(filter_pairs(&pairs, "MAX"), vec![0]);
        assert_eq!(filter_pairs(&pairs, "10.6"), vec![1]);
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_secs(45), "45s");
        assert_eq!(fmt_secs(185), "3m 05s");
        assert_eq!(fmt_secs(7380), "2h 03m");
        assert_eq!(fmt_secs(93600), "1d 02h");
        assert_eq!(one_line("select  1\n  from   t", 100), "select 1 from t");
        assert_eq!(one_line("abcdef", 3), "abc…");
    }
}
