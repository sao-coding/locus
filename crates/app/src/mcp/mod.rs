//! The built-in MCP server, app side: starts / stops `locus_mcp` from the
//! settings, keeps its token (Credential Manager), hands it the saved
//! connections, and brings its requests to the UI: Production
//! confirmations (one dialog at a time, `Locus::open_mcp_confirm`),
//! History entries and the status-bar activity.
//! The server itself (tools, access decision, HTTP checks) is `crates/mcp`.

pub(crate) mod page;

use gpui_kit::component::{WindowExt as _, h_flex};
use gpui_kit::*;
use locus_core::store::{SavedConnection, Store};
use locus_mcp::host::{ConfirmRequest, Event, Host};
use parking_lot::RwLock;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

/// From the server's threads to the UI.
enum Msg {
    Confirm(ConfirmRequest),
    Event(Event),
    /// The confirmation dialog `id` went away (answered or dismissed).
    DialogClosed(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Off,
    Listening(String),
    Failed(String),
}

#[derive(Clone, Debug)]
pub(crate) struct Busy {
    pub id: u64,
    pub client: String,
    pub conn: String,
    pub tool: &'static str,
}

/// A request handed to the dialog. The sender is taken when answered;
/// dropping the slot (dialog gone) answers Deny.
pub(crate) struct Slot {
    pub id: u64,
    pub reply: Option<tokio::sync::oneshot::Sender<bool>>,
    closed: UnboundedSender<MsgHandle>,
}

/// `Msg` is private; the slot only needs to send `DialogClosed`.
pub(crate) struct MsgHandle(Msg);

impl Drop for Slot {
    fn drop(&mut self) {
        let _ = self.closed.send(MsgHandle(Msg::DialogClosed(self.id)));
    }
}

/// Events for `Locus` (it has the window and History).
pub(crate) enum McpEvent {
    History(locus_core::history::Entry),
    /// Show this confirmation. The dialog owns the `Rc`; drop it to deny.
    Confirm {
        conn: SavedConnection,
        client: String,
        sql: String,
        info: locus_core::guard::WriteInfo,
        slot: Rc<RefCell<Slot>>,
    },
    /// Nobody answered in time: close dialog `id` if it is still open.
    Expired(u64),
}

/// What the server reads; kept in step by `Locus::sync_mcp_conns`.
#[derive(Default)]
struct Shared {
    conns: RwLock<Vec<SavedConnection>>,
    /// `Some`: passwords held in memory (demo / bench store); `None`: Credential Manager.
    memory_passwords: RwLock<Option<HashMap<String, String>>>,
}

struct AppHost {
    shared: Arc<Shared>,
    tx: UnboundedSender<MsgHandle>,
}

impl Host for AppHost {
    fn connections(&self) -> Vec<SavedConnection> {
        self.shared.conns.read().clone()
    }
    fn password(&self, conn_id: &str) -> Result<Option<String>, String> {
        if let Some(m) = self.shared.memory_passwords.read().as_ref() {
            return Ok(m.get(conn_id).cloned());
        }
        locus_core::creds::read(conn_id).map_err(|e| e.to_string())
    }
    fn confirm(&self, req: ConfirmRequest) {
        // A closed channel drops `req`: its reply sender answers Deny.
        let _ = self.tx.send(MsgHandle(Msg::Confirm(req)));
    }
    fn event(&self, ev: Event) {
        let _ = self.tx.send(MsgHandle(Msg::Event(ev)));
    }
}

pub(crate) struct Mcp {
    rt: tokio::runtime::Handle,
    shared: Arc<Shared>,
    tx: UnboundedSender<MsgHandle>,
    running: Option<locus_mcp::server::Running>,
    pub status: Status,
    /// Empty until the server is first switched on.
    pub token: String,
    /// Credential Manager (the real data folder); otherwise in memory only.
    disk: bool,
    pub busy: Vec<Busy>,
    queue: VecDeque<ConfirmRequest>,
    showing: Option<u64>,
    _task: Task<()>,
}

impl EventEmitter<McpEvent> for Mcp {}

struct McpGlobal(Entity<Mcp>);
impl Global for McpGlobal {}

pub(crate) fn entity(cx: &App) -> Option<Entity<Mcp>> {
    cx.try_global::<McpGlobal>().map(|g| g.0.clone())
}

/// Creates the entity and starts the server if the settings say so.
/// `disk`: the token lives in Credential Manager (real data folder).
pub(crate) fn init(rt: tokio::runtime::Handle, disk: bool, cx: &mut App) -> Entity<Mcp> {
    let (tx, mut rx) = unbounded_channel::<MsgHandle>();
    let e = cx.new(|cx: &mut Context<Mcp>| {
        let task = cx.spawn(async move |this, cx| {
            while let Some(MsgHandle(m)) = rx.recv().await {
                if this.update(cx, |this, cx| this.on_msg(m, cx)).is_err() {
                    break;
                }
            }
        });
        Mcp {
            rt,
            shared: Default::default(),
            tx,
            running: None,
            status: Status::Off,
            token: String::new(),
            disk,
            busy: Vec::new(),
            queue: VecDeque::new(),
            showing: None,
            _task: task,
        }
    });
    cx.set_global(McpGlobal(e.clone()));
    cx.observe_global::<crate::settings::AppSettings>(|cx| {
        if let Some(m) = entity(cx) {
            m.update(cx, |m, cx| m.apply(cx));
        }
    })
    .detach();
    e
}

impl Mcp {
    fn on_msg(&mut self, m: Msg, cx: &mut Context<Self>) {
        match m {
            Msg::Event(Event::Busy {
                id,
                client,
                conn,
                tool,
            }) => {
                self.busy.push(Busy {
                    id,
                    client,
                    conn,
                    tool,
                });
                cx.notify();
            }
            Msg::Event(Event::Idle { id }) => {
                self.busy.retain(|b| b.id != id);
                cx.notify();
            }
            Msg::Event(Event::History(e)) => cx.emit(McpEvent::History(e)),
            Msg::Event(Event::ConfirmExpired { id }) => {
                self.queue.retain(|r| r.id != id);
                if self.showing == Some(id) {
                    cx.emit(McpEvent::Expired(id));
                }
            }
            Msg::Confirm(req) => {
                self.queue.push_back(req);
                self.show_next(cx);
            }
            Msg::DialogClosed(id) => {
                if self.showing == Some(id) {
                    self.showing = None;
                }
                self.show_next(cx);
            }
        }
    }

    fn show_next(&mut self, cx: &mut Context<Self>) {
        if self.showing.is_some() {
            return;
        }
        // Requests whose caller already gave up are skipped.
        while let Some(req) = self.queue.pop_front() {
            if req.reply.is_closed() {
                continue;
            }
            self.showing = Some(req.id);
            let slot = Rc::new(RefCell::new(Slot {
                id: req.id,
                reply: Some(req.reply),
                closed: self.tx.clone(),
            }));
            cx.emit(McpEvent::Confirm {
                conn: req.conn,
                client: req.client,
                sql: req.sql,
                info: req.info,
                slot,
            });
            return;
        }
    }

    /// The port in the settings.
    pub(crate) fn port(cx: &App) -> u16 {
        crate::settings::get(cx)
            .mcp_port
            .unwrap_or(locus_core::settings::MCP_DEFAULT_PORT)
    }

    pub(crate) fn url_for(port: u16) -> String {
        format!("http://127.0.0.1:{port}/mcp")
    }

    pub(crate) fn listening(&self) -> bool {
        self.running.is_some()
    }

    /// The token, made (and saved) the first time.
    fn ensure_token(&mut self) -> Result<(), String> {
        if !self.token.is_empty() {
            return Ok(());
        }
        // Test hook of dev builds (live MCP checks): a known token, never saved.
        #[cfg(feature = "dev")]
        if let Some(t) = std::env::var("LOCUS_MCP_TOKEN")
            .ok()
            .filter(|t| !t.is_empty())
        {
            self.token = t;
            return Ok(());
        }
        if self.disk
            && let Some(t) = locus_core::creds::read_mcp_token()
                .map_err(|e| format!("token could not be read: {e}"))?
                .filter(|t| !t.is_empty())
        {
            self.token = t;
            return Ok(());
        }
        let t = locus_mcp::guard::new_token()?;
        if self.disk {
            locus_core::creds::write_mcp_token(&t)
                .map_err(|e| format!("token could not be saved: {e}"))?;
        }
        self.token = t;
        Ok(())
    }

    /// Starts, stops or restarts the server to match the settings.
    pub(crate) fn apply(&mut self, cx: &mut Context<Self>) {
        let s = crate::settings::get(cx);
        let (on, port) = (s.mcp_enabled, Self::port(cx));
        match (&self.running, on) {
            (Some(r), true) if r.port == port => return,
            (None, false) => {
                if self.status != Status::Off {
                    self.status = Status::Off;
                    cx.notify();
                }
                return;
            }
            _ => {}
        }
        if let Some(r) = self.running.take() {
            r.stop();
            self.busy.clear();
        }
        self.status = Status::Off;
        if on {
            self.status = match self.start(port) {
                Ok(url) => Status::Listening(url),
                Err(e) => {
                    log::warn!("MCP server: {e}");
                    Status::Failed(e)
                }
            };
        }
        cx.notify();
    }

    fn start(&mut self, port: u16) -> Result<String, String> {
        self.ensure_token()?;
        let host = Arc::new(AppHost {
            shared: self.shared.clone(),
            tx: self.tx.clone(),
        });
        let r = locus_mcp::server::start(&self.rt, port, self.token.clone(), host)?;
        let url = r.url();
        self.running = Some(r);
        Ok(url)
    }

    /// A new token; clients must be given it again.
    pub(crate) fn regenerate_token(&mut self, cx: &mut Context<Self>) {
        match locus_mcp::guard::new_token().and_then(|t| {
            if self.disk {
                locus_core::creds::write_mcp_token(&t)
                    .map_err(|e| format!("token could not be saved: {e}"))?;
            }
            Ok(t)
        }) {
            Ok(t) => {
                if let Some(r) = &self.running {
                    r.set_token(t.clone());
                }
                self.token = t;
            }
            Err(e) => crate::shell::toast::show(e, cx),
        }
        cx.notify();
    }

    /// Connections and (for an in-memory store) passwords the server may use.
    pub(crate) fn set_conns(&self, conns: &[SavedConnection], store: &Store) {
        *self.shared.conns.write() = conns.to_vec();
        let mem = match store {
            Store::Disk => None,
            Store::Memory { .. } => Some(
                conns
                    .iter()
                    .filter_map(|c| {
                        store
                            .password(&c.id)
                            .ok()
                            .flatten()
                            .map(|p| (c.id.clone(), p))
                    })
                    .collect(),
            ),
        };
        *self.shared.memory_passwords.write() = mem;
    }

    pub(crate) fn conns(&self) -> Vec<SavedConnection> {
        self.shared.conns.read().clone()
    }

    /// Stops the server (app closing).
    pub(crate) fn shutdown(&mut self) {
        if let Some(r) = self.running.take() {
            r.stop();
        }
    }
}

impl crate::shell::Locus {
    /// Gives the MCP server the current saved connections.
    pub(crate) fn sync_mcp_conns(&mut self, cx: &mut App) {
        let (conns, store) = (&self.conns, &self.store);
        self.mcp.read(cx).set_conns(conns, store);
    }

    pub(crate) fn on_mcp_event(
        &mut self,
        ev: &McpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            McpEvent::History(e) => {
                let e = e.clone();
                self.history.update(cx, |h, cx| h.add(e, cx));
            }
            McpEvent::Confirm {
                conn,
                client,
                sql,
                info,
                slot,
            } => self.open_mcp_confirm(
                conn.clone(),
                client.clone(),
                sql.clone(),
                info.clone(),
                slot.clone(),
                window,
                cx,
            ),
            McpEvent::Expired(id) => {
                if self.prod.mcp_open == Some(*id) {
                    self.prod.mcp_open = None;
                    window.close_dialog(cx);
                    crate::shell::toast::show(rust_i18n::t!("mcp.expired").into_owned(), cx);
                }
            }
        }
    }

    /// Status-bar item while the server listens: "MCP", or what a client is doing now.
    pub(crate) fn render_mcp_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::shell::scale;
        let m = self.mcp.read(cx);
        if !m.listening() {
            return None;
        }
        let t = crate::shell::theme::tokens(cx);
        let busy = m.busy.last().cloned();
        let (text, tip_text, dot) = match &busy {
            Some(b) => (
                rust_i18n::t!("mcp.bar_busy", tool = b.tool, conn = b.conn.clone()).into_owned(),
                rust_i18n::t!("mcp.bar_busy_tip", client = b.client.clone()).into_owned(),
                t.acc,
            ),
            None => (
                "MCP".to_string(),
                rust_i18n::t!("mcp.bar_idle_tip").into_owned(),
                t.idle,
            ),
        };
        Some(
            h_flex()
                .id("status-mcp")
                .gap(px(scale::S3))
                .items_center()
                .cursor_pointer()
                .text_color(if busy.is_some() { t.tx2 } else { t.tx3 })
                .hover(|h| h.text_color(t.tx))
                .tooltip(crate::shell::tip(tip_text))
                .child(div().size(px(6.)).rounded_full().bg(dot))
                .child(text)
                .on_click(cx.listener(|this, _, window, cx| this.open_settings_mcp(window, cx)))
                .into_any_element(),
        )
    }
}

/// `abcd…wxyz` for the masked token.
pub(crate) fn mask(token: &str) -> String {
    if token.len() < 12 {
        return "•".repeat(token.len().max(8));
    }
    format!("{}{}", "•".repeat(12), &token[token.len() - 4..])
}

/// The `claude mcp add` command for this URL and token.
pub(crate) fn claude_command(url: &str, token: &str) -> String {
    format!(
        "claude mcp add --transport http locus {url} --header \"Authorization: Bearer {token}\""
    )
}

/// Cursor (`.cursor/mcp.json`) / generic client JSON.
pub(crate) fn client_json(url: &str, token: &str) -> String {
    format!(
        "{{\n  \"mcpServers\": {{\n    \"locus\": {{\n      \"url\": \"{url}\",\n      \"headers\": {{ \"Authorization\": \"Bearer {token}\" }}\n    }}\n  }}\n}}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn snippets_and_mask() {
        assert_eq!(
            claude_command("http://127.0.0.1:7781/mcp", "T"),
            "claude mcp add --transport http locus http://127.0.0.1:7781/mcp --header \"Authorization: Bearer T\""
        );
        let j: serde_json::Value =
            serde_json::from_str(&client_json("http://127.0.0.1:7781/mcp", "T")).unwrap();
        assert_eq!(j["mcpServers"]["locus"]["url"], "http://127.0.0.1:7781/mcp");
        assert_eq!(
            j["mcpServers"]["locus"]["headers"]["Authorization"],
            "Bearer T"
        );
        let m = mask("abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG");
        assert!(m.ends_with("DEFG") && !m.contains("abcd"));
        assert!(!mask("short").contains('s'));
        assert_eq!(Mcp::url_for(7781), "http://127.0.0.1:7781/mcp");
    }
}
