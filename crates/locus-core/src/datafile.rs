//! Loading user data files without ever losing them: a file that exists but
//! cannot be parsed is renamed to `<stem>.bad-<yyyyMMdd-HHmmss>.json` (kept,
//! never deleted), defaults are used, and saving that file is refused for
//! the rest of the run. The next start finds no file and begins clean.

use serde::de::DeserializeOwned;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Result of reading one data file.
#[derive(Debug, PartialEq)]
pub enum Loaded<T> {
    /// No file yet: defaults, saving allowed.
    Missing,
    Ok(T),
    /// The file was kept as `backup`; defaults are used and saves are refused.
    Unreadable {
        backup: PathBuf,
        error: String,
    },
}

impl<T: Default> Loaded<T> {
    pub fn into_value(self) -> T {
        match self {
            Loaded::Ok(v) => v,
            _ => T::default(),
        }
    }
}

/// One start-up message: `file` could not be read and was kept as `backup`.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub file: String,
    pub backup: String,
}

static BLOCKED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);
static NOTICES: Mutex<Vec<Notice>> = Mutex::new(Vec::new());

/// True when `path` was unreadable this run (saves must not write it).
pub fn is_blocked(path: &Path) -> bool {
    BLOCKED
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.contains(path)))
        .unwrap_or(false)
}

/// Messages for the files that were backed up; the UI shows each once.
pub fn take_notices() -> Vec<Notice> {
    NOTICES
        .lock()
        .map(|mut n| std::mem::take(&mut *n))
        .unwrap_or_default()
}

fn block(path: &Path) {
    if let Ok(mut g) = BLOCKED.lock() {
        g.get_or_insert_with(HashSet::new)
            .insert(path.to_path_buf());
    }
}

/// `connections.json` + 20261121-153000 -> `connections.bad-20261121-153000.json`.
pub fn bad_name(path: &Path, stamp: &str) -> PathBuf {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("data");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("json");
    path.with_file_name(format!("{stem}.bad-{stamp}.{ext}"))
}

fn stamp() -> String {
    crate::history::format_time(crate::history::now_secs())
        .replace(['-', ':'], "")
        .replace(' ', "-")
}

/// Reads and parses `path`. On a parse error the file is renamed (see the
/// module doc). Pure with respect to globals: it does not block saves.
pub fn load_path<T: DeserializeOwned>(path: &Path) -> Loaded<T> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::Missing,
        Err(e) => return back_up(path, e.to_string()),
    };
    match serde_json::from_slice::<T>(&bytes) {
        Ok(v) => Loaded::Ok(v),
        Err(e) => back_up(path, e.to_string()),
    }
}

fn back_up<T>(path: &Path, error: String) -> Loaded<T> {
    let mut backup = bad_name(path, &stamp());
    let mut n = 1;
    while backup.exists() {
        backup = bad_name(path, &format!("{}-{n}", stamp()));
        n += 1;
    }
    match std::fs::rename(path, &backup) {
        Ok(()) => Loaded::Unreadable { backup, error },
        // Could not even move it: keep the original where it is.
        Err(e) => Loaded::Unreadable {
            backup: path.to_path_buf(),
            error: format!("{error}; rename failed: {e}"),
        },
    }
}

/// Loads `<app dir>/<name>`; an unreadable file blocks saving it this run,
/// is logged and queued for a start-up message. `None` = no file / no app dir.
pub fn load_named<T: DeserializeOwned>(name: &str) -> Option<T> {
    let path = crate::settings::app_dir()?.join(name);
    match load_path::<T>(&path) {
        Loaded::Ok(v) => Some(v),
        Loaded::Missing => None,
        Loaded::Unreadable { backup, error } => {
            log::error!(
                "{name} could not be read ({error}); kept as {}",
                backup.display()
            );
            block(&path);
            if let Ok(mut n) = NOTICES.lock() {
                n.push(Notice {
                    file: name.to_string(),
                    backup: backup
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                });
            }
            None
        }
    }
}

/// Error returned by a refused save.
pub fn refused(path: &Path) -> std::io::Error {
    std::io::Error::other(format!(
        "{} was unreadable at start-up and is kept untouched until restart",
        path.file_name()
            .map(|f| f.to_string_lossy())
            .unwrap_or_default()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, Deserialize, PartialEq)]
    struct Doc {
        a: u32,
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("locus-datafile-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn corrupt_file_is_backed_up_with_its_bytes() {
        let d = tmp("corrupt");
        let p = d.join("connections.json");
        std::fs::write(&p, b"{ not json").unwrap();
        let Loaded::Unreadable { backup, .. } = load_path::<Doc>(&p) else {
            panic!("expected Unreadable");
        };
        assert!(!p.exists());
        let name = backup.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("connections.bad-") && name.ends_with(".json"),
            "{name}"
        );
        assert_eq!(std::fs::read(&backup).unwrap(), b"{ not json");
        assert_eq!(Loaded::<Doc>::Missing.into_value(), Doc::default());
    }

    #[test]
    fn missing_file_is_missing_and_saving_works() {
        let d = tmp("missing");
        let p = d.join("x.json");
        assert_eq!(load_path::<Doc>(&p), Loaded::Missing);
        assert!(!is_blocked(&p));
        crate::settings::write_atomic(&p, b"{\"a\":1}").unwrap();
        assert_eq!(load_path::<Doc>(&p), Loaded::Ok(Doc { a: 1 }));
    }

    #[test]
    fn blocked_file_refuses_saves() {
        let d = tmp("blocked");
        let p = d.join("y.json");
        block(&p);
        assert!(crate::settings::write_atomic(&p, b"{}").is_err());
        assert!(!p.exists());
    }

    #[test]
    fn bad_name_keeps_stem_and_extension() {
        let n = bad_name(Path::new("C:/a/saved-queries.json"), "20260102-030405");
        assert_eq!(
            n,
            PathBuf::from("C:/a/saved-queries.bad-20260102-030405.json")
        );
    }
}

#[cfg(test)]
mod store_tests {
    //! Each stored file type: a corrupt file is backed up (bytes kept) and an
    //! older file without the newer fields still loads.
    use super::*;
    use crate::store::SavedConnection;
    use crate::{history::Entry, saved::SavedQuery, settings::Settings, spaces::SpacesFile};

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("locus-df2-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn corrupt<T: DeserializeOwned>(name: &str) {
        let d = dir(name);
        let p = d.join(format!("{name}.json"));
        // A newer version could write an enum value this one does not know.
        let bytes = br#"[{"env":"from-the-future"}] trailing"#;
        std::fs::write(&p, bytes).unwrap();
        let Loaded::Unreadable { backup, .. } = load_path::<T>(&p) else {
            panic!("{name}: expected Unreadable");
        };
        assert_eq!(std::fs::read(backup).unwrap(), bytes);
        assert!(!p.exists());
        assert!(load_path::<T>(&p).into_value_is_missing());
    }

    impl<T> Loaded<T> {
        fn into_value_is_missing(self) -> bool {
            matches!(self, Loaded::Missing)
        }
    }

    #[test]
    fn connections_corrupt() {
        corrupt::<Vec<SavedConnection>>("connections");
    }
    #[test]
    fn spaces_corrupt() {
        corrupt::<SpacesFile>("spaces");
    }
    #[test]
    fn history_corrupt() {
        corrupt::<Vec<Entry>>("history");
    }
    #[test]
    fn saved_queries_corrupt() {
        corrupt::<Vec<SavedQuery>>("saved-queries");
    }
    #[test]
    fn settings_corrupt() {
        corrupt::<Settings>("settings");
    }

    #[test]
    fn older_files_without_new_fields_load() {
        let d = dir("old");
        let p = d.join("settings.json");
        std::fs::write(&p, b"{}").unwrap();
        assert!(matches!(load_path::<Settings>(&p), Loaded::Ok(_)));
        let p = d.join("spaces.json");
        std::fs::write(&p, br#"{"spaces":[{"id":"s","name":"P"}]}"#).unwrap();
        assert!(matches!(load_path::<SpacesFile>(&p), Loaded::Ok(_)));
    }
}
