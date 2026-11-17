//! Driver interface shared by in-process drivers (`db-mysql`) and the Go
//! sidecar client (`db-sidecar-client`).
//!
//! One query is one [`QueryHandle`]: a bounded stream of [`QueryEvent`]s plus
//! a cancel switch. The stream is bounded, so a consumer that stops reading
//! stops the driver (back-pressure) instead of filling memory.
//!
//! Event order for every query, whichever driver runs it:
//! `Started` → (`Columns` → `Batch`* | `Affected`)* → `Finished`.
//! Each `Columns` opens a new result set and the batches after it belong to
//! it, so a call that returns several result sets sends `Columns` several
//! times. A statement with no result set (INSERT, UPDATE, DDL) sends
//! `Affected` instead. A query that fails before it reaches the server sends
//! only `Finished(Outcome::Failed)`.

mod batch;
mod catalog;
pub mod classify;
mod error;
pub mod format;
pub mod sql;
mod stream;
pub mod struct_sql;
pub mod structure;

pub use batch::{Batch, BatchBuilder, Cell, ResultSet};
pub use catalog::{
    Catalog, CatalogFuture, ColumnInfo, DbObjectKind, ForeignKey, InfoItem, ObjectEntry,
    ObjectKind, ObjectPage, Principal, RoutineParam, SchemaObject,
};
pub mod dictionary;
pub mod routine;
pub use error::{DbError, ErrorKind};
pub use stream::{Canceller, EventSink, QueryControl, QueryHandle, SinkClosed, channel, finished};

use std::time::Duration;

/// Rows per batch unless the caller asks otherwise. The sidecar protocol
/// measured in the POC uses the same size.
pub const DEFAULT_BATCH_ROWS: usize = 1000;

/// Batches a driver may have in flight before it has to wait for the consumer.
pub const DEFAULT_WINDOW: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnMeta {
    pub name: String,
    /// Server type name as the driver reports it, e.g. `VARCHAR`, `DECIMAL`.
    pub type_name: String,
    /// `None` when the driver cannot tell.
    pub nullable: Option<bool>,
    /// The table column this result column comes from. `None` for
    /// expressions, or when the driver cannot tell. The grid needs it to know
    /// whether a cell can be edited and which row to update.
    pub origin: Option<ColumnOrigin>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnOrigin {
    pub schema: String,
    /// The real table name, not an alias.
    pub table: String,
    /// The real column name, not an alias.
    pub column: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TlsMode {
    /// Refuse to connect without TLS, and verify the server certificate and
    /// host name. The default.
    #[default]
    Require,
    /// Encrypted, but the server certificate is not checked (self-signed or
    /// private CA). Stops passive eavesdropping, not an active man in the
    /// middle. The UI must warn before using this.
    RequireUnverified,
    /// Plain text. The UI must warn before using this.
    Disabled,
}

/// Connection settings. The password is passed in memory only; storing it is
/// the application's job (Windows Credential Manager), never a file.
#[derive(Clone, Default)]
pub struct ConnectOptions {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: Option<String>,
    pub tls: TlsMode,
}

impl std::fmt::Debug for ConnectOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectOptions")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &"***")
            .field("database", &self.database)
            .field("tls", &self.tls)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct QueryOptions {
    pub batch_rows: usize,
    /// Server-side stop after this long; `None` means no limit.
    pub timeout: Option<Duration>,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            batch_rows: DEFAULT_BATCH_ROWS,
            timeout: None,
        }
    }
}

/// A bound parameter value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Param {
    Null,
    Text(String),
    Bytes(Vec<u8>),
}

#[derive(Debug)]
pub enum QueryEvent {
    /// The query is running on the server. `session_id` is the server's
    /// connection / session id; the host keeps it so an orphaned query can be
    /// killed later from another connection or a fresh sidecar.
    Started {
        session_id: u64,
    },
    Columns(Vec<ColumnMeta>),
    Batch(Batch),
    /// A statement without a result set completed.
    Affected {
        rows: u64,
    },
    /// Always the last event.
    Finished(Outcome),
}

#[derive(Debug)]
pub enum Outcome {
    Done {
        rows: u64,
    },
    /// The caller cancelled and the server-side query has been stopped.
    Cancelled,
    Failed(DbError),
}

/// An open connection (or pool) to one database.
///
/// Implementations must make cancel stop the query **on the server**, not
/// only on the client: for MySQL / MariaDB that means `KILL QUERY <id>` on a
/// second connection (measured in the POC; dropping the future or closing the
/// socket leaves the query running).
pub trait Driver: Send + Sync + 'static {
    /// Short stable id, e.g. `mysql`, `sidecar`. Not shown to users.
    fn kind(&self) -> &'static str;

    /// Start a query. Never blocks; errors arrive as `Finished(Failed)`.
    fn query(&self, sql: String, opts: QueryOptions) -> QueryHandle;

    /// Kill a whole server session by the id from [`QueryEvent::Started`].
    /// Used to clean up after a driver or sidecar crash.
    fn kill_session(&self, session_id: u64) -> QueryHandle;

    /// Run one statement with bound parameters (`?` placeholders in `sql`,
    /// in order). Values never get spliced into the SQL text. Used for the
    /// UPDATE / INSERT / DELETE the grid builds when cells are edited. Same
    /// event stream as [`query`](Self::query).
    fn execute(&self, sql: String, params: Vec<Param>, opts: QueryOptions) -> QueryHandle {
        let _ = (sql, params, opts);
        finished(Outcome::Failed(DbError::new(
            ErrorKind::Internal,
            "this driver does not support bound parameters",
        )))
    }

    /// Quote a schema / table / column name for this database.
    fn quote_identifier(&self, name: &str) -> String {
        format!("\"{}\"", name.replace('"', "\"\""))
    }

    /// Schema introspection, when the driver supports it.
    fn catalog(&self) -> Option<&dyn Catalog> {
        None
    }
}
