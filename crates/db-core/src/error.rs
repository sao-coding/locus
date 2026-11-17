use std::fmt;

/// What went wrong, coarse enough for the UI to pick a translated title.
/// The server's own text stays in [`DbError::message`] untranslated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Could not reach the server.
    Connect,
    /// Server refused the credentials.
    Auth,
    /// TLS was required and could not be established.
    Tls,
    /// The server rejected or aborted the statement.
    Query,
    /// The query hit its timeout and was stopped on the server.
    Timeout,
    /// The driver, sidecar or wire protocol broke.
    Internal,
}

#[derive(Clone, Debug)]
pub struct DbError {
    pub kind: ErrorKind,
    /// Server error number when there is one (e.g. MySQL 1064).
    pub code: Option<i64>,
    pub message: String,
}

impl DbError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            code: None,
            message: message.into(),
        }
    }

    pub fn with_code(mut self, code: i64) -> Self {
        self.code = Some(code);
        self
    }
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.code {
            Some(code) => write!(f, "{:?} {code}: {}", self.kind, self.message),
            None => write!(f, "{:?}: {}", self.kind, self.message),
        }
    }
}

impl std::error::Error for DbError {}
