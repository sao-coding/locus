//! The Git panel: what the Space's folder says about its repository, and the commands that
//! change it. The engine (process wrapper, parsers, secret scan) is the `locus-git` crate;
//! this module is the model (`GitPanel`, an entity next to the shell), `panel.rs` draws it
//! in the right panel, `diffpage.rs` is the diff tab and `glue.rs` ties it to the shell
//! (tab decoration, status bar, palette commands).
//!
//! Every git call runs on the tokio runtime (`Locus::pool.rt()`), never on the UI thread.
//! A refresh runs after a folder change, a save, a command of the panel, when the window
//! is activated and (debounced) when files of the work tree change; nothing polls.
pub(crate) mod diffpage;
pub(crate) mod glue;
pub(crate) mod panel;

use crate::shell::*;
use gpui_kit::component::input::TextareaState;
use locus_git::log::{Commit, FileChange};
use locus_git::ops::Discard;
use locus_git::status::{Status, tab_letter};
use locus_git::{Cancel, Git, GitError, PullMode, Repo, root, secrets};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// How many commits one page of History loads.
const PAGE: usize = 50;

pub(crate) enum GitEvent {
    /// Status, branch or the repository itself changed: repaint tab letters and the status bar.
    Changed,
    OpenFile(PathBuf),
    OpenDiff(DiffSpec),
    BindFolder,
    /// "File history": the shell knows the active tab.
    FileHistory,
}

impl EventEmitter<GitEvent> for GitPanel {}

/// What a diff tab shows. Saved in the tab (`TabData::sql`), so it returns after a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DiffSpec {
    pub root: String,
    pub path: String,
    pub what: DiffWhat,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum DiffWhat {
    Unstaged,
    Staged,
    Untracked,
    /// One commit; an empty `path` is the whole commit.
    Commit {
        hash: String,
        short: String,
    },
}

impl DiffSpec {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }
}

/// A running command the user can see and stop.
pub(crate) struct Busy {
    pub label: String,
    pub cancel: Cancel,
}

/// The box under the header: what git said.
pub(crate) struct Notice {
    pub text: String,
    pub error: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum View {
    Changes,
    History,
}

/// Which of the panel's states applies.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Phase {
    NoFolder,
    GitMissing,
    NoRepo,
    Ready,
}

pub(crate) struct GitPanel {
    rt: tokio::runtime::Handle,
    /// The Space's folder.
    folder: Option<String>,
    /// The repository root found above it.
    pub(crate) root: Option<PathBuf>,
    /// The program that could not be started (shown in the "git not found" state).
    missing: Option<String>,
    pub(crate) status: Option<Status>,
    /// The flat list of the Changes view (`panel::GitPanel::rebuild_rows`).
    pub(crate) rows: Vec<panel::Row>,
    /// "Initialize repository" also writes a `.gitignore`.
    pub(crate) init_gitignore: bool,
    /// Lower-case relative path -> tab letter.
    letters: HashMap<String, char>,
    /// Bumped whenever the status changed; diff tabs reload on it.
    pub(crate) rev: u64,
    refreshing: bool,
    again: bool,
    pub(crate) busy: Option<Busy>,
    pub(crate) notice: Option<Notice>,
    pub(crate) msg: Entity<TextareaState>,
    pub(crate) amend: bool,
    pub(crate) view: View,
    pub(crate) commits: Vec<Commit>,
    pub(crate) commits_end: bool,
    commits_loading: bool,
    /// History of one file instead of the repository.
    pub(crate) file_history: Option<String>,
    pub(crate) sel_commit: Option<usize>,
    pub(crate) sel_files: Vec<FileChange>,
    pub(crate) scroll: UniformListScrollHandle,
    pub(crate) hist_scroll: UniformListScrollHandle,
    pub(crate) focus: FocusHandle,
    /// Git path / pull mode as last read from Settings (a change re-checks git).
    cfg_path: String,
    watch: Option<notify::RecommendedWatcher>,
    _tasks: Vec<Task<()>>,
    last_fetch: std::time::Instant,
    /// Palette "Commit": focus the box once it is drawn.
    pub(crate) focus_msg: bool,
}

type R<T> = Result<T, GitError>;

impl GitPanel {
    pub fn new(rt: tokio::runtime::Handle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let msg = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .placeholder(t!("git.msg_placeholder").to_string())
        });
        let mut me = Self {
            rt,
            folder: None,
            root: None,
            missing: None,
            status: None,
            rows: Vec::new(),
            init_gitignore: true,
            letters: HashMap::new(),
            rev: 0,
            refreshing: false,
            again: false,
            busy: None,
            notice: None,
            msg,
            amend: false,
            view: View::Changes,
            commits: Vec::new(),
            commits_end: false,
            commits_loading: false,
            file_history: None,
            sel_commit: None,
            sel_files: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            hist_scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            cfg_path: String::new(),
            watch: None,
            _tasks: Vec::new(),
            last_fetch: std::time::Instant::now(),
            focus_msg: false,
        };
        me.start_auto_fetch(cx);
        me
    }

    // ---- state -------------------------------------------------------------

    pub(crate) fn phase(&self) -> Phase {
        if self.folder.is_none() {
            Phase::NoFolder
        } else if self.missing.is_some() {
            Phase::GitMissing
        } else if self.root.is_none() {
            Phase::NoRepo
        } else {
            Phase::Ready
        }
    }

    pub(crate) fn missing_program(&self) -> String {
        self.missing.clone().unwrap_or_else(|| "git".into())
    }

    fn git(&self, cx: &App) -> Git {
        Git::with_path(&crate::settings::get(cx).git_path())
    }

    fn repo(&self, cx: &App) -> Option<Repo> {
        Some(Repo::new(self.git(cx), self.root.clone()?))
    }

    /// Current branch name, or `None` (no repository, or a detached HEAD).
    pub(crate) fn branch(&self) -> Option<&str> {
        self.status.as_ref()?.branch.head.as_deref()
    }

    /// `branch · N changes` for the status bar; `None` without a repository.
    pub(crate) fn summary(&self) -> Option<(String, usize)> {
        let st = self.status.as_ref()?;
        self.root.as_ref()?;
        Some((
            st.branch
                .head
                .clone()
                .unwrap_or_else(|| t!("git.detached").into_owned()),
            st.change_count(),
        ))
    }

    /// The letter a tab of this file wears (`M`, `A`, `U`, `C`, ...), if the file is changed.
    pub(crate) fn letter_for(&self, file: &std::path::Path) -> Option<char> {
        let root = self.root.as_ref()?;
        let rel = root::rel_path(root, file)?;
        self.letters.get(&rel.to_lowercase()).copied()
    }

    pub(crate) fn rel_of(&self, file: &std::path::Path) -> Option<String> {
        root::rel_path(self.root.as_ref()?, file)
    }

    pub(crate) fn staged_count(&self) -> usize {
        self.status.as_ref().map_or(0, |s| s.staged().count())
    }

    /// Changes `commit -a` would take (tracked, not staged).
    pub(crate) fn tracked_unstaged_count(&self) -> usize {
        self.status
            .as_ref()
            .map_or(0, |s| s.unstaged().filter(|e| !e.is_untracked()).count())
    }

    /// The commit message box has the keyboard focus.
    pub(crate) fn msg_focused(&self, window: &Window, cx: &App) -> bool {
        self.msg
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
    }

    pub(crate) fn can_commit(&self, cx: &App) -> bool {
        let msg = !self.msg.read(cx).value().trim().is_empty();
        let has = self.staged_count() > 0 || self.tracked_unstaged_count() > 0;
        self.busy.is_none() && (self.amend || (msg && has))
    }

    // ---- folder and refresh --------------------------------------------------

    /// The Space changed or its folder was (un)bound.
    pub(crate) fn set_folder(&mut self, folder: Option<String>, cx: &mut Context<Self>) {
        if self.folder == folder {
            return;
        }
        self.folder = folder;
        self.reset_repo_state();
        self.refresh(cx);
    }

    fn reset_repo_state(&mut self) {
        self.root = None;
        self.status = None;
        self.rows.clear();
        self.letters.clear();
        self.commits.clear();
        self.commits_end = false;
        self.sel_commit = None;
        self.sel_files.clear();
        self.file_history = None;
        self.notice = None;
        self.watch = None;
        self.rev += 1;
    }

    /// Reads the status in the background. Calls that arrive meanwhile become one more run.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refreshing {
            self.again = true;
            return;
        }
        let Some(folder) = self.folder.clone() else {
            self.finish_refresh(None, Ok(None), cx);
            return;
        };
        let path = crate::settings::get(cx).git_path();
        if path != self.cfg_path {
            self.cfg_path = path;
            self.missing = None;
        }
        self.refreshing = true;
        let git = self.git(cx);
        let job = self
            .rt
            .spawn_blocking(move || -> (Option<PathBuf>, R<Option<Status>>) {
                let dir = PathBuf::from(&folder);
                let root = root::find_root(&dir);
                match &root {
                    Some(r) => {
                        let st = Repo::new(git, r.clone()).status().map(Some);
                        (root, st)
                    }
                    // No repository: is git there at all?
                    None => (None, Repo::version(&git).map(|_| None)),
                }
            });
        cx.spawn(async move |this, cx| {
            let (root, res) = job
                .await
                .unwrap_or_else(|e| (None, Err(GitError::Io(e.to_string()))));
            let _ = this.update(cx, |this, cx| this.finish_refresh(root, res, cx));
        })
        .detach();
    }

    fn finish_refresh(
        &mut self,
        root: Option<PathBuf>,
        res: R<Option<Status>>,
        cx: &mut Context<Self>,
    ) {
        self.refreshing = false;
        let root_changed = self.root != root;
        self.root = root;
        match res {
            Ok(st) => {
                self.missing = None;
                if let Some(st) = &st {
                    self.letters = st
                        .entries
                        .iter()
                        .map(|e| (e.path.to_lowercase(), tab_letter(e)))
                        .collect();
                } else {
                    self.letters.clear();
                }
                if self.status != st {
                    self.rev += 1;
                }
                self.status = st;
            }
            Err(GitError::NotInstalled(p)) => {
                self.missing = Some(p);
                self.status = None;
                self.letters.clear();
                self.rev += 1;
            }
            Err(e) => {
                // A repository that git cannot read (broken, dubious ownership): say so.
                self.status = None;
                self.letters.clear();
                self.set_error(e, cx);
            }
        }
        if root_changed {
            self.commits.clear();
            self.commits_end = false;
            self.sel_commit = None;
            self.sel_files.clear();
            self.start_watch(cx);
            if self.view == View::History {
                self.load_commits(true, cx);
            }
        }
        self.rebuild_rows();
        cx.emit(GitEvent::Changed);
        cx.notify();
        if self.again {
            self.again = false;
            self.refresh(cx);
        }
    }

    /// Watches the work tree: a burst of file events becomes one refresh after half a second.
    fn start_watch(&mut self, cx: &mut Context<Self>) {
        use notify::{EventKind, RecursiveMode, Watcher};
        self.watch = None;
        let Some(root) = self.root.clone() else {
            return;
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let skip = root.clone();
        let w = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
            let Ok(ev) = ev else { return };
            if matches!(ev.kind, EventKind::Access(_)) {
                return;
            }
            // `.git` changes with every command of ours; those refresh explicitly.
            let outside = ev.paths.iter().any(|p| {
                root::rel_path(&skip, p).is_some_and(|r| r != ".git" && !r.starts_with(".git/"))
            });
            if outside {
                let _ = tx.send(());
            }
        });
        let Ok(mut w) = w else { return };
        if w.watch(&root, RecursiveMode::Recursive).is_err() {
            return;
        }
        self.watch = Some(w);
        let task = cx.spawn(async move |this, cx| {
            while rx.recv().await.is_some() {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                while rx.try_recv().is_ok() {}
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        });
        // The watcher task replaces the previous one.
        self._tasks.truncate(1);
        self._tasks.push(task);
    }

    /// Settings › Git › auto-fetch: one timer for the panel's life; it asks the setting each minute.
    fn start_auto_fetch(&mut self, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(60))
                    .await;
                let ok = this
                    .update(cx, |this, cx| {
                        let mins = crate::settings::get(cx).git_auto_fetch_minutes();
                        if mins > 0
                            && this.root.is_some()
                            && this.busy.is_none()
                            && this.last_fetch.elapsed() >= Duration::from_secs(mins * 60)
                        {
                            this.last_fetch = std::time::Instant::now();
                            this.fetch(true, cx);
                        }
                    })
                    .is_ok();
                if !ok {
                    break;
                }
            }
        });
        self._tasks.push(task);
    }

    // ---- running commands ----------------------------------------------------

    pub(crate) fn set_error(&mut self, e: GitError, cx: &mut Context<Self>) {
        match e {
            GitError::NotInstalled(p) => self.missing = Some(p),
            GitError::Cancelled => {
                self.notice = Some(Notice {
                    text: t!("git.cancelled").into_owned(),
                    error: false,
                })
            }
            GitError::Timeout => {
                self.notice = Some(Notice {
                    text: t!("git.timeout").into_owned(),
                    error: true,
                })
            }
            e => {
                self.notice = Some(Notice {
                    text: e.to_string(),
                    error: true,
                })
            }
        }
        cx.notify();
    }

    /// Runs `job` off the UI thread. `label` shows the progress box with a Cancel button.
    /// On success `then` runs on the UI thread; on failure git's words go to the notice box.
    fn run<T: Send + 'static>(
        &mut self,
        label: Option<String>,
        job: impl FnOnce(Repo, Cancel) -> R<T> + Send + 'static,
        then: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repo(cx) else {
            return;
        };
        let cancel = Cancel::new();
        if let Some(label) = label {
            self.busy = Some(Busy {
                label,
                cancel: cancel.clone(),
            });
            self.notice = None;
        }
        let tracked = self.busy.is_some();
        cx.notify();
        let handle = self.rt.spawn_blocking(move || job(repo, cancel));
        cx.spawn(async move |this, cx| {
            let res = handle
                .await
                .unwrap_or_else(|e| Err(GitError::Io(e.to_string())));
            let _ = this.update(cx, |this, cx| {
                if tracked {
                    this.busy = None;
                }
                match res {
                    Ok(v) => then(this, v, cx),
                    Err(e) => {
                        this.set_error(e, cx);
                        this.refresh(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn then_refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh(cx);
    }

    pub(crate) fn stage(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        self.run(
            None,
            move |r, _| r.stage(&paths),
            |t, _, cx| t.then_refresh(cx),
            cx,
        );
    }

    pub(crate) fn unstage(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        self.run(
            None,
            move |r, _| r.unstage(&paths),
            |t, _, cx| t.then_refresh(cx),
            cx,
        );
    }

    pub(crate) fn stage_all(&mut self, cx: &mut Context<Self>) {
        self.run(
            None,
            |r, _| r.stage_all(),
            |t, _, cx| t.then_refresh(cx),
            cx,
        );
    }

    pub(crate) fn unstage_all(&mut self, cx: &mut Context<Self>) {
        self.run(
            None,
            |r, _| r.unstage_all(),
            |t, _, cx| t.then_refresh(cx),
            cx,
        );
    }

    /// Discards after the user confirmed: `tracked` back to the index, `untracked` deleted.
    pub(crate) fn discard(
        &mut self,
        tracked: Vec<String>,
        untracked: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.run(
            None,
            move |r, _| {
                if !tracked.is_empty() {
                    r.discard(&tracked, Discard::Worktree)?;
                }
                if !untracked.is_empty() {
                    r.discard(&untracked, Discard::Untracked)?;
                }
                Ok(())
            },
            |t, _, cx| t.then_refresh(cx),
            cx,
        );
    }

    pub(crate) fn fetch(&mut self, quiet: bool, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.root.is_none() {
            return;
        }
        self.last_fetch = std::time::Instant::now();
        let label = (!quiet).then(|| t!("git.busy_fetch").into_owned());
        if quiet {
            self.run(None, |r, c| r.fetch(&c), |t, _, cx| t.then_refresh(cx), cx);
            return;
        }
        self.run(label, |r, c| r.fetch(&c), |t, out, cx| t.done(out, cx), cx);
    }

    pub(crate) fn pull(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.root.is_none() {
            return;
        }
        let mode = PullMode::parse(&crate::settings::get(cx).git_pull());
        self.run(
            Some(t!("git.busy_pull").into_owned()),
            move |r, c| r.pull(mode, &c),
            |t, out, cx| t.done(out, cx),
            cx,
        );
    }

    pub(crate) fn push(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.root.is_none() {
            return;
        }
        let (branch, up) = self
            .status
            .as_ref()
            .map(|s| (s.branch.head.clone(), s.branch.upstream.is_some()))
            .unwrap_or((None, true));
        self.run(
            Some(t!("git.busy_push").into_owned()),
            move |r, c| r.push(branch.as_deref(), up, &c),
            |t, out, cx| t.done(out, cx),
            cx,
        );
    }

    /// A finished network command: show what git said, refresh.
    fn done(&mut self, out: String, cx: &mut Context<Self>) {
        let out = out.trim().to_string();
        self.notice = (!out.is_empty()).then_some(Notice {
            text: out,
            error: false,
        });
        self.refresh(cx);
        if self.view == View::History {
            self.load_commits(true, cx);
        }
    }

    pub(crate) fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(b) = &self.busy {
            b.cancel.cancel();
        }
        cx.notify();
    }

    pub(crate) fn dismiss_notice(&mut self, cx: &mut Context<Self>) {
        self.notice = None;
        cx.notify();
    }

    pub(crate) fn init_repo(&mut self, gitignore: bool, cx: &mut Context<Self>) {
        let Some(folder) = self.folder.clone() else {
            return;
        };
        let git = self.git(cx);
        let handle = self
            .rt
            .spawn_blocking(move || Repo::init(&git, std::path::Path::new(&folder), gitignore));
        cx.spawn(async move |this, cx| {
            let res = handle
                .await
                .unwrap_or_else(|e| Err(GitError::Io(e.to_string())));
            let _ = this.update(cx, |this, cx| match res {
                Ok(_) => this.refresh(cx),
                Err(e) => this.set_error(e, cx),
            });
        })
        .detach();
    }

    // ---- committing ------------------------------------------------------------

    /// Ctrl+Enter / the Commit button: scan for credentials, then commit.
    pub(crate) fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_commit(cx) {
            return;
        }
        let message = self.msg.read(cx).value().to_string();
        let amend = self.amend;
        // Like VS Code: with nothing staged, commit what is tracked and changed.
        let all = self.staged_count() == 0 && self.tracked_unstaged_count() > 0;
        let scan = crate::settings::get(cx).git_secret_scan() && !(amend && message.is_empty());
        let paths: Vec<String> = self
            .status
            .as_ref()
            .map(|s| {
                s.entries
                    .iter()
                    .filter(|e| {
                        if all {
                            !e.is_untracked()
                        } else {
                            e.is_staged()
                        }
                    })
                    .map(|e| e.path.clone())
                    .collect()
            })
            .unwrap_or_default();
        let Some(repo) = self.repo(cx) else {
            return;
        };
        self.busy = Some(Busy {
            label: t!("git.busy_commit").into_owned(),
            cancel: Cancel::new(),
        });
        self.notice = None;
        cx.notify();
        let cancel = self.busy.as_ref().map(|b| b.cancel.clone()).unwrap();
        let repo2 = repo.clone();
        let scan_job = self.rt.spawn_blocking(move || -> R<Vec<secrets::Finding>> {
            if !scan {
                return Ok(vec![]);
            }
            let diffs = if all {
                repo2.diff_unstaged_all()?
            } else {
                repo2.diff_staged()?
            };
            Ok(secrets::scan(&paths, &diffs))
        });
        cx.spawn_in(window, async move |this, cx| {
            let found = scan_job
                .await
                .unwrap_or_else(|e| Err(GitError::Io(e.to_string())));
            let _ = this.update_in(cx, |this, window, cx| match found {
                Err(e) => {
                    this.busy = None;
                    this.set_error(e, cx);
                }
                Ok(f) if f.is_empty() => {
                    this.do_commit(repo, message, amend, all, cancel, window, cx)
                }
                Ok(f) => {
                    this.busy = None;
                    cx.notify();
                    this.secret_dialog(f, repo, message, amend, all, window, cx);
                }
            });
        })
        .detach();
    }

    fn do_commit(
        &mut self,
        repo: Repo,
        message: String,
        amend: bool,
        all: bool,
        cancel: Cancel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_none() {
            self.busy = Some(Busy {
                label: t!("git.busy_commit").into_owned(),
                cancel: cancel.clone(),
            });
        }
        let handle = self
            .rt
            .spawn_blocking(move || repo.commit(&message, amend, all, &cancel));
        cx.spawn_in(window, async move |this, cx| {
            let res = handle
                .await
                .unwrap_or_else(|e| Err(GitError::Io(e.to_string())));
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match res {
                    Ok(out) => {
                        this.msg.update(cx, |m, cx| m.set_value("", window, cx));
                        this.amend = false;
                        let first = out.lines().next().unwrap_or("").to_string();
                        this.notice = (!first.is_empty()).then_some(Notice {
                            text: first,
                            error: false,
                        });
                        this.commits.clear();
                        this.commits_end = false;
                    }
                    Err(e) => this.set_error(e, cx),
                }
                this.refresh(cx);
                if this.view == View::History {
                    this.load_commits(true, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Warns about credentials in what is about to be committed.
    #[allow(clippy::too_many_arguments)]
    fn secret_dialog(
        &mut self,
        found: Vec<secrets::Finding>,
        repo: Repo,
        message: String,
        amend: bool,
        all: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let me = cx.entity();
        window.open_pop_dialog(cx, move |dialog, _, cx| {
            use crate::shell::scale::*;
            let t = crate::shell::theme::tokens(cx);
            let mono = cx.theme().mono_font_family.clone();
            let mut list = v_flex().gap(px(S3));
            for f in found.iter().take(12) {
                let place = if f.line == 0 {
                    f.path.clone()
                } else {
                    format!("{}:{}", f.path, f.line)
                };
                let what = match f.kind {
                    secrets::Kind::UrlPassword => t!("git.secret_url"),
                    secrets::Kind::Password => t!("git.secret_password"),
                    secrets::Kind::PrivateKey => t!("git.secret_key"),
                    secrets::Kind::EnvFile => t!("git.secret_env"),
                    secrets::Kind::CloudKey => t!("git.secret_cloud"),
                };
                list = list.child(
                    v_flex()
                        .gap(px(S1))
                        .child(
                            h_flex()
                                .gap(px(S4))
                                .child(
                                    div()
                                        .font_family(mono.clone())
                                        .text_size(px(TX_SM))
                                        .text_color(t.tx1)
                                        .child(place),
                                )
                                .child(
                                    div()
                                        .text_size(px(TX_SM))
                                        .text_color(t.yel_text)
                                        .child(what.into_owned()),
                                ),
                        )
                        .when(!f.excerpt.is_empty(), |d| {
                            d.child(
                                div()
                                    .font_family(mono.clone())
                                    .text_size(px(TX_XS))
                                    .text_color(t.tx3)
                                    .truncate()
                                    .child(f.excerpt.clone()),
                            )
                        }),
                );
            }
            if found.len() > 12 {
                list =
                    list.child(div().text_size(px(TX_SM)).text_color(t.tx3).child(
                        t!("git.secret_more", n = (found.len() - 12).to_string()).into_owned(),
                    ));
            }
            let (me2, repo2, msg2) = (me.clone(), repo.clone(), message.clone());
            let body = v_flex()
                .gap(px(S5))
                .child(
                    div()
                        .text_size(px(TX_BASE))
                        .text_color(t.tx2)
                        .child(t!("git.secret_note").into_owned()),
                )
                .child(list)
                .child(
                    h_flex()
                        .gap(px(S4))
                        .child(div().flex_1())
                        .child(
                            Button::new("git-secret-cancel")
                                .h(px(H_ROW))
                                .label(t!("common.cancel").to_string())
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("git-secret-go")
                                .h(px(H_ROW))
                                .primary()
                                .label(t!("git.commit_anyway").to_string())
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let (repo, msg) = (repo2.clone(), msg2.clone());
                                    me2.update(cx, |this, cx| {
                                        this.do_commit(
                                            repo,
                                            msg,
                                            amend,
                                            all,
                                            Cancel::new(),
                                            window,
                                            cx,
                                        )
                                    });
                                }),
                        ),
                );
            dialog
                .title(t!("git.secret_title").to_string())
                .w(px(W_DLG_M))
                .child(body)
        });
    }

    // ---- branches ----------------------------------------------------------------

    pub(crate) fn switch_branch(&mut self, name: String, remote: bool, cx: &mut Context<Self>) {
        self.run(
            Some(t!("git.busy_switch", name = name.clone()).into_owned()),
            move |r, _| r.switch(&name, remote),
            |t, _, cx| {
                t.commits.clear();
                t.commits_end = false;
                t.refresh(cx);
                if t.view == View::History {
                    t.load_commits(true, cx);
                }
            },
            cx,
        );
    }

    pub(crate) fn create_branch(&mut self, name: String, cx: &mut Context<Self>) {
        self.run(
            Some(t!("git.busy_switch", name = name.clone()).into_owned()),
            move |r, _| r.create_branch(&name),
            |t, _, cx| t.then_refresh(cx),
            cx,
        );
    }

    // ---- history -----------------------------------------------------------------

    pub(crate) fn set_view(&mut self, v: View, cx: &mut Context<Self>) {
        self.view = v;
        if v == View::History && self.commits.is_empty() && !self.commits_end {
            self.load_commits(true, cx);
        }
        cx.notify();
    }

    /// History of one file (`None` = of the repository).
    pub(crate) fn set_file_history(&mut self, rel: Option<String>, cx: &mut Context<Self>) {
        self.file_history = rel;
        self.view = View::History;
        self.load_commits(true, cx);
    }

    /// Loads the first page (`restart`) or the next one.
    pub(crate) fn load_commits(&mut self, restart: bool, cx: &mut Context<Self>) {
        if self.commits_loading && !restart {
            return;
        }
        if restart {
            self.commits.clear();
            self.commits_end = false;
            self.sel_commit = None;
            self.sel_files.clear();
        } else if self.commits_end {
            return;
        }
        let skip = self.commits.len();
        let file = self.file_history.clone();
        self.commits_loading = true;
        self.run(
            None,
            move |r, _| r.log(skip, PAGE, file.as_deref()),
            move |t, page, cx| {
                t.commits_loading = false;
                t.commits_end = page.len() < PAGE;
                t.commits.extend(page);
                cx.notify();
            },
            cx,
        );
    }

    pub(crate) fn select_commit(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(c) = self.commits.get(ix) else {
            return;
        };
        self.sel_commit = Some(ix);
        self.sel_files.clear();
        let hash = c.hash.clone();
        let want = ix;
        self.run(
            None,
            move |r, _| r.commit_files(&hash),
            move |t, files, cx| {
                if t.sel_commit == Some(want) {
                    t.sel_files = files;
                    cx.notify();
                }
            },
            cx,
        );
        cx.notify();
    }

    /// The loader the list calls when it nears its end.
    pub(crate) fn more_commits(&mut self, cx: &mut Context<Self>) {
        self.load_commits(false, cx);
    }

    pub(crate) fn root_string(&self) -> Option<String> {
        self.root.as_ref().map(|r| r.to_string_lossy().into_owned())
    }
}
