//! Finding the repository of a folder without starting git, and mapping file paths to it.
use std::path::{Component, Path, PathBuf};

/// The nearest folder at or above `start` that holds a `.git` entry (a folder, or the file
/// a worktree / submodule uses).
pub fn find_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
}

/// `file` relative to `root` with `/` separators, or `None` when it lies outside.
/// Windows paths compare without regard to case.
pub fn rel_path(root: &Path, file: &Path) -> Option<String> {
    let r: Vec<_> = root.components().collect();
    let f: Vec<_> = file.components().collect();
    if f.len() <= r.len() {
        return None;
    }
    for (a, b) in r.iter().zip(&f) {
        if !same(a, b) {
            return None;
        }
    }
    let parts: Vec<String> = f[r.len()..]
        .iter()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn same(a: &Component, b: &Component) -> bool {
    let (a, b) = (
        a.as_os_str().to_string_lossy(),
        b.as_os_str().to_string_lossy(),
    );
    if cfg!(windows) {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

/// The absolute path of a repo-relative `rel`.
pub fn abs_path(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, s| p.join(s))
}

/// Splits `dir/sub/file.sql` into (`dir/sub`, `file.sql`).
pub fn split_name(rel: &str) -> (&str, &str) {
    match rel.rfind('/') {
        Some(i) => (&rel[..i], &rel[i + 1..]),
        None => ("", rel),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("locus-git-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn finds_the_repo_above_a_subfolder() {
        let d = scratch("root");
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        assert_eq!(find_root(&d.join("a/b")), Some(d.clone()));
        assert_eq!(find_root(&d), Some(d.clone()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_git_file_counts() {
        let d = scratch("gitfile");
        std::fs::write(d.join(".git"), "gitdir: ../elsewhere").unwrap();
        assert_eq!(find_root(&d), Some(d.clone()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn no_repo_is_none() {
        let d = scratch("none");
        // A temp dir sits under no repo on a normal machine; if it does, skip.
        if find_root(&d).is_none() {
            assert_eq!(find_root(&d.join("x")), None);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn relative_paths() {
        let root = Path::new("C:/work/repo");
        assert_eq!(
            rel_path(root, Path::new("C:/work/repo/sql/報表 1.sql")).as_deref(),
            Some("sql/報表 1.sql")
        );
        assert_eq!(rel_path(root, Path::new("C:/work/other/a.sql")), None);
        assert_eq!(rel_path(root, Path::new("C:/work/repo")), None);
        assert_eq!(rel_path(root, Path::new("C:/work/repository/a.sql")), None);
        if cfg!(windows) {
            assert_eq!(
                rel_path(root, Path::new("c:/WORK/Repo/a.sql")).as_deref(),
                Some("a.sql")
            );
        }
    }

    #[test]
    fn abs_and_split() {
        let p = abs_path(Path::new("C:/r"), "a/b.sql");
        assert!(p.ends_with("a/b.sql") || p.ends_with("a\\b.sql"));
        assert_eq!(split_name("a/b/c.sql"), ("a/b", "c.sql"));
        assert_eq!(split_name("c.sql"), ("", "c.sql"));
    }
}
