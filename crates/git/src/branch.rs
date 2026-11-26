//! `git branch` listing.

const FS: char = '\x1f';

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    /// `main`, or `origin/feature` for a remote branch.
    pub name: String,
    pub current: bool,
    pub remote: bool,
    pub upstream: Option<String>,
}

pub fn args() -> Vec<String> {
    vec![
        "branch".into(),
        "-a".into(),
        format!("--format=%(HEAD){FS}%(refname){FS}%(upstream:short)"),
    ]
}

/// Local branches first, then remote branches that have no local branch of the same name
/// (`origin/HEAD` is dropped).
pub fn parse(out: &str) -> Vec<BranchInfo> {
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for line in out.lines() {
        let f: Vec<&str> = line.split(FS).collect();
        if f.len() < 2 {
            continue;
        }
        let current = f[0].trim() == "*";
        let up = f.get(2).map(|s| s.trim()).filter(|s| !s.is_empty());
        if let Some(n) = f[1].strip_prefix("refs/heads/") {
            local.push(BranchInfo {
                name: n.to_string(),
                current,
                remote: false,
                upstream: up.map(String::from),
            });
        } else if let Some(n) = f[1].strip_prefix("refs/remotes/") {
            if n.ends_with("/HEAD") {
                continue;
            }
            remote.push(BranchInfo {
                name: n.to_string(),
                current: false,
                remote: true,
                upstream: None,
            });
        }
    }
    let have: Vec<String> = local.iter().map(|b| b.name.clone()).collect();
    remote.retain(|r| {
        let short = r.name.split_once('/').map(|x| x.1).unwrap_or(&r.name);
        !have.iter().any(|h| h == short)
    });
    local.extend(remote);
    local
}

/// A branch name git accepts (a conservative subset of `git check-ref-format`).
pub fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && !n.starts_with(['-', '/', '.'])
        && !n.ends_with(['/', '.'])
        && !n.ends_with(".lock")
        && !n.contains("..")
        && !n.contains("//")
        && !n.contains("@{")
        && n != "@"
        && !n
            .chars()
            .any(|c| c.is_control() || c == ' ' || "~^:?*[\\".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_first_and_remote_dedup() {
        let out = format!(
            "*{FS}refs/heads/main{FS}origin/main\n {FS}refs/heads/dev{FS}\n {FS}refs/remotes/origin/HEAD{FS}\n {FS}refs/remotes/origin/main{FS}\n {FS}refs/remotes/origin/feature{FS}\n"
        );
        let b = parse(&out);
        let names: Vec<_> = b.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["main", "dev", "origin/feature"]);
        assert!(b[0].current && b[0].upstream.as_deref() == Some("origin/main"));
        assert!(b[2].remote);
    }

    #[test]
    fn names() {
        for ok in ["feature/報表", "fix-1", "v1.2"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "", "-x", "a b", "a..b", "a.lock", "/a", "a/", "a~1", "x@{1}", "a:b",
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }
}
