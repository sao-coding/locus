//! What the shell shows about a connection: state, round-trip time and server
//! version. Measured once per opened driver with one `SELECT 1` and the
//! catalog's server info (only for connections the pool already opened, never
//! a hidden connect).
use crate::shell::scale::*;
use crate::shell::theme::ConnState;
use crate::shell::*;
use locus_core::store::DbKind;

#[derive(Default, Clone)]
pub(crate) struct ConnMeta {
    pub version: Option<String>,
    pub latency_ms: Option<u64>,
    /// Identity of the driver this was measured on (a reconnect measures again).
    driver: usize,
}

/// Round trip above this is shown as "slow" (yellow).
const SLOW_MS: u64 = 150;

impl Locus {
    /// Starts the probe for each open connection of the active Space that has none yet.
    pub(crate) fn probe_conns(&mut self, cx: &mut Context<Self>) {
        let ids = self.space().connections.clone();
        for id in ids {
            let Some(d) = self.pool.open_driver(&id) else {
                continue;
            };
            let key = Arc::as_ptr(&d) as *const () as usize;
            if self.conn_meta.get(&id).is_some_and(|m| m.driver == key) {
                continue;
            }
            let kind = self.conns.iter().find(|c| c.id == id).map(|c| c.kind);
            self.conn_meta.insert(
                id.clone(),
                ConnMeta {
                    driver: key,
                    ..Default::default()
                },
            );
            cx.spawn(async move |this, cx| {
                let sql = if d.is_oracle() {
                    "SELECT 1 FROM DUAL"
                } else {
                    "SELECT 1"
                };
                let t0 = std::time::Instant::now();
                let mut h = db_core::Driver::query(&*d, sql.to_string(), Default::default());
                let mut ok = false;
                while let Some(ev) = h.next().await {
                    if let db_core::QueryEvent::Finished(o) = ev {
                        ok = matches!(o, db_core::Outcome::Done { .. });
                        break;
                    }
                }
                let ms = t0.elapsed().as_millis() as u64;
                let version = match db_core::Driver::catalog(&*d).map(|c| c.server_info(1)) {
                    Some(f) => f.await.ok().and_then(|items| {
                        items
                            .iter()
                            .find(|i| i.name.eq_ignore_ascii_case("version"))
                            .or_else(|| items.first())
                            .map(|i| short_version(&i.value, kind))
                    }),
                    None => None,
                };
                let _ = this.update(cx, |this, cx| {
                    if let Some(m) = this.conn_meta.get_mut(&id)
                        && m.driver == key
                    {
                        m.latency_ms = ok.then_some(ms);
                        m.version = version;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// State, dot colour and label of a connection, as of now.
    pub(crate) fn conn_state(&self, id: &str, cx: &App) -> (ConnState, String) {
        if let Some(h) = crate::redis::health(id) {
            if h.failed {
                return (ConnState::Failed, t!("shell.state_failed").into_owned());
            }
            if h.ok {
                return match h.latency_ms {
                    Some(ms) if ms as u64 > SLOW_MS => (
                        ConnState::Slow,
                        t!("shell.state_slow", ms = ms.to_string()).into_owned(),
                    ),
                    Some(ms) => (
                        ConnState::Ok,
                        t!("shell.state_ok_ms", ms = ms.to_string()).into_owned(),
                    ),
                    None => (ConnState::Ok, t!("shell.state_ok").into_owned()),
                };
            }
        }
        if self.pool.open_driver(id).is_some() {
            return match self.conn_meta.get(id).and_then(|m| m.latency_ms) {
                Some(ms) if ms > SLOW_MS => (
                    ConnState::Slow,
                    t!("shell.state_slow", ms = ms.to_string()).into_owned(),
                ),
                Some(ms) => (
                    ConnState::Ok,
                    t!("shell.state_ok_ms", ms = ms.to_string()).into_owned(),
                ),
                None => (ConnState::Ok, t!("shell.state_ok").into_owned()),
            };
        }
        let tabs = self
            .space()
            .tabs
            .iter()
            .map(|t| t.read(cx))
            .filter(|t| t.conn_id.as_deref() == Some(id));
        let mut failed = false;
        for t in tabs {
            let st = t.status.read(cx);
            if st.phase == locus_core::status::Phase::Connecting {
                return (ConnState::Connecting, t!("state.connecting").into_owned());
            }
            if st.error.as_ref().is_some_and(|e| {
                matches!(
                    e.kind,
                    ErrorKind::Connect | ErrorKind::Auth | ErrorKind::Tls
                )
            }) {
                failed = true;
            }
        }
        if failed
            || matches!(
                self.objects.read(cx).schema_list(id),
                crate::connections::objects::SchemaList::Failed
            )
        {
            return (ConnState::Failed, t!("shell.state_failed").into_owned());
        }
        (ConnState::Idle, t!("shell.state_idle").into_owned())
    }

    pub(crate) fn conn_version(&self, id: &str) -> Option<String> {
        self.conn_meta
            .get(id)
            .and_then(|m| m.version.clone())
            .or_else(|| {
                crate::redis::health(id)
                    .and_then(|h| h.version)
                    .map(|v| format!("Redis {v}"))
            })
    }

    /// Engine badge of a connection (MariaDB once the server said so).
    pub(crate) fn conn_badge(&self, c: &SavedConnection, cx: &App) -> AnyElement {
        let maria = self
            .conn_version(&c.id)
            .is_some_and(|v| v.contains("MariaDB"));
        let (label, color) = crate::shell::theme::db_badge(c.kind, maria, cx);
        let (bg, fg) = crate::shell::theme::badge_colors(color, cx);
        div()
            .flex_none()
            .w(px(20.))
            .h(px(18.))
            .rounded(px(R_SM))
            .flex()
            .items_center()
            .justify_center()
            .bg(bg)
            .text_color(fg)
            .text_size(px(10.))
            .font_weight(FontWeight::SEMIBOLD)
            .font_family(cx.theme().mono_font_family.clone())
            .child(label)
            .into_any_element()
    }
}

/// "11.8.5-MariaDB-ubu2404" → "MariaDB 11.8.5"; other strings are cut at 40 characters.
fn short_version(v: &str, kind: Option<DbKind>) -> String {
    let v = v.trim();
    if v.contains("MariaDB") {
        let num = v.split('-').next().unwrap_or(v);
        return format!("MariaDB {num}");
    }
    if let Some(rest) = v.strip_prefix("PostgreSQL ") {
        return format!(
            "PostgreSQL {}",
            rest.split_whitespace().next().unwrap_or("")
        );
    }
    if v.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        let name = match kind {
            Some(DbKind::Oracle) => "Oracle",
            Some(DbKind::Postgres) => "PostgreSQL",
            Some(DbKind::Sqlite) => "SQLite",
            _ => "MySQL",
        };
        return format!("{name} {}", v.split('-').next().unwrap_or(v));
    }
    v.chars().take(40).collect()
}
