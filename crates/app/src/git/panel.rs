//! The Git panel's views: the four states (no folder, git missing, no repository, ready),
//! the changes list, the history list and the dialogs of the panel.
use super::*;
use crate::shell::scale::*;
use crate::shell::seg::{SegItem, seg};
use gpui_kit::component::Disableable as _;
use gpui_kit::component::Size;
use gpui_kit::component::checkbox::Checkbox;
use locus_git::status::Entry;
use std::ops::Range;
use std::rc::Rc;

/// Row height of the lists: the same as the schema tree.
const ROW_H: f32 = 24.0;
/// A commit: subject on one line, hash · author · age on the next.
const COMMIT_H: f32 = 40.0;
/// Row icon size and the button box whose icon (0.75 x) is that size.
const NODE_ICON: f32 = 14.0;
const ACTION_BTN: f32 = NODE_ICON / 0.75;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Group {
    Conflicts,
    Staged,
    Changes,
}

#[derive(Clone)]
pub(crate) enum Row {
    Group {
        group: Group,
        n: usize,
    },
    File {
        path: String,
        letter: char,
        group: Group,
        untracked: bool,
    },
    Empty,
}

impl GitPanel {
    /// Rebuilds the flat list the changes view draws.
    pub(crate) fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        if let Some(st) = &self.status {
            let file = |e: &Entry, group: Group| Row::File {
                path: e.path.clone(),
                letter: match group {
                    Group::Staged => e.index.letter(),
                    Group::Changes if e.is_untracked() => 'U',
                    Group::Changes => e.worktree.letter(),
                    Group::Conflicts => 'C',
                },
                group,
                untracked: e.is_untracked(),
            };
            let conflicts: Vec<_> = st.conflicts().collect();
            if !conflicts.is_empty() {
                rows.push(Row::Group {
                    group: Group::Conflicts,
                    n: conflicts.len(),
                });
                rows.extend(conflicts.iter().map(|e| file(e, Group::Conflicts)));
            }
            let staged: Vec<_> = st.staged().collect();
            if !staged.is_empty() {
                rows.push(Row::Group {
                    group: Group::Staged,
                    n: staged.len(),
                });
                rows.extend(staged.iter().map(|e| file(e, Group::Staged)));
            }
            let un: Vec<_> = st.unstaged().collect();
            if !un.is_empty() {
                rows.push(Row::Group {
                    group: Group::Changes,
                    n: un.len(),
                });
                rows.extend(un.iter().map(|e| file(e, Group::Changes)));
            }
            if rows.is_empty() {
                rows.push(Row::Empty);
            }
        }
        self.rows = rows;
    }

    // ---- header ------------------------------------------------------------------

    /// Fetch, Pull, Push and Refresh for the panel header (same buttons as the schema panel's).
    pub(crate) fn header_actions(&self, me: &Entity<Self>, cx: &App) -> Vec<AnyElement> {
        if self.phase() != Phase::Ready {
            return Vec::new();
        }
        let t = crate::shell::theme::tokens(cx);
        let idle = self.busy.is_none();
        let btn = |id: &'static str,
                   icon: &'static str,
                   tip_key: &'static str,
                   on: Rc<dyn Fn(&mut Self, &mut Context<Self>)>| {
            let me = me.clone();
            let mut b = ib(id, crate::shell::icons::icon(icon), H_COMPACT, false, cx)
                .tooltip(tip(t!(tip_key).into_owned()));
            if !idle && id != "git-refresh" {
                b = b.opacity(0.4).cursor_default();
            }
            b.on_click(move |_, _, cx| {
                let on = on.clone();
                me.update(cx, |g, cx| {
                    if g.busy.is_none() || id == "git-refresh" {
                        on(g, cx)
                    }
                })
            })
            .into_any_element()
        };
        let _ = t;
        vec![
            btn(
                "git-fetch",
                "cloud-download",
                "git.fetch",
                Rc::new(|g, cx| g.fetch(false, cx)),
            ),
            btn("git-pull", "pull", "git.pull", Rc::new(|g, cx| g.pull(cx))),
            btn("git-push", "push", "git.push", Rc::new(|g, cx| g.push(cx))),
            btn(
                "git-refresh",
                "refresh",
                "git.refresh",
                Rc::new(|g, cx| g.refresh(cx)),
            ),
        ]
    }

    // ---- states ------------------------------------------------------------------

    fn centred(&self, cx: &App, icon: &'static str, title: String, body: String) -> Div {
        let t = crate::shell::theme::tokens(cx);
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(S4))
            .px(px(S6))
            .child(
                crate::shell::icons::icon(icon)
                    .size(px(24.))
                    .text_color(t.tx4),
            )
            .child(
                div()
                    .text_size(px(TX_BASE))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.tx1)
                    .text_center()
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(TX_SM))
                    .text_color(t.tx3)
                    .text_center()
                    .whitespace_normal()
                    .child(body),
            )
    }

    fn render_no_folder(&self, cx: &mut Context<Self>) -> AnyElement {
        self.centred(
            cx,
            "folder",
            t!("git.no_folder").into_owned(),
            t!("git.no_folder_note").into_owned(),
        )
        .child(
            Button::new("git-bind")
                .primary()
                .h(px(H_CONTROL))
                .label(t!("git.bind_folder").to_string())
                .on_click(cx.listener(|_, _, _, cx| cx.emit(GitEvent::BindFolder))),
        )
        .into_any_element()
    }

    fn render_missing(&self, cx: &mut Context<Self>) -> AnyElement {
        let program = self.missing_program();
        self.centred(
            cx,
            "warn",
            t!("git.missing").into_owned(),
            t!("git.missing_note", program = program).into_owned(),
        )
        .child(
            h_flex()
                .gap(px(S4))
                .child(
                    Button::new("git-install")
                        .primary()
                        .h(px(H_CONTROL))
                        .label(t!("git.install").to_string())
                        .on_click(|_, _, cx| cx.open_url(locus_git::exec::INSTALL_URL)),
                )
                .child(
                    Button::new("git-retry")
                        .h(px(H_CONTROL))
                        .label(t!("git.retry").to_string())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.missing = None;
                            this.refresh(cx);
                        })),
                ),
        )
        .into_any_element()
    }

    fn render_no_repo(&self, cx: &mut Context<Self>) -> AnyElement {
        let gi = self.init_gitignore;
        self.centred(
            cx,
            "git-branch",
            t!("git.no_repo").into_owned(),
            t!("git.no_repo_note").into_owned(),
        )
        .child(
            Checkbox::new("git-init-ignore")
                .label(t!("git.init_ignore").to_string())
                .checked(gi)
                .on_click(cx.listener(|this, c: &bool, _, cx| {
                    this.init_gitignore = *c;
                    cx.notify();
                })),
        )
        .child(
            Button::new("git-init")
                .primary()
                .h(px(H_CONTROL))
                .label(t!("git.init").to_string())
                .on_click(cx.listener(|this, _, _, cx| {
                    let gi = this.init_gitignore;
                    this.init_repo(gi, cx)
                })),
        )
        .into_any_element()
    }

    // ---- the ready view --------------------------------------------------------------

    fn render_branch_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let st = self.status.as_ref();
        let name = self
            .branch()
            .map(str::to_string)
            .unwrap_or_else(|| t!("git.detached").into_owned());
        let (ahead, behind, upstream) = st
            .map(|s| (s.branch.ahead, s.branch.behind, s.branch.upstream.is_some()))
            .unwrap_or((0, 0, false));
        let mono = cx.theme().mono_font_family.clone();
        h_flex()
            .flex_none()
            .h(px(H_COMPACT))
            .px(px(S5))
            .gap(px(S3))
            .items_center()
            .child(
                h_flex()
                    .id("git-branch")
                    .flex_1()
                    .min_w_0()
                    .h(px(H_COMPACT))
                    .px(px(S3))
                    .gap(px(S3))
                    .items_center()
                    .rounded(px(R_SM))
                    .cursor_pointer()
                    .text_size(px(TX_BASE))
                    .text_color(t.tx1)
                    .hover(|h| h.bg(t.hv))
                    .tooltip(tip(t!("git.branches_tip").into_owned()))
                    .child(
                        crate::shell::icons::icon("git-branch")
                            .size(px(NODE_ICON))
                            .flex_none()
                            .text_color(t.tx3),
                    )
                    .child(div().min_w_0().truncate().child(name))
                    .child(
                        crate::shell::icons::icon("chevron-down")
                            .size(px(12.))
                            .flex_none()
                            .text_color(t.tx3),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.open_branches(window, cx))),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(S3))
                    .font_family(mono)
                    .text_size(px(TX_XS))
                    .text_color(t.tx3)
                    .when(upstream, |d| {
                        d.child(format!("↑{ahead}")).child(format!("↓{behind}"))
                    })
                    .when(!upstream && st.is_some(), |d| {
                        d.child(
                            div()
                                .font_family(cx.theme().font_family.clone())
                                .child(t!("git.no_upstream").into_owned()),
                        )
                    }),
            )
            .into_any_element()
    }

    /// Progress of a running command, or what git last said.
    fn render_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = crate::shell::theme::tokens(cx);
        if let Some(b) = &self.busy {
            return Some(
                h_flex()
                    .flex_none()
                    .mx(px(S5))
                    .mb(px(S3))
                    .h(px(H_COMPACT))
                    .px(px(S4))
                    .gap(px(S4))
                    .items_center()
                    .rounded(px(R_SM))
                    .bg(t.hv)
                    .text_size(px(TX_SM))
                    .text_color(t.tx2)
                    .child(crate::shell::style::spinner(t.tx3))
                    .child(div().flex_1().min_w_0().truncate().child(b.label.clone()))
                    .child(
                        div()
                            .id("git-cancel")
                            .flex_none()
                            .px(px(S3))
                            .h(px(H_BADGE))
                            .flex()
                            .items_center()
                            .rounded(px(R_XS))
                            .cursor_pointer()
                            .text_color(t.tx1)
                            .hover(|h| h.bg(t.on))
                            .child(t!("common.cancel").into_owned())
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    )
                    .into_any_element(),
            );
        }
        let n = self.notice.as_ref()?;
        let col = if n.error { t.red_text } else { t.tx2 };
        Some(
            h_flex()
                .flex_none()
                .mx(px(S5))
                .mb(px(S3))
                .p(px(S4))
                .gap(px(S3))
                .items_start()
                .rounded(px(R_SM))
                .bg(t.hv)
                .border_1()
                .border_color(if n.error { t.red.opacity(0.4) } else { t.ln })
                .child(
                    div()
                        .id("git-notice-text")
                        .flex_1()
                        .min_w_0()
                        .max_h(px(120.))
                        .overflow_y_scroll()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(px(TX_MONO))
                        .text_color(col)
                        .whitespace_normal()
                        .child(n.text.clone()),
                )
                .child(
                    ib(
                        "git-notice-x",
                        crate::shell::icons::icon("x"),
                        H_BADGE,
                        false,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss_notice(cx))),
                )
                .into_any_element(),
        )
    }

    fn render_commit_box(&self, cx: &mut Context<Self>) -> AnyElement {
        let can = self.can_commit(cx);
        let staged = self.staged_count();
        let label = if self.amend {
            t!("git.amend_btn")
        } else if staged == 0 && self.tracked_unstaged_count() > 0 {
            t!("git.commit_all")
        } else {
            t!("git.commit")
        }
        .into_owned();
        v_flex()
            .flex_none()
            .px(px(S5))
            .pb(px(S4))
            .gap(px(S4))
            .child(
                div()
                    .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                        let k = &ev.keystroke;
                        if k.key == "enter" && k.modifiers.control && !k.modifiers.shift {
                            cx.stop_propagation();
                            this.commit(window, cx);
                        }
                    }))
                    .child(gpui_kit::component::input::Textarea::new(&self.msg).h(px(72.))),
            )
            .child(
                h_flex()
                    .gap(px(S4))
                    .items_center()
                    .child(
                        Checkbox::new("git-amend")
                            .label(t!("git.amend").to_string())
                            .checked(self.amend)
                            .on_click(cx.listener(|this, c: &bool, _, cx| {
                                this.amend = *c;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("git-commit")
                            .primary()
                            .h(px(H_CONTROL))
                            .label(label)
                            .disabled(!can)
                            .tooltip(t!("git.commit_tip").to_string())
                            .on_click(cx.listener(|this, _, window, cx| this.commit(window, cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let n = self.status.as_ref().map_or(0, |s| s.change_count());
        let view = self.view;
        let me = cx.entity();
        let (m1, m2) = (me.clone(), me.clone());
        let items = vec![
            SegItem::new(
                "git-seg-changes",
                if n > 0 {
                    format!("{} {n}", t!("git.changes"))
                } else {
                    t!("git.changes").into_owned()
                },
                view == View::Changes,
                Rc::new(move |_, _, cx| m1.update(cx, |g, cx| g.set_view(View::Changes, cx))),
            ),
            SegItem::new(
                "git-seg-history",
                t!("git.history").into_owned(),
                view == View::History,
                Rc::new(move |_, _, cx| m2.update(cx, |g, cx| g.set_view(View::History, cx))),
            ),
        ];
        h_flex()
            .flex_none()
            .px(px(S5))
            .pb(px(S4))
            .gap(px(S4))
            .items_center()
            .child(div().flex_1().child(seg(items, cx)))
            .when(view == View::History, |d| {
                d.child(
                    ib(
                        "git-file-history",
                        crate::shell::icons::icon("script"),
                        H_COMPACT,
                        self.file_history.is_some(),
                        cx,
                    )
                    .tooltip(tip(t!("git.file_history_tip").into_owned()))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(GitEvent::FileHistory))),
                )
            })
            .into_any_element()
    }

    // ---- changes ----------------------------------------------------------------------

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let Some(row) = self.rows.get(ix).cloned() else {
            return div().h(px(ROW_H)).into_any_element();
        };
        let base = h_flex()
            .id(("git-row", ix))
            .w_full()
            .h(px(ROW_H))
            .pl(px(S2))
            .pr(px(S5))
            .gap(px(S2))
            .items_center()
            .text_size(px(TX_BASE))
            .rounded(px(R_MD));
        match row {
            Row::Empty => base
                .text_color(t.tx3)
                .pl(px(S4))
                .child(t!("git.clean").into_owned())
                .into_any_element(),
            Row::Group { group, n } => self.render_group_row(ix, base, group, n, cx),
            Row::File {
                path,
                letter,
                group,
                untracked,
            } => self.render_file_row(ix, base, path, letter, group, untracked, cx),
        }
    }

    fn render_group_row(
        &self,
        ix: usize,
        base: Stateful<Div>,
        group: Group,
        n: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let title = match group {
            Group::Conflicts => t!("git.g_conflicts"),
            Group::Staged => t!("git.g_staged"),
            Group::Changes => t!("git.g_changes"),
        };
        let gname = SharedString::from(format!("git-grow-{ix}"));
        let all_btn = |id: &'static str, icon: &'static str, tipk: &'static str| {
            Button::new((id, ix))
                .ghost()
                .with_size(Size::Size(px(ACTION_BTN)))
                .icon(crate::shell::icons::icon(icon))
                .tooltip(t!(tipk).to_string())
        };
        let actions = h_flex()
            .flex_none()
            .invisible()
            .group_hover(gname.clone(), |s| s.visible())
            .when(group == Group::Staged, |d| {
                d.child(
                    all_btn("git-unstage-all", "minus", "git.unstage_all")
                        .on_click(cx.listener(|this, _, _, cx| this.unstage_all(cx))),
                )
            })
            .when(group == Group::Changes, |d| {
                d.child(
                    all_btn("git-discard-all", "undo", "git.discard_all").on_click(cx.listener(
                        |this, _, window, cx| {
                            let (tr, un) = this.changes_paths();
                            this.confirm_discard(tr, un, window, cx)
                        },
                    )),
                )
                .child(
                    all_btn("git-stage-all", "plus", "git.stage_all")
                        .on_click(cx.listener(|this, _, _, cx| this.stage_all(cx))),
                )
            });
        base.group(gname)
            .pl(px(S4))
            .text_size(px(TX_XS))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if group == Group::Conflicts {
                t.red_text
            } else {
                t.tx3
            })
            .child(
                div()
                    .flex_1()
                    .child(format!("{} ({n})", title.to_uppercase())),
            )
            .child(actions)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_file_row(
        &self,
        ix: usize,
        base: Stateful<Div>,
        path: String,
        letter: char,
        group: Group,
        untracked: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let (dir, name) = locus_git::root::split_name(&path);
        let (dir, name) = (dir.to_string(), name.to_string());
        let gname = SharedString::from(format!("git-frow-{ix}"));
        let what = match group {
            Group::Staged => DiffWhat::Staged,
            _ if untracked => DiffWhat::Untracked,
            _ => DiffWhat::Unstaged,
        };
        let root = self.root_string().unwrap_or_default();
        let (p_click, p_open, p_stage, p_discard) =
            (path.clone(), path.clone(), path.clone(), path.clone());
        let btn = |id: &'static str, icon: &'static str, tipk: &'static str| {
            Button::new((id, ix))
                .ghost()
                .with_size(Size::Size(px(ACTION_BTN)))
                .icon(crate::shell::icons::icon(icon))
                .tooltip(t!(tipk).to_string())
        };
        let actions = h_flex()
            .flex_none()
            .invisible()
            .group_hover(gname.clone(), |s| s.visible())
            .child(
                btn("git-open", "pencil", "git.open_file").on_click(cx.listener(
                    move |this, _, _, cx| {
                        cx.stop_propagation();
                        if let Some(root) = &this.root {
                            cx.emit(GitEvent::OpenFile(root::abs_path(root, &p_open)));
                        }
                    },
                )),
            )
            .when(group == Group::Changes, |d| {
                let (tr, un) = if untracked {
                    (vec![], vec![p_discard.clone()])
                } else {
                    (vec![p_discard.clone()], vec![])
                };
                d.child(
                    btn("git-discard", "undo", "git.discard").on_click(cx.listener(
                        move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.confirm_discard(tr.clone(), un.clone(), window, cx)
                        },
                    )),
                )
            })
            .when(group != Group::Conflicts, |d| {
                let staged = group == Group::Staged;
                d.child(
                    btn(
                        "git-stage",
                        if staged { "minus" } else { "plus" },
                        if staged { "git.unstage" } else { "git.stage" },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if staged {
                            this.unstage(vec![p_stage.clone()], cx)
                        } else {
                            this.stage(vec![p_stage.clone()], cx)
                        }
                    })),
                )
            });
        let col = letter_color(letter, t);
        base.group(gname)
            .cursor_pointer()
            .hover(|s| s.bg(t.hv))
            .child(
                div()
                    .flex_none()
                    .w(px(16.))
                    .text_center()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(px(TX_XS))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(col)
                    .child(letter.to_string()),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(S3))
                    .items_baseline()
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(160.))
                            .truncate()
                            .text_color(if group == Group::Conflicts {
                                t.red_text
                            } else {
                                t.tx
                            })
                            .child(name),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(TX_XS))
                            .text_color(t.tx3)
                            .child(dir),
                    ),
            )
            .child(actions)
            .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                if e.click_count() >= 2 {
                    if let Some(root) = &this.root {
                        cx.emit(GitEvent::OpenFile(root::abs_path(root, &p_click)));
                    }
                } else {
                    cx.emit(GitEvent::OpenDiff(DiffSpec {
                        root: root.clone(),
                        path: p_click.clone(),
                        what: what.clone(),
                    }));
                }
            }))
            .into_any_element()
    }

    /// (tracked, untracked) paths of the Changes group.
    fn changes_paths(&self) -> (Vec<String>, Vec<String>) {
        let mut tr = Vec::new();
        let mut un = Vec::new();
        if let Some(st) = &self.status {
            for e in st.unstaged() {
                if e.is_untracked() {
                    un.push(e.path.clone());
                } else {
                    tr.push(e.path.clone());
                }
            }
        }
        (tr, un)
    }

    /// Discarding cannot be undone: ask first.
    pub(crate) fn confirm_discard(
        &mut self,
        tracked: Vec<String>,
        untracked: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let n = tracked.len() + untracked.len();
        if n == 0 {
            return;
        }
        let me = cx.entity();
        let what = if n == 1 {
            tracked
                .first()
                .or(untracked.first())
                .cloned()
                .unwrap_or_default()
        } else {
            t!("git.n_files", n = n.to_string()).into_owned()
        };
        let del = untracked.len();
        window.open_pop_dialog(cx, move |dialog, _, cx| {
            let t = crate::shell::theme::tokens(cx);
            let (me2, tr, un) = (me.clone(), tracked.clone(), untracked.clone());
            let body = v_flex()
                .gap(px(S5))
                .child(
                    div()
                        .text_size(px(TX_BASE))
                        .text_color(t.tx1)
                        .whitespace_normal()
                        .child(t!("git.discard_body", what = what.clone()).into_owned()),
                )
                .child(
                    div()
                        .text_size(px(TX_SM))
                        .text_color(t.tx3)
                        .whitespace_normal()
                        .child(if del > 0 {
                            t!("git.discard_note_untracked", n = del.to_string()).into_owned()
                        } else {
                            t!("git.discard_note").into_owned()
                        }),
                )
                .child(
                    h_flex()
                        .gap(px(S4))
                        .child(div().flex_1())
                        .child(
                            Button::new("git-discard-cancel")
                                .h(px(H_ROW))
                                .label(t!("common.cancel").to_string())
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("git-discard-go")
                                .h(px(H_ROW))
                                .danger()
                                .label(t!("git.discard").to_string())
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let (tr, un) = (tr.clone(), un.clone());
                                    me2.update(cx, |g, cx| g.discard(tr, un, cx));
                                }),
                        ),
                );
            dialog
                .title(t!("git.discard_title").to_string())
                .w(px(W_DLG_S))
                .child(body)
        });
    }

    // ---- history ------------------------------------------------------------------------

    fn render_commit(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let Some(c) = self.commits.get(ix) else {
            // The row after the last commit: "Load more" while there are more.
            return h_flex()
                .id("git-more")
                .h(px(COMMIT_H))
                .px(px(S5))
                .items_center()
                .text_size(px(TX_SM))
                .text_color(t.tx3)
                .child(if self.commits_end {
                    String::new()
                } else {
                    t!("git.loading").into_owned()
                })
                .into_any_element();
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let age = age_text(locus_git::log::age(now, c.time));
        let on = self.sel_commit == Some(ix);
        let (subject, short, author) = (c.subject.clone(), c.short.clone(), c.author.clone());
        let (hash, root) = (c.hash.clone(), self.root_string().unwrap_or_default());
        let mono = cx.theme().mono_font_family.clone();
        v_flex()
            .id(("git-commit", ix))
            .w_full()
            .h(px(COMMIT_H))
            .px(px(S4))
            .justify_center()
            .gap(px(S1))
            .rounded(px(R_MD))
            .cursor_pointer()
            .when(on, |d| d.bg(t.on))
            .when(!on, |d| d.hover(|h| h.bg(t.hv)))
            .child(
                div()
                    .truncate()
                    .text_size(px(TX_BASE))
                    .text_color(t.tx)
                    .child(if subject.is_empty() {
                        t!("git.no_subject").into_owned()
                    } else {
                        subject
                    }),
            )
            .child(
                h_flex()
                    .gap(px(S3))
                    .min_w_0()
                    .text_size(px(TX_XS))
                    .text_color(t.tx3)
                    .child(
                        div()
                            .flex_none()
                            .font_family(mono)
                            .text_color(t.acc_text)
                            .child(short.clone()),
                    )
                    .child(div().min_w_0().truncate().child(author))
                    .child(div().flex_none().child(age)),
            )
            .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                this.select_commit(ix, cx);
                if e.click_count() >= 2 {
                    cx.emit(GitEvent::OpenDiff(DiffSpec {
                        root: root.clone(),
                        path: String::new(),
                        what: DiffWhat::Commit {
                            hash: hash.clone(),
                            short: short.clone(),
                        },
                    }));
                }
            }))
            .into_any_element()
    }

    /// The files of the selected commit, under the list.
    fn render_commit_files(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = crate::shell::theme::tokens(cx);
        let c = self.commits.get(self.sel_commit?)?;
        let root = self.root_string().unwrap_or_default();
        let (hash, short) = (c.hash.clone(), c.short.clone());
        let whole = DiffSpec {
            root: root.clone(),
            path: String::new(),
            what: DiffWhat::Commit {
                hash: hash.clone(),
                short: short.clone(),
            },
        };
        let mut list = v_flex().gap(px(S1)).px(px(S4)).pb(px(S4));
        for (i, f) in self.sel_files.iter().enumerate() {
            let (dir, name) = locus_git::root::split_name(&f.path);
            let spec = DiffSpec {
                root: root.clone(),
                path: f.path.clone(),
                what: DiffWhat::Commit {
                    hash: hash.clone(),
                    short: short.clone(),
                },
            };
            list = list.child(
                h_flex()
                    .id(("git-cfile", i))
                    .w_full()
                    .h(px(ROW_H))
                    .px(px(S2))
                    .gap(px(S2))
                    .items_center()
                    .rounded(px(R_MD))
                    .cursor_pointer()
                    .text_size(px(TX_BASE))
                    .hover(|h| h.bg(t.hv))
                    .child(
                        div()
                            .flex_none()
                            .w(px(16.))
                            .text_center()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(TX_XS))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(letter_color(f.letter, t))
                            .child(f.letter.to_string()),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(S3))
                            .items_baseline()
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(px(160.))
                                    .truncate()
                                    .child(name.to_string()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(TX_XS))
                                    .text_color(t.tx3)
                                    .child(dir.to_string()),
                            ),
                    )
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(GitEvent::OpenDiff(spec.clone()))),
                    ),
            );
        }
        Some(
            v_flex()
                .flex_none()
                .max_h(relative(0.45))
                .border_t_1()
                .border_color(t.ln)
                .child(
                    h_flex()
                        .flex_none()
                        .h(px(H_COMPACT))
                        .pl(px(S5))
                        .pr(px(S4))
                        .items_center()
                        .text_size(px(TX_XS))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.tx3)
                        .child(
                            div().flex_1().child(
                                t!("git.commit_files", n = self.sel_files.len().to_string())
                                    .to_uppercase(),
                            ),
                        )
                        .child(
                            Button::new("git-open-commit")
                                .ghost()
                                .xsmall()
                                .label(t!("git.open_diff").to_string())
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(GitEvent::OpenDiff(whole.clone()))
                                })),
                        ),
                )
                .child(
                    div()
                        .id("git-cfiles-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(list),
                )
                .into_any_element(),
        )
    }

    fn render_history(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let n = self.commits.len() + usize::from(!self.commits_end || self.commits.is_empty());
        let banner = self.file_history.clone().map(|p| {
            h_flex()
                .flex_none()
                .mx(px(S5))
                .mb(px(S3))
                .h(px(H_COMPACT))
                .px(px(S4))
                .gap(px(S3))
                .items_center()
                .rounded(px(R_SM))
                .bg(t.hv)
                .text_size(px(TX_SM))
                .text_color(t.tx2)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(t!("git.history_of", file = p).into_owned()),
                )
                .child(
                    ib(
                        "git-file-history-x",
                        crate::shell::icons::icon("x"),
                        H_BADGE,
                        false,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.file_history = None;
                        this.load_commits(true, cx);
                    })),
                )
        });
        let list: AnyElement = if self.commits.is_empty() && self.commits_end {
            div()
                .px(px(S6))
                .py(px(S5))
                .text_size(px(TX_BASE))
                .text_color(t.tx3)
                .child(t!("git.no_commits").into_owned())
                .into_any_element()
        } else {
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .px(px(S4))
                .child(
                    uniform_list(
                        "git-history",
                        n,
                        cx.processor(|this, range: Range<usize>, _, cx| {
                            if range.end + 8 >= this.commits.len() && !this.commits_end {
                                this.more_commits(cx);
                            }
                            range.map(|i| this.render_commit(i, cx)).collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.hist_scroll)
                    .size_full(),
                )
                .vertical_scrollbar(&self.hist_scroll)
                .into_any_element()
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .children(banner)
            .child(list)
            .children(self.render_commit_files(cx))
            .into_any_element()
    }

    fn render_changes(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let n = self.rows.len();
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .px(px(S4))
            .child(
                uniform_list(
                    "git-changes",
                    n,
                    cx.processor(|this, range: Range<usize>, _, cx| {
                        range.map(|i| this.render_row(i, cx)).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
                .size_full(),
            )
            .vertical_scrollbar(&self.scroll)
            .into_any_element()
    }

    // ---- branches -------------------------------------------------------------------------

    /// The branch switcher: a list with a filter box, and "create" for a name that is new.
    pub(crate) fn open_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.repo(cx) else {
            return;
        };
        let handle = self.rt.spawn_blocking(move || repo.branches());
        cx.spawn_in(window, async move |this, cx| {
            let res = handle
                .await
                .unwrap_or_else(|e| Err(GitError::Io(e.to_string())));
            let _ = this.update_in(cx, |this, window, cx| match res {
                Ok(list) => {
                    let me = cx.entity();
                    let picker = cx.new(|cx| BranchPicker::new(me, list, window, cx));
                    let first = std::cell::Cell::new(true);
                    window.open_pop_dialog(cx, move |dialog, window, cx| {
                        if first.replace(false) {
                            picker
                                .read(cx)
                                .filter
                                .read(cx)
                                .focus_handle(cx)
                                .focus(window, cx);
                        }
                        dialog
                            .title(t!("git.branches_title").to_string())
                            .w(px(W_DLG_S))
                            .child(picker.clone())
                    });
                }
                Err(e) => this.set_error(e, cx),
            });
        })
        .detach();
    }
}

fn age_text(a: locus_git::log::Age) -> String {
    use locus_git::log::Age::*;
    match a {
        Now => t!("git.age_now").into_owned(),
        Minutes(n) => t!("git.age_min", n = n.to_string()).into_owned(),
        Hours(n) => t!("git.age_hour", n = n.to_string()).into_owned(),
        Days(n) => t!("git.age_day", n = n.to_string()).into_owned(),
        Months(n) => t!("git.age_month", n = n.to_string()).into_owned(),
        Years(n) => t!("git.age_year", n = n.to_string()).into_owned(),
    }
}

/// The colour of a status letter, in the panel, on tabs and in the commit's file list.
pub(crate) fn letter_color(letter: char, t: &crate::shell::theme::Tokens) -> Hsla {
    match letter {
        'A' | 'U' => t.grn_text,
        'D' | 'C' => t.red_text,
        'R' => t.acc_text,
        _ => t.yel_text,
    }
}

impl Render for GitPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_msg {
            self.focus_msg = false;
            let h = self.msg.read(cx).focus_handle(cx);
            cx.defer_in(window, move |_, window, cx| h.focus(window, cx));
        }
        let body = match self.phase() {
            Phase::NoFolder => self.render_no_folder(cx),
            Phase::GitMissing => self.render_missing(cx),
            Phase::NoRepo => self.render_no_repo(cx),
            Phase::Ready => v_flex()
                .size_full()
                .child(self.render_branch_bar(cx))
                .children(self.render_notice(cx))
                .child(self.render_commit_box(cx))
                .child(self.render_tabs(cx))
                .child(match self.view {
                    View::Changes => self.render_changes(cx),
                    View::History => self.render_history(cx),
                })
                .into_any_element(),
        };
        div()
            .id("git-panel")
            .track_focus(&self.focus)
            .size_full()
            .child(body)
    }
}

// ---- branch picker --------------------------------------------------------------------

/// The body of the branch dialog.
struct BranchPicker {
    git: Entity<GitPanel>,
    all: Vec<locus_git::branch::BranchInfo>,
    filter: Entity<InputState>,
    _sub: Subscription,
}

impl BranchPicker {
    fn new(
        git: Entity<GitPanel>,
        all: Vec<locus_git::branch::BranchInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx
            .new(|cx| InputState::new(window, cx).placeholder(t!("git.branch_filter").to_string()));
        filter.read(cx).focus_handle(cx).focus(window, cx);
        let sub = cx.subscribe_in(
            &filter,
            window,
            |this, input, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => {
                    // Enter: the only match, else create the typed name.
                    let q = input.read(cx).value().trim().to_string();
                    let m = this.matches(&q);
                    if m.len() == 1 {
                        let b = m[0].clone();
                        this.pick(b.name, b.remote, window, cx);
                    } else if this.can_create(&q) {
                        this.create(q, window, cx);
                    }
                }
                _ => {}
            },
        );
        Self {
            git,
            all,
            filter,
            _sub: sub,
        }
    }

    fn matches(&self, q: &str) -> Vec<locus_git::branch::BranchInfo> {
        let q = q.to_lowercase();
        self.all
            .iter()
            .filter(|b| q.is_empty() || b.name.to_lowercase().contains(&q))
            .cloned()
            .collect()
    }

    fn can_create(&self, q: &str) -> bool {
        locus_git::branch::valid_name(q) && !self.all.iter().any(|b| b.name == q)
    }

    fn pick(&mut self, name: String, remote: bool, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        self.git
            .update(cx, |g, cx| g.switch_branch(name, remote, cx));
    }

    fn create(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        self.git.update(cx, |g, cx| g.create_branch(name, cx));
    }
}

impl Render for BranchPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        let q = self.filter.read(cx).value().trim().to_string();
        let list = self.matches(&q);
        let create = self.can_create(&q).then(|| q.clone());
        let mut col = v_flex().gap(px(S1));
        for (i, b) in list.into_iter().enumerate() {
            let (name, remote) = (b.name.clone(), b.remote);
            col = col.child(
                h_flex()
                    .id(("git-br", i))
                    .w_full()
                    .h(px(H_ROW))
                    .px(px(S4))
                    .gap(px(S4))
                    .items_center()
                    .rounded(px(R_MD))
                    .cursor_pointer()
                    .text_size(px(TX_BASE))
                    .text_color(t.tx)
                    .when(b.current, |d| d.bg(t.on))
                    .hover(|h| h.bg(t.hv))
                    .child(div().flex_none().w(px(14.)).when(b.current, |d| {
                        d.child(
                            crate::shell::icons::icon("check")
                                .size(px(NODE_ICON))
                                .text_color(t.acc_text),
                        )
                    }))
                    .child(div().flex_1().min_w_0().truncate().child(b.name.clone()))
                    .when(b.remote, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .text_size(px(TX_XS))
                                .text_color(t.tx3)
                                .child(t!("git.remote").into_owned()),
                        )
                    })
                    .when_some(b.upstream.clone().filter(|_| !b.remote), |d, u| {
                        d.child(
                            div()
                                .flex_none()
                                .text_size(px(TX_XS))
                                .text_color(t.tx3)
                                .child(u),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick(name.clone(), remote, window, cx)
                    })),
            );
        }
        if let Some(name) = create {
            let label = t!("git.create_branch", name = name.clone()).into_owned();
            col = col.child(
                h_flex()
                    .id("git-br-create")
                    .w_full()
                    .h(px(H_ROW))
                    .px(px(S4))
                    .gap(px(S4))
                    .items_center()
                    .rounded(px(R_MD))
                    .cursor_pointer()
                    .text_size(px(TX_BASE))
                    .text_color(t.acc_text)
                    .hover(|h| h.bg(t.hv))
                    .child(
                        crate::shell::icons::icon("plus")
                            .size(px(NODE_ICON))
                            .flex_none(),
                    )
                    .child(div().min_w_0().truncate().child(label))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.create(name.clone(), window, cx)
                    })),
            );
        }
        v_flex().gap(px(S4)).child(Input::new(&self.filter)).child(
            div()
                .id("git-br-list")
                .max_h(px(300.))
                .overflow_y_scroll()
                .child(col),
        )
    }
}
