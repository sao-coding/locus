//! What the app gives the server: saved connections, passwords, the
//! Production confirmation dialog, and a place to report activity.

use locus_core::guard::WriteInfo;
use locus_core::store::SavedConnection;

/// A Production write waiting for the user's answer in Locus.
pub struct ConfirmRequest {
    /// Matches the [`Event::ConfirmExpired`] sent when nobody answered.
    pub id: u64,
    pub conn: SavedConnection,
    /// The MCP client's name.
    pub client: String,
    pub sql: String,
    pub info: WriteInfo,
    /// `true` = Allow. Dropping it counts as Deny.
    pub reply: tokio::sync::oneshot::Sender<bool>,
}

/// Activity, for the status-bar indicator and History.
#[derive(Debug, Clone)]
pub enum Event {
    /// A tool started work on a connection.
    Busy {
        id: u64,
        client: String,
        conn: String,
        tool: &'static str,
    },
    Idle {
        id: u64,
    },
    /// A `run_sql` / `explain` call finished: one History entry (`mcp_client` set).
    History(locus_core::history::Entry),
    /// Nobody answered the confirmation `id` in time: close its dialog.
    ConfirmExpired {
        id: u64,
    },
}

pub trait Host: Send + Sync + 'static {
    /// Every saved connection (the server filters by MCP access itself).
    fn connections(&self) -> Vec<SavedConnection>;
    /// The saved password; `Ok(None)` when there is none.
    fn password(&self, conn_id: &str) -> Result<Option<String>, String>;
    /// Shows the Production confirmation; answers through `req.reply`.
    fn confirm(&self, req: ConfirmRequest);
    fn event(&self, ev: Event);
}
