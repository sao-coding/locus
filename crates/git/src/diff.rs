//! Unified diff text → files, hunks and numbered lines.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Del,
    /// `\ No newline at end of file`.
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The `@@ -1,3 +1,4 @@ section` line.
    pub header: String,
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    /// The name to show: the new path, else the old one.
    pub fn path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or("")
    }
    pub fn added(&self) -> usize {
        self.count(LineKind::Add)
    }
    pub fn removed(&self) -> usize {
        self.count(LineKind::Del)
    }
    fn count(&self, k: LineKind) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == k)
            .count()
    }
}

fn strs(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// Work tree against the index (what is not staged yet).
pub fn args_unstaged(path: Option<&str>) -> Vec<String> {
    with_path(strs(&["diff", "--no-color", "--no-ext-diff", "-U3"]), path)
}

/// Index against HEAD (what the next commit holds).
pub fn args_staged(path: Option<&str>) -> Vec<String> {
    with_path(
        strs(&["diff", "--cached", "--no-color", "--no-ext-diff", "-U3"]),
        path,
    )
}

/// What one commit changed (optionally one file of it).
pub fn args_commit(hash: &str, path: Option<&str>) -> Vec<String> {
    let mut a = strs(&[
        "show",
        "--no-color",
        "--no-ext-diff",
        "--format=",
        "-U3",
        "-M",
        hash,
    ]);
    if let Some(p) = path {
        a.push("--".into());
        a.push(p.to_string());
    }
    a
}

fn with_path(mut a: Vec<String>, path: Option<&str>) -> Vec<String> {
    if let Some(p) = path {
        a.push("--".into());
        a.push(p.to_string());
    }
    a
}

/// The diff of a new, untracked file: every line added.
pub fn untracked(path: &str, text: &str, binary: bool) -> FileDiff {
    if binary {
        return FileDiff {
            old_path: None,
            new_path: Some(path.to_string()),
            binary: true,
            hunks: vec![],
        };
    }
    let lines: Vec<Line> = text
        .lines()
        .enumerate()
        .map(|(i, l)| Line {
            kind: LineKind::Add,
            old_no: None,
            new_no: Some(i as u32 + 1),
            text: l.to_string(),
        })
        .collect();
    FileDiff {
        old_path: None,
        new_path: Some(path.to_string()),
        binary: false,
        hunks: if lines.is_empty() {
            vec![]
        } else {
            vec![Hunk {
                header: format!("@@ -0,0 +1,{} @@", lines.len()),
                lines,
            }]
        },
    }
}

/// Undoes git's C-style quoting of a path (`"a\tb"`, `"\346\226..."`).
pub fn unquote(s: &str) -> String {
    let s = s.trim_end_matches(['\t', '\r']);
    let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) else {
        return s.to_string();
    };
    let b = inner.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' || i + 1 >= b.len() {
            out.push(b[i]);
            i += 1;
            continue;
        }
        i += 1;
        match b[i] {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'"' => out.push(b'"'),
            b'\\' => out.push(b'\\'),
            c @ b'0'..=b'7' => {
                let mut v = (c - b'0') as u32;
                let mut n = 1;
                while n < 3 && i + 1 < b.len() && (b'0'..=b'7').contains(&b[i + 1]) {
                    v = v * 8 + (b[i + 1] - b'0') as u32;
                    i += 1;
                    n += 1;
                }
                out.push(v as u8);
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn strip_ab(p: &str) -> Option<String> {
    if p == "/dev/null" {
        return None;
    }
    let u = unquote(p);
    Some(
        u.strip_prefix("a/")
            .or_else(|| u.strip_prefix("b/"))
            .unwrap_or(&u)
            .to_string(),
    )
}

/// Parses `git diff` / `git show` output. Several files in, several `FileDiff`s out.
pub fn parse(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut cur: Option<FileDiff> = None;
    let (mut old_no, mut new_no) = (0u32, 0u32);
    let mut in_hunk = false;
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(f) = cur.take() {
                files.push(f);
            }
            // Fallback names from the header; `---` / `+++` / rename lines refine them.
            let (a, b) = split_header(rest);
            cur = Some(FileDiff {
                old_path: Some(a),
                new_path: Some(b),
                ..Default::default()
            });
            in_hunk = false;
            continue;
        }
        let Some(f) = cur.as_mut() else { continue };
        if !in_hunk {
            if let Some(p) = line.strip_prefix("--- ") {
                f.old_path = strip_ab(p);
                continue;
            }
            if let Some(p) = line.strip_prefix("+++ ") {
                f.new_path = strip_ab(p);
                continue;
            }
            if let Some(p) = line.strip_prefix("rename from ") {
                f.old_path = Some(unquote(p));
                continue;
            }
            if let Some(p) = line.strip_prefix("rename to ") {
                f.new_path = Some(unquote(p));
                continue;
            }
            if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
                f.binary = true;
                continue;
            }
        }
        if line.starts_with("@@") {
            let (o, n) = hunk_starts(line);
            old_no = o;
            new_no = n;
            in_hunk = true;
            f.hunks.push(Hunk {
                header: line.to_string(),
                lines: vec![],
            });
            continue;
        }
        if !in_hunk {
            continue;
        }
        let Some(h) = f.hunks.last_mut() else {
            continue;
        };
        let (kind, body) = match line.as_bytes().first() {
            Some(b'+') => (LineKind::Add, &line[1..]),
            Some(b'-') => (LineKind::Del, &line[1..]),
            Some(b' ') => (LineKind::Context, &line[1..]),
            Some(b'\\') => (LineKind::Note, line),
            // The empty line that ends the output (or a context line whose space was trimmed).
            None => continue,
            _ => continue,
        };
        let (o, n) = match kind {
            LineKind::Context => {
                let r = (Some(old_no), Some(new_no));
                old_no += 1;
                new_no += 1;
                r
            }
            LineKind::Add => {
                let r = (None, Some(new_no));
                new_no += 1;
                r
            }
            LineKind::Del => {
                let r = (Some(old_no), None);
                old_no += 1;
                r
            }
            LineKind::Note => (None, None),
        };
        h.lines.push(Line {
            kind,
            old_no: o,
            new_no: n,
            text: body.to_string(),
        });
    }
    if let Some(f) = cur {
        files.push(f);
    }
    files
}

/// `a/x b/y` (maybe quoted) → (x, y). Ambiguous when names hold " b/"; the `---` lines fix it.
fn split_header(rest: &str) -> (String, String) {
    if rest.starts_with('"') {
        // "a/..." "b/..." : find the closing quote of the first name.
        let b = rest.as_bytes();
        let mut i = 1;
        while i < b.len() {
            if b[i] == b'\\' {
                i += 2;
                continue;
            }
            if b[i] == b'"' {
                break;
            }
            i += 1;
        }
        let first = &rest[..(i + 1).min(rest.len())];
        let second = rest.get(i + 2..).unwrap_or("");
        return (
            strip_ab(first).unwrap_or_default(),
            strip_ab(second).unwrap_or_default(),
        );
    }
    let half = rest.len() / 2;
    // With equal names (no rename) the two halves are identical apart from a/ and b/.
    if let (Some(a), Some(b)) = (rest.get(..half), rest.get(half + 1..))
        && rest.as_bytes().get(half) == Some(&b' ')
        && a.strip_prefix("a/") == b.strip_prefix("b/")
    {
        return (
            strip_ab(a).unwrap_or_default(),
            strip_ab(b).unwrap_or_default(),
        );
    }
    match rest.find(" b/") {
        Some(i) => (
            strip_ab(&rest[..i]).unwrap_or_default(),
            strip_ab(&rest[i + 1..]).unwrap_or_default(),
        ),
        None => (String::new(), String::new()),
    }
}

/// `@@ -12,3 +14,4 @@` → (12, 14).
fn hunk_starts(h: &str) -> (u32, u32) {
    let mut o = 0;
    let mut n = 0;
    for w in h.split_whitespace().skip(1).take(2) {
        if let Some(v) = w.strip_prefix('-') {
            o = v
                .split(',')
                .next()
                .and_then(|x| x.parse().ok())
                .unwrap_or(0);
        } else if let Some(v) = w.strip_prefix('+') {
            n = v
                .split(',')
                .next()
                .and_then(|x| x.parse().ok())
                .unwrap_or(0);
        }
    }
    (o, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE: &str = "diff --git a/q.sql b/q.sql\nindex 1..2 100644\n--- a/q.sql\n+++ b/q.sql\n@@ -1,3 +1,4 @@ SELECT\n select 1;\n-from a;\n+from b;\n+where x;\n end;\n";

    #[test]
    fn numbers_lines() {
        let f = parse(SIMPLE);
        assert_eq!(f.len(), 1);
        let f = &f[0];
        assert_eq!(f.path(), "q.sql");
        assert_eq!((f.added(), f.removed()), (2, 1));
        let l = &f.hunks[0].lines;
        assert_eq!((l[0].old_no, l[0].new_no), (Some(1), Some(1)));
        assert_eq!((l[1].old_no, l[1].new_no), (Some(2), None));
        assert_eq!((l[2].old_no, l[2].new_no), (None, Some(2)));
        assert_eq!((l[3].old_no, l[3].new_no), (None, Some(3)));
        assert_eq!((l[4].old_no, l[4].new_no), (Some(3), Some(4)));
        assert_eq!(l[2].text, "from b;");
    }

    #[test]
    fn a_dash_dash_line_inside_a_hunk_is_content() {
        // `-- comment` removed shows as `--- comment`; it is not a file header.
        let t = "diff --git a/x.sql b/x.sql\n--- a/x.sql\n+++ b/x.sql\n@@ -1,2 +1,2 @@\n--- old comment\n+-- new comment\n select 1;\n";
        let f = &parse(t)[0];
        assert_eq!(f.path(), "x.sql");
        assert_eq!(f.hunks[0].lines[0].kind, LineKind::Del);
        assert_eq!(f.hunks[0].lines[0].text, "-- old comment");
        assert_eq!(f.hunks[0].lines[1].text, "-- new comment");
    }

    #[test]
    fn rename_and_cjk() {
        let t = "diff --git a/舊 名.sql b/新 名.sql\nsimilarity index 90%\nrename from 舊 名.sql\nrename to 新 名.sql\nindex 1..2 100644\n--- a/舊 名.sql\n+++ b/新 名.sql\n@@ -1 +1 @@\n-查詢\n+查詢全部\n";
        let f = &parse(t)[0];
        assert_eq!(f.old_path.as_deref(), Some("舊 名.sql"));
        assert_eq!(f.new_path.as_deref(), Some("新 名.sql"));
        assert_eq!(f.hunks[0].lines[1].text, "查詢全部");
    }

    #[test]
    fn quoted_paths() {
        assert_eq!(unquote("\"a\\tb\""), "a\tb");
        assert_eq!(unquote("\"\\346\\237\\245.sql\""), "查.sql");
        assert_eq!(unquote("plain.sql\t"), "plain.sql");
        let t = "diff --git \"a/x\\ty.sql\" \"b/x\\ty.sql\"\n--- \"a/x\\ty.sql\"\n+++ \"b/x\\ty.sql\"\n@@ -1 +1 @@\n-a\n+b\n";
        assert_eq!(parse(t)[0].path(), "x\ty.sql");
    }

    #[test]
    fn new_deleted_and_binary() {
        let t = "diff --git a/n.sql b/n.sql\nnew file mode 100644\nindex 0..1\n--- /dev/null\n+++ b/n.sql\n@@ -0,0 +1,2 @@\n+a\n+b\ndiff --git a/d.sql b/d.sql\ndeleted file mode 100644\n--- a/d.sql\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\ndiff --git a/i.png b/i.png\nBinary files a/i.png and b/i.png differ\n";
        let f = parse(t);
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].old_path, None);
        assert_eq!(f[0].path(), "n.sql");
        assert_eq!(f[1].new_path, None);
        assert_eq!(f[1].path(), "d.sql");
        assert!(f[2].binary && f[2].hunks.is_empty());
    }

    #[test]
    fn no_newline_note_and_crlf() {
        let t = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\r\n-a\r\n\\ No newline at end of file\n+b\r\n";
        let l = &parse(t)[0].hunks[0].lines;
        assert_eq!(l[0].text, "a");
        assert_eq!(l[1].kind, LineKind::Note);
        assert_eq!(l[2].new_no, Some(1));
    }

    #[test]
    fn untracked_file_is_all_added() {
        let f = untracked("n.sql", "a\nb\n", false);
        assert_eq!(f.added(), 2);
        assert_eq!(f.hunks[0].lines[1].new_no, Some(2));
        assert!(untracked("i.png", "", true).binary);
    }

    #[test]
    fn command_lines() {
        let a = args_staged(Some("a b.sql"));
        assert_eq!(a[..2], ["diff", "--cached"]);
        assert_eq!(a[a.len() - 2..], ["--", "a b.sql"]);
        let a = args_commit("abc123", None);
        assert_eq!(a.last().unwrap(), "abc123");
    }
}
