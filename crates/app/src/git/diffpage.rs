//! The read-only diff tab: unified (default) or side by side, line numbers, green / red
//! line backgrounds and SQL colours for `.sql` files. It loads in the background and
//! reloads when the Git panel sees the work tree change.
use super::*;
use crate::shell::scale::*;
use crate::shell::seg::{SegItem, seg};
use locus_git::diff::{FileDiff, LineKind};
use std::ops::Range;
use std::rc::Rc;

const LINE_H: f32 = 20.0;
const NUM_W: f32 = 44.0;

#[derive(Clone)]
struct Cell {
    no: Option<u32>,
    kind: LineKind,
    text: String,
    sql: bool,
}

#[derive(Clone)]
enum DRow {
    File {
        name: String,
        added: usize,
        removed: usize,
        binary: bool,
    },
    Hunk(String),
    /// Unified: both numbers and one text.
    Line {
        old: Option<u32>,
        new: Option<u32>,
        kind: LineKind,
        text: String,
        sql: bool,
    },
    /// Side by side: the removed side and the added side of the same row.
    Pair(Option<Cell>, Option<Cell>),
}

enum Load {
    Loading,
    Failed(String),
    Ready,
}

pub(crate) struct DiffPage {
    git: Entity<GitPanel>,
    rt: tokio::runtime::Handle,
    spec: Option<DiffSpec>,
    state: Load,
    files: Vec<FileDiff>,
    rows: Vec<DRow>,
    split: bool,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    seen_rev: u64,
    seq: u64,
    _sub: Subscription,
}

fn is_sql(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".sql")
}

impl DiffPage {
    pub fn new(
        git: Entity<GitPanel>,
        rt: tokio::runtime::Handle,
        spec_json: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let spec = DiffSpec::from_json(spec_json);
        let seen_rev = git.read(cx).rev;
        let sub = cx.observe(&git, |this, git, cx| {
            let rev = git.read(cx).rev;
            let live = this
                .spec
                .as_ref()
                .is_some_and(|s| !matches!(s.what, DiffWhat::Commit { .. }));
            if rev != this.seen_rev && live {
                this.seen_rev = rev;
                this.load(cx);
            }
        });
        let mut me = Self {
            git,
            rt,
            spec,
            state: Load::Loading,
            files: Vec::new(),
            rows: Vec::new(),
            split: false,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            seen_rev,
            seq: 0,
            _sub: sub,
        };
        me.load(cx);
        me
    }

    pub fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }

    pub(crate) fn load(&mut self, cx: &mut Context<Self>) {
        let Some(spec) = self.spec.clone() else {
            self.state = Load::Failed(t!("git.diff_bad_spec").into_owned());
            return;
        };
        self.seq += 1;
        let seq = self.seq;
        let git = Git::with_path(&crate::settings::get(cx).git_path());
        let repo = Repo::new(git, PathBuf::from(&spec.root));
        let job = self.rt.spawn_blocking(move || -> R<Vec<FileDiff>> {
            match &spec.what {
                DiffWhat::Unstaged => repo.diff_file(&spec.path, false, false),
                DiffWhat::Staged => repo.diff_file(&spec.path, true, false),
                DiffWhat::Untracked => repo.diff_file(&spec.path, false, true),
                DiffWhat::Commit { hash, .. } => {
                    repo.diff_commit(hash, (!spec.path.is_empty()).then_some(spec.path.as_str()))
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let res = job
                .await
                .unwrap_or_else(|e| Err(GitError::Io(e.to_string())));
            let _ = this.update(cx, |this, cx| {
                if this.seq != seq {
                    return;
                }
                match res {
                    Ok(files) => {
                        this.files = files;
                        this.rebuild();
                        this.state = Load::Ready;
                    }
                    Err(e) => this.state = Load::Failed(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn rebuild(&mut self) {
        self.rows = if self.split {
            split_rows(&self.files)
        } else {
            unified_rows(&self.files)
        };
    }

    fn set_split(&mut self, split: bool, cx: &mut Context<Self>) {
        if self.split != split {
            self.split = split;
            self.rebuild();
            cx.notify();
        }
    }

    fn totals(&self) -> (usize, usize) {
        (
            self.files.iter().map(|f| f.added()).sum(),
            self.files.iter().map(|f| f.removed()).sum(),
        )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let Some(spec) = &self.spec else {
            return div().into_any_element();
        };
        let mode = match &spec.what {
            DiffWhat::Unstaged => t!("git.diff_unstaged").into_owned(),
            DiffWhat::Staged => t!("git.diff_staged").into_owned(),
            DiffWhat::Untracked => t!("git.diff_untracked").into_owned(),
            DiffWhat::Commit { short, .. } => {
                t!("git.diff_commit", short = short.clone()).into_owned()
            }
        };
        let (a, r) = self.totals();
        let me = cx.entity();
        let (m1, m2) = (me.clone(), me.clone());
        let items = vec![
            SegItem::new(
                "git-diff-unified",
                t!("git.diff_unified").into_owned(),
                !self.split,
                Rc::new(move |_, _, cx| m1.update(cx, |p, cx| p.set_split(false, cx))),
            ),
            SegItem::new(
                "git-diff-split",
                t!("git.diff_split").into_owned(),
                self.split,
                Rc::new(move |_, _, cx| m2.update(cx, |p, cx| p.set_split(true, cx))),
            ),
        ];
        let mono = cx.theme().mono_font_family.clone();
        h_flex()
            .id("git-diff-header")
            .flex_none()
            .h(px(H_ROW + S2))
            .px(px(S6))
            .gap(px(S5))
            .items_center()
            .border_b_1()
            .border_color(t.ln)
            .text_size(px(TX_SM))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(mono.clone())
                    .text_color(t.tx1)
                    .child(if spec.path.is_empty() {
                        t!("git.diff_whole").into_owned()
                    } else {
                        spec.path.clone()
                    }),
            )
            .child(div().flex_none().text_color(t.tx3).child(mode))
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(S3))
                    .font_family(mono)
                    .child(div().text_color(t.grn_text).child(format!("+{a}")))
                    .child(div().text_color(t.red_text).child(format!("−{r}"))),
            )
            .child(div().flex_1())
            .child(seg(items, cx))
            .child(
                ib(
                    "git-diff-reload",
                    crate::shell::icons::icon("refresh"),
                    H_CONTROL,
                    false,
                    cx,
                )
                .tooltip(tip(t!("git.refresh").into_owned()))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.load(cx);
                    this.git.update(cx, |g, cx| g.refresh(cx));
                })),
            )
            .into_any_element()
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let dark = t.dark;
        let Some(row) = self.rows.get(ix).cloned() else {
            return div().h(px(LINE_H)).into_any_element();
        };
        let base = h_flex()
            .id(("git-dl", ix))
            .w_full()
            .h(px(LINE_H))
            .items_center()
            .font_family(mono.clone())
            .text_size(px(TX_MONO));
        let tint = |k: LineKind| match k {
            LineKind::Add => Some(t.grn.opacity(if dark { 0.16 } else { 0.14 })),
            LineKind::Del => Some(t.red.opacity(if dark { 0.18 } else { 0.12 })),
            _ => None,
        };
        let num = |n: Option<u32>| {
            div()
                .flex_none()
                .w(px(NUM_W))
                .pr(px(S4))
                .text_right()
                .text_color(t.tx4)
                .child(n.map(|n| n.to_string()).unwrap_or_default())
        };
        let body = |text: String, sql: bool, kind: LineKind| -> AnyElement {
            let d = div()
                .flex_1()
                .min_w_0()
                .whitespace_nowrap()
                .text_color(t.tx);
            let d = if kind == LineKind::Note {
                d.text_color(t.tx3)
            } else {
                d
            };
            if sql && kind != LineKind::Note && !text.is_empty() {
                d.child(crate::history::page::highlighted(
                    text.replace('\t', "    ").into(),
                    dark,
                ))
                .into_any_element()
            } else {
                d.child(text.replace('\t', "    ")).into_any_element()
            }
        };
        match row {
            DRow::File {
                name,
                added,
                removed,
                binary,
            } => base
                .h(px(H_COMPACT + S2))
                .mt(px(S3))
                .px(px(S6))
                .gap(px(S4))
                .bg(t.hv)
                .font_family(cx.theme().font_family.clone())
                .text_size(px(TX_SM))
                .child(
                    div()
                        .text_color(t.tx1)
                        .font_weight(FontWeight::MEDIUM)
                        .child(name),
                )
                .child(div().text_color(t.grn_text).child(format!("+{added}")))
                .child(div().text_color(t.red_text).child(format!("−{removed}")))
                .when(binary, |d| {
                    d.child(
                        div()
                            .text_color(t.tx3)
                            .child(t!("git.diff_binary").into_owned()),
                    )
                })
                .into_any_element(),
            DRow::Hunk(h) => base
                .px(px(S6))
                .bg(t.hv)
                .text_color(t.tx3)
                .whitespace_nowrap()
                .child(h)
                .into_any_element(),
            DRow::Line {
                old,
                new,
                kind,
                text,
                sql,
            } => {
                let sign = match kind {
                    LineKind::Add => "+",
                    LineKind::Del => "−",
                    _ => " ",
                };
                base.when_some(tint(kind), |d, c| d.bg(c))
                    .child(num(old))
                    .child(num(new))
                    .child(
                        div()
                            .flex_none()
                            .w(px(14.))
                            .text_color(match kind {
                                LineKind::Add => t.grn_text,
                                LineKind::Del => t.red_text,
                                _ => t.tx4,
                            })
                            .child(sign),
                    )
                    .child(body(text, sql, kind))
                    .into_any_element()
            }
            DRow::Pair(l, r) => {
                let side = |c: Option<Cell>| -> Div {
                    let d = h_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .items_center()
                        .overflow_hidden();
                    match c {
                        None => d.bg(t.hv.opacity(0.5)),
                        Some(c) => d
                            .when_some(tint(c.kind), |d, col| d.bg(col))
                            .child(num(c.no))
                            .child(body(c.text, c.sql, c.kind)),
                    }
                };
                base.child(side(l))
                    .child(div().flex_none().w(px(1.)).h_full().bg(t.ln))
                    .child(side(r))
                    .into_any_element()
            }
        }
    }
}

fn unified_rows(files: &[FileDiff]) -> Vec<DRow> {
    let mut rows = Vec::new();
    let many = files.len() > 1;
    for f in files {
        let sql = is_sql(f.path());
        if many || f.binary {
            rows.push(DRow::File {
                name: f.path().to_string(),
                added: f.added(),
                removed: f.removed(),
                binary: f.binary,
            });
        }
        for h in &f.hunks {
            rows.push(DRow::Hunk(h.header.clone()));
            for l in &h.lines {
                rows.push(DRow::Line {
                    old: l.old_no,
                    new: l.new_no,
                    kind: l.kind,
                    text: l.text.clone(),
                    sql,
                });
            }
        }
    }
    rows
}

/// Removed lines on the left and added lines on the right, paired in order.
fn split_rows(files: &[FileDiff]) -> Vec<DRow> {
    let mut rows = Vec::new();
    let many = files.len() > 1;
    for f in files {
        let sql = is_sql(f.path());
        if many || f.binary {
            rows.push(DRow::File {
                name: f.path().to_string(),
                added: f.added(),
                removed: f.removed(),
                binary: f.binary,
            });
        }
        for h in &f.hunks {
            rows.push(DRow::Hunk(h.header.clone()));
            let (mut dels, mut adds): (Vec<Cell>, Vec<Cell>) = (Vec::new(), Vec::new());
            let flush = |rows: &mut Vec<DRow>, dels: &mut Vec<Cell>, adds: &mut Vec<Cell>| {
                for i in 0..dels.len().max(adds.len()) {
                    rows.push(DRow::Pair(dels.get(i).cloned(), adds.get(i).cloned()));
                }
                dels.clear();
                adds.clear();
            };
            for l in &h.lines {
                let cell = |no: Option<u32>| Cell {
                    no,
                    kind: l.kind,
                    text: l.text.clone(),
                    sql,
                };
                match l.kind {
                    LineKind::Del => dels.push(cell(l.old_no)),
                    LineKind::Add => adds.push(cell(l.new_no)),
                    LineKind::Context => {
                        flush(&mut rows, &mut dels, &mut adds);
                        rows.push(DRow::Pair(Some(cell(l.old_no)), Some(cell(l.new_no))));
                    }
                    LineKind::Note => {}
                }
            }
            flush(&mut rows, &mut dels, &mut adds);
        }
    }
    rows
}

impl Render for DiffPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        let header = self.render_header(cx);
        let note = |text: String, col: Hsla| -> AnyElement {
            div()
                .p(px(S7))
                .text_size(px(TX_BASE))
                .text_color(col)
                .whitespace_normal()
                .child(text)
                .into_any_element()
        };
        let body: AnyElement = match &self.state {
            Load::Loading => note(t!("git.loading").into_owned(), t.tx3),
            Load::Failed(e) => note(e.clone(), t.red_text),
            Load::Ready if self.rows.is_empty() => note(t!("git.diff_empty").into_owned(), t.tx3),
            Load::Ready => {
                let widest = (!self.split).then(|| {
                    self.rows
                        .iter()
                        .enumerate()
                        .max_by_key(|(_, r)| match r {
                            DRow::Line { text, .. } => text.chars().count(),
                            DRow::Hunk(h) => h.chars().count(),
                            _ => 0,
                        })
                        .map(|(i, _)| i)
                });
                let n = self.rows.len();
                let mut list = uniform_list(
                    "git-diff-rows",
                    n,
                    cx.processor(|this, range: Range<usize>, _, cx| {
                        range.map(|i| this.render_row(i, cx)).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
                .size_full();
                if let Some(Some(w)) = widest {
                    list = list
                        .with_horizontal_sizing_behavior(
                            gpui_kit::ListHorizontalSizingBehavior::Unconstrained,
                        )
                        .with_width_from_item(Some(w));
                }
                let mut wrap = div().relative().flex_1().min_h_0().child(list);
                wrap = wrap.vertical_scrollbar(&self.scroll);
                if !self.split {
                    wrap = wrap.horizontal_scrollbar(&self.scroll);
                }
                wrap.into_any_element()
            }
        };
        v_flex()
            .id("git-diff-page")
            .track_focus(&self.focus)
            .size_full()
            .child(header)
            .child(body)
    }
}
