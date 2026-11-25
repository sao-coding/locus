//! Built-in MCP server of Locus: Streamable HTTP on 127.0.0.1, inside the
//! running app. The app starts / stops it ([`server::start`]) and answers
//! through [`host::Host`]; everything here is plain Rust without GPUI.
//!
//! - [`access`]: Off / Read-only / Read-write per connection, then the
//!   Production guard (`locus_core::guard`).
//! - [`guard`]: Host / Origin / bearer token / body size checks.
//! - [`format`]: compact TSV results, row cap and cell truncation.
//! - [`tools`]: the six tools.

pub mod access;
pub mod exec;
pub mod format;
pub mod guard;
pub mod host;
pub mod server;
pub mod tools;

#[cfg(test)]
mod tests;
