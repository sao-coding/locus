//! `git status --porcelain=v2 -z --branch` → branch info and changed files.

/// Arguments of the status command.
pub fn args() -> Vec<String> {
    [
        "status",
        "--porcelain=v2",
        "-z",
        "--branch",
        "--untracked-files=all",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Branch {
    pub oid: Option<String>,
    /// `None` while HEAD is detached.
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

/// One side (index or work tree) of a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    None,
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
}

impl Change {
    fn of(c: char) -> Change {
        match c {
            'M' => Change::Modified,
            'A' => Change::Added,
            'D' => Change::Deleted,
            'R' => Change::Renamed,
            'C' => Change::Copied,
            'T' => Change::TypeChanged,
            'U' => Change::Unmerged,
            _ => Change::None,
        }
    }

    pub fn letter(self) -> char {
        match self {
            Change::None => ' ',
            Change::Modified | Change::TypeChanged => 'M',
            Change::Added => 'A',
            Change::Deleted => 'D',
            Change::Renamed => 'R',
            Change::Copied => 'C',
            Change::Unmerged => 'U',
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ordinary,
    Renamed,
    Unmerged,
    Untracked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Relative to the repository root, `/` separated.
    pub path: String,
    /// The old path of a rename or copy.
    pub orig: Option<String>,
    pub kind: Kind,
    pub index: Change,
    pub worktree: Change,
}

impl Entry {
    pub fn is_conflict(&self) -> bool {
        self.kind == Kind::Unmerged
    }
    pub fn is_untracked(&self) -> bool {
        self.kind == Kind::Untracked
    }
    /// Has a change in the index (shown under Staged).
    pub fn is_staged(&self) -> bool {
        !self.is_conflict() && !self.is_untracked() && self.index != Change::None
    }
    /// Has a change in the work tree, or is untracked (shown under Changes).
    pub fn is_unstaged(&self) -> bool {
        !self.is_conflict() && (self.is_untracked() || self.worktree != Change::None)
    }
}

/// The marker a tab of this file wears: `C` conflict, `U` untracked, `A` added, `D` deleted,
/// `R` renamed, else `M`.
pub fn tab_letter(e: &Entry) -> char {
    if e.is_conflict() {
        'C'
    } else if e.is_untracked() {
        'U'
    } else if e.index == Change::Added || e.worktree == Change::Added {
        'A'
    } else if e.index == Change::Deleted || e.worktree == Change::Deleted {
        'D'
    } else if e.index == Change::Renamed {
        'R'
    } else {
        'M'
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: Branch,
    pub entries: Vec<Entry>,
}

impl Status {
    pub fn staged(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.is_staged())
    }
    pub fn unstaged(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.is_unstaged())
    }
    pub fn conflicts(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.is_conflict())
    }
    /// Files with any change (a file in both lists counts once).
    pub fn change_count(&self) -> usize {
        self.entries.len()
    }
    pub fn find(&self, path: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.path == path)
    }
}

/// Parses the `-z` output. Tolerates anything it does not know (new header lines).
pub fn parse(out: &str) -> Status {
    let mut st = Status::default();
    let mut it = out.split('\0');
    while let Some(rec) = it.next() {
        if rec.is_empty() {
            continue;
        }
        if let Some(h) = rec.strip_prefix("# ") {
            header(h, &mut st.branch);
            continue;
        }
        let mut chars = rec.chars();
        match chars.next() {
            Some('1') => {
                // 1 XY sub mH mI mW hH hI path
                let f: Vec<&str> = rec.splitn(9, ' ').collect();
                if f.len() == 9 {
                    st.entries.push(entry(f[8], None, Kind::Ordinary, f[1]));
                }
            }
            Some('2') => {
                // 2 XY sub mH mI mW hH hI Xscore path NUL origPath
                let f: Vec<&str> = rec.splitn(10, ' ').collect();
                let orig = it.next().unwrap_or("");
                if f.len() == 10 {
                    st.entries
                        .push(entry(f[9], Some(orig.to_string()), Kind::Renamed, f[1]));
                }
            }
            Some('u') => {
                // u XY sub m1 m2 m3 mW h1 h2 h3 path
                let f: Vec<&str> = rec.splitn(11, ' ').collect();
                if f.len() == 11 {
                    st.entries.push(entry(f[10], None, Kind::Unmerged, f[1]));
                }
            }
            Some('?') => {
                if let Some(p) = rec.strip_prefix("? ") {
                    st.entries.push(Entry {
                        path: p.to_string(),
                        orig: None,
                        kind: Kind::Untracked,
                        index: Change::None,
                        worktree: Change::Added,
                    });
                }
            }
            _ => {}
        }
    }
    st
}

fn entry(path: &str, orig: Option<String>, kind: Kind, xy: &str) -> Entry {
    let mut c = xy.chars();
    let x = c.next().unwrap_or('.');
    let y = c.next().unwrap_or('.');
    let (index, worktree) = if kind == Kind::Unmerged {
        (Change::Unmerged, Change::Unmerged)
    } else {
        (Change::of(x), Change::of(y))
    };
    Entry {
        path: path.to_string(),
        orig,
        kind,
        index,
        worktree,
    }
}

fn header(h: &str, b: &mut Branch) {
    if let Some(v) = h.strip_prefix("branch.oid ") {
        b.oid = (v != "(initial)").then(|| v.to_string());
    } else if let Some(v) = h.strip_prefix("branch.head ") {
        b.head = (v != "(detached)").then(|| v.to_string());
    } else if let Some(v) = h.strip_prefix("branch.upstream ") {
        b.upstream = Some(v.to_string());
    } else if let Some(v) = h.strip_prefix("branch.ab ") {
        for w in v.split_whitespace() {
            if let Some(n) = w.strip_prefix('+') {
                b.ahead = n.parse().unwrap_or(0);
            } else if let Some(n) = w.strip_prefix('-') {
                b.behind = n.parse().unwrap_or(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "0123456789abcdef0123456789abcdef01234567";

    fn z(lines: &[&str]) -> String {
        let mut s = lines.join("\0");
        s.push('\0');
        s
    }

    #[test]
    fn branch_headers() {
        let out = z(&[
            &format!("# branch.oid {OID}"),
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -3",
        ]);
        let s = parse(&out);
        assert_eq!(s.branch.head.as_deref(), Some("main"));
        assert_eq!(s.branch.upstream.as_deref(), Some("origin/main"));
        assert_eq!((s.branch.ahead, s.branch.behind), (2, 3));
        assert_eq!(s.branch.oid.as_deref(), Some(OID));
        assert!(s.entries.is_empty());
    }

    #[test]
    fn initial_and_detached() {
        let s = parse(&z(&["# branch.oid (initial)", "# branch.head (detached)"]));
        assert_eq!(s.branch.oid, None);
        assert_eq!(s.branch.head, None);
    }

    #[test]
    fn ordinary_entries_and_staging_sides() {
        let out = z(&[
            "# branch.head main",
            "1 M. N... 100644 100644 100644 aaa bbb staged.sql",
            "1 .M N... 100644 100644 100644 aaa bbb worktree.sql",
            "1 MM N... 100644 100644 100644 aaa bbb both.sql",
            "1 A. N... 000000 100644 100644 000 bbb new.sql",
            "1 .D N... 100644 100644 000000 aaa aaa gone.sql",
            "? untracked.sql",
        ]);
        let s = parse(&out);
        assert_eq!(s.entries.len(), 6);
        let staged: Vec<_> = s.staged().map(|e| e.path.as_str()).collect();
        assert_eq!(staged, ["staged.sql", "both.sql", "new.sql"]);
        let un: Vec<_> = s.unstaged().map(|e| e.path.as_str()).collect();
        assert_eq!(
            un,
            ["worktree.sql", "both.sql", "gone.sql", "untracked.sql"]
        );
        assert_eq!(tab_letter(s.find("new.sql").unwrap()), 'A');
        assert_eq!(tab_letter(s.find("untracked.sql").unwrap()), 'U');
        assert_eq!(tab_letter(s.find("worktree.sql").unwrap()), 'M');
        assert_eq!(tab_letter(s.find("gone.sql").unwrap()), 'D');
    }

    #[test]
    fn cjk_names_and_spaces_survive() {
        let out = z(&[
            "1 .M N... 100644 100644 100644 aaa bbb 報表/月 報.sql",
            "? 新 增 檔案.sql",
        ]);
        let s = parse(&out);
        assert_eq!(s.entries[0].path, "報表/月 報.sql");
        assert_eq!(s.entries[1].path, "新 增 檔案.sql");
    }

    #[test]
    fn rename_takes_the_next_record_as_the_old_path() {
        let out = z(&[
            "2 R. N... 100644 100644 100644 aaa bbb R100 new name.sql",
            "old name.sql",
            "? after.sql",
        ]);
        let s = parse(&out);
        assert_eq!(s.entries.len(), 2);
        let r = &s.entries[0];
        assert_eq!(r.path, "new name.sql");
        assert_eq!(r.orig.as_deref(), Some("old name.sql"));
        assert_eq!(r.index, Change::Renamed);
        assert!(r.is_staged());
        assert_eq!(s.entries[1].path, "after.sql");
    }

    #[test]
    fn conflicts_are_neither_staged_nor_unstaged() {
        let out = z(&["u UU N... 100644 100644 100644 100644 aaa bbb ccc clash.sql"]);
        let s = parse(&out);
        let e = &s.entries[0];
        assert!(e.is_conflict());
        assert!(!e.is_staged() && !e.is_unstaged());
        assert_eq!(s.conflicts().count(), 1);
        assert_eq!(tab_letter(e), 'C');
    }

    #[test]
    fn unknown_records_are_skipped() {
        let s = parse(&z(&["# branch.future x", "! ignored.log", "9 what"]));
        assert!(s.entries.is_empty());
    }
}
