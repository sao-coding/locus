//! Mapping from `mysql_async` errors to [`db_core::DbError`].
//!
//! Messages come from the server or the driver; neither includes the password
//! (the connection options are never formatted anywhere in this crate).

use db_core::{DbError, ErrorKind};
use mysql_async::{DriverError, Error, IoError};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Connect,
    Query,
}

/// Server error numbers that mean "the credentials were refused".
const AUTH_CODES: [u16; 4] = [
    1045, // ER_ACCESS_DENIED_ERROR
    1044, // ER_DBACCESS_DENIED_ERROR
    1698, // ER_ACCESS_DENIED_NO_PASSWORD_ERROR
    1251, // ER_NOT_SUPPORTED_AUTH_MODE
];

pub(crate) fn map_error(err: &Error, phase: Phase) -> DbError {
    match err {
        Error::Server(s) => {
            let kind = match phase {
                Phase::Connect if AUTH_CODES.contains(&s.code) => ErrorKind::Auth,
                Phase::Connect => ErrorKind::Connect,
                Phase::Query => ErrorKind::Query,
            };
            DbError::new(kind, s.message.clone()).with_code(i64::from(s.code))
        }
        Error::Io(IoError::Tls(e)) => DbError::new(ErrorKind::Tls, e.to_string()),
        Error::Io(e) => DbError::new(ErrorKind::Connect, e.to_string()),
        Error::Driver(d) => {
            let kind = match d {
                DriverError::NoClientSslFlagFromServer => ErrorKind::Tls,
                DriverError::UnknownAuthPlugin { .. }
                | DriverError::MysqlOldPasswordDisabled
                | DriverError::CleartextPluginDisabled
                | DriverError::InvalidParsecSalt => ErrorKind::Auth,
                // Wrong number of bound parameters: the caller's statement is wrong.
                DriverError::StmtParamsMismatch { .. } => ErrorKind::Query,
                DriverError::ConnectionClosed | DriverError::PoolDisconnected => ErrorKind::Connect,
                _ => ErrorKind::Internal,
            };
            let message = match d {
                DriverError::NoClientSslFlagFromServer => {
                    "TLS is required but the server does not offer it".to_string()
                }
                other => other.to_string(),
            };
            DbError::new(kind, message)
        }
        Error::Url(e) => DbError::new(ErrorKind::Internal, e.to_string()),
        Error::Other(e) => DbError::new(ErrorKind::Internal, e.to_string()),
    }
}
