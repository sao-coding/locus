//! The Git engine of Locus: the user's installed `git` CLI run as a process, plus pure
//! parsers for what it prints (status, log, diff), the secret scan and repo-root lookup.
//!
//! Why the CLI and not libgit2 / gix: the user's credentials, SSH setup, Git Credential
//! Manager, hooks and config then work exactly as in their terminal.
pub mod branch;
pub mod diff;
pub mod exec;
pub mod log;
pub mod ops;
pub mod root;
pub mod secrets;
pub mod status;

pub use exec::{Cancel, Git, GitError};
pub use ops::{PullMode, Repo};
