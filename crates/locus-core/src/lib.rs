//! Locus logic without GPUI. The `app` crate (locus.exe) holds the
//! views; everything here is plain Rust and unit-tested without a window.
//!
//! Strings come from `crates/app/locales` (the same files the app embeds);
//! the language is the process-wide `rust_i18n::set_locale` the app sets.

rust_i18n::i18n!("../app/locales", fallback = "en");

pub mod activity;
pub mod apply;
pub mod audit;
pub mod colfilter;
pub mod colfilter_store;
pub mod colors;
pub mod copyfmt;
pub mod creds;
pub mod datafile;
pub mod driver;
pub mod export;
pub mod export_stream;
#[cfg(test)]
mod export_tests;
mod export_xlsx;
pub mod fknav;
pub mod fmt;
pub mod groups;
pub mod guard;
pub mod hexfmt;
pub mod history;
pub mod import;
pub mod joinsug;
pub mod keymap;
mod locale_keys;
pub mod palette_scope;
pub mod params;
pub mod plan;
pub mod pool;
pub mod redis;
pub mod rowcap;
pub mod rowfilter;
pub mod runview;
pub mod saved;
pub mod selstats;
pub mod settings;
pub mod snippet;
pub mod space_theme;
pub mod spaces;
pub mod split;
pub mod sqlrefs;
pub mod sshcfg;
pub mod status;
pub mod store;
pub mod tabfit;
pub mod tunnel;
pub mod txn;
pub mod update;
pub mod update_install;
pub mod vars;
pub mod viewer;
