//! What the panel asks git to do. Every command line is built by a plain function of its
//! inputs (tested below); `Repo` runs them.
use crate::branch::{self, BranchInfo};
use crate::diff::{self, FileDiff};
use crate::exec::{Cancel, Git, GitError};
use crate::log::{self, Commit, FileChange};
use crate::status::{self, Status};
use std::path::{Path, PathBuf};

/// `git pull` strategy (Settings › Git).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PullMode {
    #[default]
    FfOnly,
    Rebase,
    Merge,
}

impl PullMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "rebase" => PullMode::Rebase,
            "merge" => PullMode::Merge,
            _ => PullMode::FfOnly,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            PullMode::FfOnly => "ff-only",
            PullMode::Rebase => "rebase",
            PullMode::Merge => "merge",
        }
    }
}

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// Paths per command: Windows allows about 32k characters of arguments.
const CHUNK: usize = 100;

fn with_paths(base: &[&str], paths: &[String]) -> Vec<Vec<String>> {
    paths
        .chunks(CHUNK)
        .map(|c| {
            let mut a = v(base);
            a.push("--".into());
            a.extend(c.iter().cloned());
            a
        })
        .collect()
}

pub fn stage_args(paths: &[String]) -> Vec<Vec<String>> {
    with_paths(&["add"], paths)
}

pub fn stage_all_args() -> Vec<String> {
    v(&["add", "-A"])
}

/// `reset` also works before the first commit, where `restore --staged` does not.
pub fn unstage_args(paths: &[String]) -> Vec<Vec<String>> {
    with_paths(&["reset", "-q"], paths)
}

pub fn unstage_all_args() -> Vec<String> {
    v(&["reset", "-q"])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discard {
    /// Work tree back to the index.
    Worktree,
    /// Work tree and index back to HEAD.
    Both,
    /// Delete an untracked file.
    Untracked,
}

pub fn discard_args(paths: &[String], what: Discard) -> Vec<Vec<String>> {
    match what {
        Discard::Worktree => with_paths(&["restore"], paths),
        Discard::Both => with_paths(
            &["restore", "--source=HEAD", "--staged", "--worktree"],
            paths,
        ),
        Discard::Untracked => with_paths(&["clean", "-f", "-q"], paths),
    }
}

/// Never `--no-verify`: the user's hooks run.
pub fn commit_args(message: &str, amend: bool, all: bool) -> Vec<String> {
    let mut a = v(&["commit"]);
    if all {
        a.push("-a".into());
    }
    if amend {
        a.push("--amend".into());
        if message.trim().is_empty() {
            a.push("--no-edit".into());
            return a;
        }
    }
    a.push("-m".into());
    a.push(message.to_string());
    a
}

pub fn fetch_args() -> Vec<String> {
    v(&["fetch", "--prune"])
}

pub fn pull_args(mode: PullMode) -> Vec<String> {
    let mut a = v(&["pull"]);
    a.push(
        match mode {
            PullMode::FfOnly => "--ff-only",
            PullMode::Rebase => "--rebase",
            PullMode::Merge => "--no-rebase",
        }
        .into(),
    );
    a
}

/// A branch with no upstream is published to `origin` and tracks it afterwards.
pub fn push_args(branch: Option<&str>, has_upstream: bool) -> Vec<String> {
    match (has_upstream, branch) {
        (false, Some(b)) => v(&["push", "--set-upstream", "origin", b]),
        _ => v(&["push"]),
    }
}

pub fn switch_args(name: &str, remote: bool) -> Vec<String> {
    if remote {
        v(&["switch", "--track", name])
    } else {
        v(&["switch", name])
    }
}

pub fn create_branch_args(name: &str) -> Vec<String> {
    v(&["switch", "-c", name])
}

pub fn init_args() -> Vec<String> {
    v(&["init"])
}

/// Content of the optional `.gitignore` made with a new repository.
pub const GITIGNORE: &str = "*.env\n.locus/\n";

/// One repository: git, and the folder with `.git` in it.
#[derive(Debug, Clone)]
pub struct Repo {
    pub git: Git,
    pub root: PathBuf,
}

type R<T> = Result<T, GitError>;

impl Repo {
    pub fn new(git: Git, root: PathBuf) -> Self {
        Self { git, root }
    }

    fn run(&self, args: &[String], c: Option<&Cancel>) -> R<crate::exec::Output> {
        self.git.run(&self.root, args, c)
    }

    fn run_all(&self, cmds: Vec<Vec<String>>, c: Option<&Cancel>) -> R<()> {
        for a in cmds {
            self.run(&a, c)?;
        }
        Ok(())
    }

    /// The text git printed on stderr and stdout, for the progress box.
    fn say(o: crate::exec::Output) -> String {
        let e = crate::exec::tidy(&o.stderr);
        let e = e.trim();
        let s = crate::exec::tidy(&o.text());
        let s = s.trim();
        match (e.is_empty(), s.is_empty()) {
            (true, true) => String::new(),
            (false, true) => e.to_string(),
            (true, false) => s.to_string(),
            _ => format!("{e}\n{s}"),
        }
    }

    pub fn version(git: &Git) -> R<String> {
        let o = git.run(&std::env::temp_dir(), &v(&["--version"]), None)?;
        Ok(o.text().trim().to_string())
    }

    /// Creates a repository in `dir` (and the optional `.gitignore`).
    pub fn init(git: &Git, dir: &Path, gitignore: bool) -> R<Repo> {
        git.run(dir, &init_args(), None)?;
        if gitignore {
            let p = dir.join(".gitignore");
            if !p.exists() {
                std::fs::write(&p, GITIGNORE).map_err(|e| GitError::Io(e.to_string()))?;
            }
        }
        Ok(Repo::new(git.clone(), dir.to_path_buf()))
    }

    pub fn status(&self) -> R<Status> {
        let o = self.run(&status::args(), None)?;
        Ok(status::parse(&o.text()))
    }

    pub fn stage(&self, paths: &[String]) -> R<()> {
        self.run_all(stage_args(paths), None)
    }
    pub fn stage_all(&self) -> R<()> {
        self.run(&stage_all_args(), None).map(|_| ())
    }
    pub fn unstage(&self, paths: &[String]) -> R<()> {
        self.run_all(unstage_args(paths), None)
    }
    pub fn unstage_all(&self) -> R<()> {
        self.run(&unstage_all_args(), None).map(|_| ())
    }
    pub fn discard(&self, paths: &[String], what: Discard) -> R<()> {
        self.run_all(discard_args(paths, what), None)
    }

    pub fn commit(&self, message: &str, amend: bool, all: bool, c: &Cancel) -> R<String> {
        self.run(&commit_args(message, amend, all), Some(c))
            .map(Self::say)
    }

    pub fn fetch(&self, c: &Cancel) -> R<String> {
        self.run(&fetch_args(), Some(c)).map(Self::say)
    }
    pub fn pull(&self, mode: PullMode, c: &Cancel) -> R<String> {
        self.run(&pull_args(mode), Some(c)).map(Self::say)
    }
    pub fn push(&self, branch: Option<&str>, has_upstream: bool, c: &Cancel) -> R<String> {
        self.run(&push_args(branch, has_upstream), Some(c))
            .map(Self::say)
    }

    pub fn branches(&self) -> R<Vec<BranchInfo>> {
        Ok(branch::parse(&self.run(&branch::args(), None)?.text()))
    }
    pub fn switch(&self, name: &str, remote: bool) -> R<()> {
        self.run(&switch_args(name, remote), None).map(|_| ())
    }
    pub fn create_branch(&self, name: &str) -> R<()> {
        self.run(&create_branch_args(name), None).map(|_| ())
    }

    pub fn log(&self, skip: usize, count: usize, path: Option<&str>) -> R<Vec<Commit>> {
        // A repository without commits has no log: git exits 128 with "does not have any commits".
        match self.run(&log::args(skip, count, path), None) {
            Ok(o) => Ok(log::parse(&o.text())),
            Err(GitError::Failed { stderr, .. })
                if stderr.contains("does not have any commits") =>
            {
                Ok(vec![])
            }
            Err(e) => Err(e),
        }
    }
    pub fn commit_files(&self, hash: &str) -> R<Vec<FileChange>> {
        Ok(log::parse_files(
            &self.run(&log::files_args(hash), None)?.text(),
        ))
    }

    /// The diff of one file against the index (`staged = false`) or of the index against HEAD.
    /// An untracked file is shown as all added.
    pub fn diff_file(&self, path: &str, staged: bool, untracked: bool) -> R<Vec<FileDiff>> {
        if untracked {
            let bytes = std::fs::read(crate::root::abs_path(&self.root, path))
                .map_err(|e| GitError::Io(e.to_string()))?;
            let binary = bytes.iter().take(8000).any(|b| *b == 0);
            let text = String::from_utf8_lossy(&bytes);
            return Ok(vec![diff::untracked(path, &text, binary)]);
        }
        let args = if staged {
            diff::args_staged(Some(path))
        } else {
            diff::args_unstaged(Some(path))
        };
        Ok(diff::parse(&self.run(&args, None)?.text()))
    }

    /// Every change of tracked files not staged yet (what `commit -a` adds).
    pub fn diff_unstaged_all(&self) -> R<Vec<FileDiff>> {
        Ok(diff::parse(
            &self.run(&diff::args_unstaged(None), None)?.text(),
        ))
    }

    /// Every staged change (for the secret scan).
    pub fn diff_staged(&self) -> R<Vec<FileDiff>> {
        Ok(diff::parse(
            &self.run(&diff::args_staged(None), None)?.text(),
        ))
    }

    pub fn diff_commit(&self, hash: &str, path: Option<&str>) -> R<Vec<FileDiff>> {
        Ok(diff::parse(
            &self.run(&diff::args_commit(hash, path), None)?.text(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(a: &[&str]) -> Vec<String> {
        v(a)
    }

    #[test]
    fn paths_are_arguments_after_a_double_dash() {
        let cmds = stage_args(&s(&["a b.sql", "報表/月.sql", "--evil"]));
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], ["add", "--", "a b.sql", "報表/月.sql", "--evil"]);
    }

    #[test]
    fn many_paths_are_split() {
        let paths: Vec<String> = (0..250).map(|i| format!("f{i}.sql")).collect();
        let cmds = unstage_args(&paths);
        assert_eq!(cmds.len(), 3);
        assert!(cmds.iter().all(|c| c[..3] == ["reset", "-q", "--"]));
        assert_eq!(cmds.iter().map(|c| c.len() - 3).sum::<usize>(), 250);
    }

    #[test]
    fn discard_variants() {
        let p = s(&["x.sql"]);
        assert_eq!(
            discard_args(&p, Discard::Worktree)[0],
            ["restore", "--", "x.sql"]
        );
        assert_eq!(
            discard_args(&p, Discard::Both)[0],
            [
                "restore",
                "--source=HEAD",
                "--staged",
                "--worktree",
                "--",
                "x.sql"
            ]
        );
        assert_eq!(
            discard_args(&p, Discard::Untracked)[0],
            ["clean", "-f", "-q", "--", "x.sql"]
        );
    }

    #[test]
    fn commit_never_skips_hooks() {
        for (m, amend, all) in [
            ("msg", false, false),
            ("msg", true, true),
            ("", true, false),
            ("a\n\nb `x` \"y\"", false, true),
        ] {
            let a = commit_args(m, amend, all);
            assert!(!a.iter().any(|x| x == "--no-verify" || x == "-n"), "{a:?}");
        }
        assert_eq!(commit_args("hi", false, false), ["commit", "-m", "hi"]);
        assert_eq!(commit_args("hi", false, true), ["commit", "-a", "-m", "hi"]);
        assert_eq!(
            commit_args("", true, false),
            ["commit", "--amend", "--no-edit"]
        );
        // The message is one argument, newlines and quotes included.
        assert_eq!(commit_args("a\n\"b\"", false, false)[2], "a\n\"b\"");
    }

    #[test]
    fn pull_and_push() {
        assert_eq!(pull_args(PullMode::FfOnly).last().unwrap(), "--ff-only");
        assert_eq!(pull_args(PullMode::Rebase).last().unwrap(), "--rebase");
        assert_eq!(pull_args(PullMode::Merge).last().unwrap(), "--no-rebase");
        assert_eq!(PullMode::parse("rebase"), PullMode::Rebase);
        assert_eq!(PullMode::parse("anything"), PullMode::FfOnly);
        assert_eq!(push_args(Some("dev"), true), ["push"]);
        assert_eq!(
            push_args(Some("dev"), false),
            ["push", "--set-upstream", "origin", "dev"]
        );
        assert_eq!(push_args(None, false), ["push"]);
    }

    #[test]
    fn switching() {
        assert_eq!(switch_args("dev", false), ["switch", "dev"]);
        assert_eq!(
            switch_args("origin/dev", true),
            ["switch", "--track", "origin/dev"]
        );
        assert_eq!(create_branch_args("x"), ["switch", "-c", "x"]);
    }
}
