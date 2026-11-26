//! The Git panel in the shell: Space folder → work tree, tab letters, the status-bar item,
//! the diff tab, files opened from the panel and the palette commands.
use super::*;
use crate::shell::scale::*;
use crate::shell::shortcuts::Shortcut;
use locus_core::spaces::{RightPanel, TabPage};

impl Locus {
    /// The active Space's folder is the work tree: tell the panel (after a Space switch, a
    /// bind / unbind and at start-up).
    pub(crate) fn sync_git(&mut self, cx: &mut Context<Self>) {
        let folder = self.space().folder.clone();
        self.git.update(cx, |g, cx| g.set_folder(folder, cx));
    }

    /// Something saved a file: the panel reads the status again.
    pub(crate) fn git_file_saved(&mut self, cx: &mut Context<Self>) {
        self.git.update(cx, |g, cx| g.refresh(cx));
    }

    pub(crate) fn on_git_event(
        &mut self,
        ev: &GitEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            GitEvent::Changed => cx.notify(),
            GitEvent::OpenFile(p) => self.open_path(p.clone(), window, cx),
            GitEvent::OpenDiff(spec) => self.open_git_diff(spec.clone(), window, cx),
            GitEvent::BindFolder => self.bind_folder(window, cx),
            GitEvent::FileHistory => {
                let path = self.active_tab(cx).read(cx).doc.path.clone();
                let rel = path.and_then(|p| self.git.read(cx).rel_of(std::path::Path::new(&p)));
                match rel {
                    Some(rel) => self
                        .git
                        .update(cx, |g, cx| g.set_file_history(Some(rel), cx)),
                    None => crate::shell::toast::show(t!("git.file_history_none").to_string(), cx),
                }
            }
        }
    }

    /// A file the panel names: in a tab (the one already open, else read from disk).
    pub(crate) fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let p = path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move { std::fs::read(&p) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match read {
                Ok(bytes) => {
                    let b = bytes
                        .strip_prefix(&[0xEF, 0xBB, 0xBF][..])
                        .unwrap_or(&bytes);
                    let text = String::from_utf8_lossy(b).into_owned();
                    this.open_text_tab(path.to_string_lossy().into_owned(), text, window, cx);
                }
                Err(e) => crate::shell::toast::show(
                    t!(
                        "file.read_error",
                        path = path.display().to_string(),
                        msg = e.to_string()
                    )
                    .to_string(),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Shows the diff tab of `spec`, or opens it.
    pub(crate) fn open_git_diff(
        &mut self,
        spec: DiffSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let json = spec.to_json();
        let existing = self.space().tabs.iter().find_map(|t| {
            let t = t.read(cx);
            (t.pageslot.kind == Some(TabPage::Diff)
                && t.editor.read(cx).state.read(cx).value().as_ref() == json.as_str())
            .then(|| t.id.clone())
        });
        if let Some(id) = existing {
            self.show_tab(&id, window, cx);
            return;
        }
        let name = if spec.path.is_empty() {
            t!("git.diff_whole").into_owned()
        } else {
            locus_git::root::split_name(&spec.path).1.to_string()
        };
        let title = match &spec.what {
            DiffWhat::Staged => t!("git.tab_staged", name = name).into_owned(),
            DiffWhat::Commit { short, .. } => {
                t!("git.tab_commit", name = name, short = short.clone()).into_owned()
            }
            _ => t!("git.tab_diff", name = name).into_owned(),
        };
        let mut data = self.blank_tab(None, cx);
        data.page = Some(TabPage::Diff);
        data.title = Some(title);
        data.sql = json;
        let tab = self.make_tab(&data, window, cx);
        self.space_mut().tabs.push(tab);
        self.show_tab(&data.id, window, cx);
        self.schedule_save(cx);
    }

    /// Creates the view of a restored or new diff tab (called from `attach_page`).
    pub(crate) fn attach_diff(
        &mut self,
        tab: &Entity<QueryTab>,
        data: &TabData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let git = self.git.clone();
        let rt = self.pool.rt().clone();
        let json = data.sql.clone();
        let view = cx.new(|cx| diffpage::DiffPage::new(git, rt, &json, window, cx));
        let focus = view.read(cx).focus_handle(cx);
        tab.update(cx, |t, _| {
            t.pageslot.view = Some(view.into());
            t.pageslot.focus = Some(focus);
        });
    }

    /// The letter a tab of a changed file wears, between its label and the close button.
    pub(crate) fn git_tab_marker(&self, tab: &Entity<QueryTab>, cx: &App) -> Option<AnyElement> {
        let t = crate::shell::theme::tokens(cx);
        let tr = tab.read(cx);
        let path = tr.doc.path.as_ref()?;
        let letter = self.git.read(cx).letter_for(std::path::Path::new(path))?;
        Some(
            div()
                .flex_none()
                .w(px(14.))
                .text_center()
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(px(TX_XS))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(panel::letter_color(letter, t))
                .child(letter.to_string())
                .into_any_element(),
        )
    }

    /// `branch · N changes`; a click opens the panel.
    pub(crate) fn git_status_item(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = crate::shell::theme::tokens(cx);
        let (branch, n) = self.git.read(cx).summary()?;
        let text = if n == 0 {
            t!("git.status_clean", branch = branch).into_owned()
        } else {
            t!("git.status_changes", branch = branch, n = n.to_string()).into_owned()
        };
        Some(
            h_flex()
                .id("status-git")
                .gap(px(S3))
                .items_center()
                .cursor_pointer()
                .text_color(t.tx2)
                .hover(|h| h.text_color(t.tx))
                .tooltip(tip(t!("git.status_tip").into_owned()))
                .child(
                    crate::shell::icons::icon("git-branch")
                        .size(px(12.))
                        .text_color(t.tx3),
                )
                .child(text)
                .on_click(
                    cx.listener(|this, _, _, cx| this.set_right_panel(Some(RightPanel::Git), cx)),
                )
                .into_any_element(),
        )
    }

    /// The palette commands of the Git panel.
    pub(crate) fn git_command(&mut self, s: Shortcut, window: &mut Window, cx: &mut Context<Self>) {
        if s == Shortcut::ToggleGit {
            self.toggle_right_panel(RightPanel::Git, cx);
            return;
        }
        // The panel shows progress and git's own words, so open it for every command.
        if self.right_panel != Some(RightPanel::Git) {
            self.set_right_panel(Some(RightPanel::Git), cx);
        }
        self.git.update(cx, |g, cx| match s {
            Shortcut::GitCommit => {
                g.set_view(View::Changes, cx);
                g.focus_msg = true;
                cx.notify();
            }
            Shortcut::GitPull => g.pull(cx),
            Shortcut::GitPush => g.push(cx),
            Shortcut::GitFetch => g.fetch(false, cx),
            Shortcut::GitSwitchBranch => g.open_branches(window, cx),
            Shortcut::GitRefresh => g.refresh(cx),
            _ => {}
        });
    }
}
