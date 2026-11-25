//! Key tab: one key, by type. Strings (text / JSON / hex), hashes, lists,
//! sets, sorted sets and streams, each paged: a big key is never loaded whole.
//! Writes go through the Production lock (`Ctx::locked`).
use super::*;
use db_redis::{KeyMeta, StreamEntry};
use gpui_kit::component::Icon;
use gpui_kit::component::{Disableable as _, Selectable as _};
use locus_core::redis::{self as core, ValueView};
use locus_core::redis_edit as core_edit;
use std::ops::Range;

/// Rows per page of lists and sorted sets; hash / set / stream scans use it as the batch size.
const PAGE: u64 = 100;
/// Most rows kept for cursor-scanned types.
const MAX_ROWS: usize = 5_000;
/// Largest string fetched for display.
const STR_MAX: usize = 256 * 1024;
const ROW: f32 = 28.;
const MAX_LINES: usize = 4_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum StrMode {
    Auto,
    Raw,
    Hex,
}

enum Payload {
    Nothing,
    Str(Vec<u8>),
    Hash(u64, Vec<(Vec<u8>, Vec<u8>)>),
    List(Vec<Vec<u8>>),
    Set(u64, Vec<Vec<u8>>),
    Zset(Vec<(Vec<u8>, f64)>),
    Stream(Vec<StreamEntry>),
}

enum Body {
    Loading,
    Missing,
    Str {
        raw: Vec<u8>,
        lines: Vec<SharedString>,
    },
    Hash {
        rows: Vec<(Vec<u8>, Vec<u8>)>,
        cursor: u64,
        done: bool,
    },
    List {
        rows: Vec<Vec<u8>>,
    },
    Set {
        rows: Vec<Vec<u8>>,
        cursor: u64,
        done: bool,
    },
    Zset {
        rows: Vec<(Vec<u8>, f64)>,
    },
    Stream {
        rows: Vec<StreamEntry>,
        done: bool,
    },
    Other,
}

/// The cell being edited in place (hash / list / zset / set rows).
struct CellEditor {
    ix: usize,
    /// 0 = first column (hash field, zset score), 1 = second (value, member).
    col: u8,
    input: Entity<InputState>,
    orig: String,
    _sub: Subscription,
}

/// The inline editor of a string value: the text as loaded (`base`) decides Save / Revert.
struct StrEditor {
    state: Entity<TextareaState>,
    base: String,
    _sub: Subscription,
}

pub struct KeyView {
    ctx: Ctx,
    key: String,
    meta: Option<KeyMeta>,
    body: Body,
    mode: StrMode,
    page: u64,
    mem: Option<u64>,
    structural: bool,
    deleted: bool,
    error: Option<String>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    gen_: u64,
    task: Option<Task<()>>,
    cell: Option<CellEditor>,
    /// The cell F2 edits: (row, column).
    sel: Option<(usize, u8)>,
    text: Option<StrEditor>,
}

impl EventEmitter<RedisEvent> for KeyView {}

fn lines_of(text: &str) -> Vec<SharedString> {
    text.lines()
        .take(MAX_LINES)
        .map(|l| {
            SharedString::from(if l.chars().count() > 4000 {
                l.chars().take(4000).collect::<String>()
            } else {
                l.to_string()
            })
        })
        .collect()
}

impl KeyView {
    pub fn new(ctx: Ctx, key: String, cx: &mut Context<Self>) -> Self {
        let mut me = Self {
            ctx,
            key,
            mem: None,
            structural: false,
            deleted: false,
            meta: None,
            body: Body::Loading,
            mode: StrMode::Auto,
            page: 0,
            error: None,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            gen_: 0,
            task: None,
            cell: None,
            sel: None,
            text: None,
        };
        me.load(cx);
        me
    }

    /// The text of a string value in a display mode.
    fn str_text(raw: &[u8], mode: StrMode) -> String {
        match mode {
            StrMode::Auto => match core::value_view(raw) {
                ValueView::Text(s) | ValueView::Json(s) | ValueView::Hex(s) => s,
            },
            StrMode::Raw => String::from_utf8_lossy(raw).into_owned(),
            StrMode::Hex => core::hex_dump(raw),
        }
    }

    fn set_str(raw: Vec<u8>, mode: StrMode) -> Body {
        Body::Str {
            lines: lines_of(&Self::str_text(&raw, mode)),
            raw,
        }
    }

    /// True while the string editor holds text that is not saved.
    pub fn is_dirty(&self, cx: &App) -> bool {
        self.text
            .as_ref()
            .is_some_and(|e| e.state.read(cx).value().as_ref() != e.base)
    }

    pub fn key_name(&self) -> &str {
        &self.key
    }

    /// The string editor exists while the loaded value is whole, valid UTF-8 and not in Hex mode.
    fn str_editable(&self) -> bool {
        let Body::Str { raw, .. } = &self.body else {
            return false;
        };
        let is_json = self.meta.as_ref().is_some_and(|m| m.ty == "ReJSON-RL");
        let total = self.meta.as_ref().map_or(0, |m| m.len);
        let whole = is_json || raw.len() as u64 == total;
        self.mode != StrMode::Hex && whole && std::str::from_utf8(raw).is_ok()
    }

    /// Creates the editor of a string value on first render after a load (it needs a window).
    fn ensure_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text.is_some() || !self.str_editable() {
            return;
        }
        let Body::Str { raw, .. } = &self.body else {
            return;
        };
        let shown = Self::str_text(raw, self.mode);
        let state = cx.new(|cx| TextareaState::new(window, cx).default_value(shown));
        // What the field reports is the baseline (it may normalise line endings).
        let base = state.read(cx).value().to_string();
        let sub = cx.subscribe(&state, |_, _, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                cx.notify();
            }
        });
        self.text = Some(StrEditor {
            state,
            base,
            _sub: sub,
        });
    }

    fn revert_string(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(e) = &self.text {
            let base = e.base.clone();
            e.state.update(cx, |s, cx| s.set_value(base, window, cx));
            cx.notify();
        }
    }

    fn save_string(&mut self, cx: &mut Context<Self>) {
        let Some(e) = &self.text else { return };
        let value = e.state.read(cx).value().to_string();
        if value == e.base || !self.guard(cx) {
            return;
        }
        let key = self.key.clone().into_bytes();
        if self.meta.as_ref().is_some_and(|m| m.ty == "ReJSON-RL") {
            if serde_json::from_str::<serde_json::Value>(&value).is_err() {
                crate::shell::toast::show(t!("redis.json_invalid").to_string(), cx);
                return;
            }
            let cmd = vec![b"JSON.SET".to_vec(), key, b"$".to_vec(), value.into_bytes()];
            return self.write(vec![cmd], cx);
        }
        let ttl = self.meta.as_ref().and_then(|m| m.ttl_ms);
        let version = health(&self.ctx.conn_id).and_then(|h| h.version);
        let keepttl = locus_core::redis_edit::supports_keepttl(version.as_deref());
        let plan = locus_core::redis_edit::plan_set_string(&key, value.as_bytes(), ttl, keepttl);
        self.write_plan(plan, cx);
    }

    /// Reads meta and the current page of the value in one round of commands.
    pub fn load(&mut self, cx: &mut Context<Self>) {
        self.gen_ += 1;
        let (g, key, page) = (self.gen_, self.key.clone(), self.page);
        self.error = None;
        self.task = Some(run(
            cx,
            &self.ctx,
            move |d| async move {
                let meta = d.key_meta(&key).await?;
                let mem = d.memory_usage(&key).await;
                let payload = match meta.ty.as_str() {
                    "string" => Payload::Str(d.get_prefix(&key, STR_MAX).await?),
                    "hash" => {
                        let (c, r) = d.hscan(&key, 0, PAGE as usize).await?;
                        Payload::Hash(c, r)
                    }
                    "list" => Payload::List(d.lrange(&key, page * PAGE, PAGE).await?),
                    "set" => {
                        let (c, r) = d.sscan(&key, 0, PAGE as usize).await?;
                        Payload::Set(c, r)
                    }
                    "zset" => Payload::Zset(d.zrange(&key, page * PAGE, PAGE).await?),
                    "stream" => Payload::Stream(d.xrange(&key, "-", PAGE).await?),
                    // RedisJSON module type: JSON.GET returns `[value]` for the `$` path.
                    "ReJSON-RL" => Payload::Str(unwrap_json(
                        &d.cmd(&["JSON.GET", key.as_str(), "$"]).await?.bytes(),
                    )),
                    _ => Payload::Nothing,
                };
                Ok((meta, mem, payload))
            },
            move |this: &mut Self, r, cx| {
                if this.gen_ != g {
                    return;
                }
                match r {
                    Ok((meta, mem, p)) => {
                        let mode = this.mode;
                        this.mem = mem;
                        this.body = match (meta.ty.as_str(), p) {
                            ("none", _) => Body::Missing,
                            (_, Payload::Str(raw)) => Self::set_str(raw, mode),
                            (_, Payload::Hash(c, rows)) => Body::Hash {
                                rows,
                                cursor: c,
                                done: c == 0,
                            },
                            (_, Payload::List(rows)) => Body::List { rows },
                            (_, Payload::Set(c, rows)) => Body::Set {
                                rows,
                                cursor: c,
                                done: c == 0,
                            },
                            (_, Payload::Zset(rows)) => Body::Zset { rows },
                            (_, Payload::Stream(rows)) => Body::Stream {
                                done: (rows.len() as u64) < PAGE,
                                rows,
                            },
                            _ => Body::Other,
                        };
                        this.meta = Some(meta);
                        this.text = None;
                        this.cell = None;
                    }
                    Err(e) => {
                        this.body = Body::Other;
                        this.error = Some(e);
                    }
                }
                cx.notify();
            },
        ));
        cx.notify();
    }

    /// The refresh button: reloading would drop an unsaved string.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.is_dirty(cx) {
            return crate::shell::toast::show(t!("redis2.unsaved_first").to_string(), cx);
        }
        self.load(cx);
    }

    /// Next batch of a cursor-scanned type (hash, set) or stream range.
    fn load_more(&mut self, cx: &mut Context<Self>) {
        let (g, key) = (self.gen_, self.key.clone());
        match &self.body {
            Body::Hash {
                cursor,
                done: false,
                ..
            } => {
                let c = *cursor;
                self.task = Some(run(
                    cx,
                    &self.ctx,
                    move |d| async move { d.hscan(&key, c, PAGE as usize).await },
                    move |this: &mut Self, r, cx| {
                        if this.gen_ == g {
                            match (r, &mut this.body) {
                                (Ok((c, more)), Body::Hash { rows, cursor, done }) => {
                                    rows.extend(more);
                                    (*cursor, *done) = (c, c == 0 || rows.len() >= MAX_ROWS);
                                }
                                (Err(e), _) => this.error = Some(e),
                                _ => {}
                            }
                            cx.notify();
                        }
                    },
                ));
            }
            Body::Set {
                cursor,
                done: false,
                ..
            } => {
                let c = *cursor;
                self.task = Some(run(
                    cx,
                    &self.ctx,
                    move |d| async move { d.sscan(&key, c, PAGE as usize).await },
                    move |this: &mut Self, r, cx| {
                        if this.gen_ == g {
                            match (r, &mut this.body) {
                                (Ok((c, more)), Body::Set { rows, cursor, done }) => {
                                    rows.extend(more);
                                    (*cursor, *done) = (c, c == 0 || rows.len() >= MAX_ROWS);
                                }
                                (Err(e), _) => this.error = Some(e),
                                _ => {}
                            }
                            cx.notify();
                        }
                    },
                ));
            }
            Body::Stream { rows, done: false } => {
                let from = rows
                    .last()
                    .map(|e| format!("({}", e.id))
                    .unwrap_or_else(|| "-".into());
                self.task = Some(run(
                    cx,
                    &self.ctx,
                    move |d| async move { d.xrange(&key, &from, PAGE).await },
                    move |this: &mut Self, r, cx| {
                        if this.gen_ == g {
                            match (r, &mut this.body) {
                                (Ok(more), Body::Stream { rows, done }) => {
                                    *done = (more.len() as u64) < PAGE
                                        || rows.len() + more.len() >= MAX_ROWS;
                                    rows.extend(more);
                                }
                                (Err(e), _) => this.error = Some(e),
                                _ => {}
                            }
                            cx.notify();
                        }
                    },
                ));
            }
            _ => {}
        }
    }

    fn goto_page(&mut self, page: u64, cx: &mut Context<Self>) {
        self.page = page;
        self.load(cx);
    }

    /// The grid's value viewer (JSON tree, hex) for the loaded string.
    fn open_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Body::Str { raw, .. } = &self.body else {
            return;
        };
        let text = String::from_utf8_lossy(raw).into_owned();
        let bytes = std::str::from_utf8(raw).is_err().then(|| raw.clone());
        crate::viewer::open(
            crate::viewer::ViewerInput {
                title: self.key.clone(),
                is_null: false,
                bytes,
                text,
                type_name: String::new(),
            },
            window,
            cx,
        );
    }

    fn set_mode(&mut self, mode: StrMode, cx: &mut Context<Self>) {
        if mode != self.mode && self.is_dirty(cx) {
            return crate::shell::toast::show(t!("redis2.unsaved_first").to_string(), cx);
        }
        self.mode = mode;
        self.text = None;
        if let Body::Str { raw, .. } = &self.body {
            self.body = Self::set_str(raw.clone(), mode);
        }
        cx.notify();
    }

    // ---- writes -------------------------------------------------------------------------

    /// Sends commands one after another, then reloads. Blocked while the Production lock is on.
    fn write(&mut self, cmds: Vec<Vec<Vec<u8>>>, cx: &mut Context<Self>) {
        self.write_cmds(cmds, false, cx);
    }

    /// An edit plan: in one MULTI / EXEC when the plan says so.
    fn write_plan(&mut self, plan: locus_core::redis_edit::EditPlan, cx: &mut Context<Self>) {
        if !plan.cmds.is_empty() {
            self.write_cmds(plan.cmds, plan.atomic, cx);
        }
    }

    fn write_cmds(&mut self, cmds: Vec<Vec<Vec<u8>>>, atomic: bool, cx: &mut Context<Self>) {
        if self.ctx.locked() {
            cx.emit(RedisEvent::Locked);
            return;
        }
        run(
            cx,
            &self.ctx,
            move |d| async move {
                if atomic {
                    return d.atomic(&cmds).await;
                }
                for c in cmds {
                    d.cmd(&c).await?;
                }
                Ok(())
            },
            |this: &mut Self, r: Result<(), String>, cx| {
                if let Err(e) = r {
                    crate::shell::toast::show(e.clone(), cx);
                    this.error = Some(e);
                    this.structural = false;
                    this.deleted = false;
                    // A failed save keeps what was typed.
                    if this.is_dirty(cx) {
                        return cx.notify();
                    }
                }
                let structural = std::mem::take(&mut this.structural);
                if structural {
                    let gone = this.deleted;
                    cx.emit(RedisEvent::Selected((!gone).then(|| this.key.clone())));
                    cx.emit(RedisEvent::KeyChanged);
                } else {
                    cx.emit(RedisEvent::MetaChanged(this.key.clone()));
                }
                this.load(cx);
            },
        )
        .detach();
    }

    fn cmd(parts: &[&str]) -> Vec<Vec<u8>> {
        parts.iter().map(|p| p.as_bytes().to_vec()).collect()
    }

    fn guard(&self, cx: &mut Context<Self>) -> bool {
        if self.ctx.locked() {
            cx.emit(RedisEvent::Locked);
            return false;
        }
        true
    }

    fn weak_ok(
        cx: &mut Context<Self>,
        f: impl Fn(&mut Self, Vec<String>, &mut Context<Self>) + 'static,
    ) -> Rc<dyn Fn(Vec<String>, &mut Window, &mut App)> {
        let me = cx.entity().downgrade();
        Rc::new(move |vals, _, cx| {
            let _ = me.update(cx, |this, cx| f(this, vals, cx));
        })
    }

    fn edit_string(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Body::Str { raw, .. } = &self.body else {
            return;
        };
        let is_json = self.meta.as_ref().is_some_and(|m| m.ty == "ReJSON-RL");
        let total = self.meta.as_ref().map_or(0, |m| m.len);
        let Ok(text) = String::from_utf8(raw.clone()) else {
            crate::shell::toast::show(t!("redis.binary_readonly").to_string(), cx);
            return;
        };
        let partial = !is_json && raw.len() as u64 != total;
        if partial || !self.guard(cx) {
            if partial {
                crate::shell::toast::show(t!("redis.too_big_edit").to_string(), cx);
            }
            return;
        }
        let ok = Self::weak_ok(cx, move |this, v, cx| {
            if is_json {
                if serde_json::from_str::<serde_json::Value>(&v[0]).is_err() {
                    crate::shell::toast::show(t!("redis.json_invalid").to_string(), cx);
                    return;
                }
                let cmd = vec![
                    b"JSON.SET".to_vec(),
                    this.key.clone().into_bytes(),
                    b"$".to_vec(),
                    v[0].clone().into_bytes(),
                ];
                this.write(vec![cmd], cx);
                return;
            }
            let cmd = vec![
                b"SET".to_vec(),
                this.key.clone().into_bytes(),
                v[0].clone().into_bytes(),
                b"KEEPTTL".to_vec(),
            ];
            this.write(vec![cmd], cx);
        });
        prompt(
            window,
            cx,
            t!("redis.edit_value").into_owned(),
            vec![Field::text(self.key.clone(), text)],
            t!("redis.save").into_owned(),
            ok,
        );
    }

    pub(super) fn set_ttl(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.guard(cx) {
            return;
        }
        let ok = Self::weak_ok(cx, |this, v, cx| {
            let s = v[0].trim();
            let key = this.key.as_str();
            if s.is_empty() || s == "0" || s.eq_ignore_ascii_case("persist") {
                this.write(vec![Self::cmd(&["PERSIST", key])], cx);
            } else if let Some(secs) = core::parse_ttl_input(s) {
                this.write(vec![Self::cmd(&["EXPIRE", key, &secs.to_string()])], cx);
            } else {
                crate::shell::toast::show(t!("redis.ttl_invalid").to_string(), cx);
            }
        });
        let cur = self
            .meta
            .as_ref()
            .and_then(|m| m.ttl_ms)
            .map(|ms| (ms / 1000).to_string())
            .unwrap_or_default();
        prompt(
            window,
            cx,
            t!("redis.ttl_title").into_owned(),
            vec![Field::line(t!("redis.ttl_hint").into_owned(), cur)],
            t!("redis.save").into_owned(),
            ok,
        );
    }

    pub(super) fn rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.guard(cx) {
            return;
        }
        let ok = Self::weak_ok(cx, |this, v, cx| {
            let new = v[0].trim().to_string();
            if new.is_empty() || new == this.key {
                return;
            }
            let old = std::mem::replace(&mut this.key, new.clone());
            // RENAME replaces an existing target: the user typed it, so that is intended.
            this.structural = true;
            this.write(vec![Self::cmd(&["RENAME", &old, &new])], cx);
        });
        prompt(
            window,
            cx,
            t!("redis.rename_title").into_owned(),
            vec![Field::line(
                t!("redis.new_name").into_owned(),
                self.key.clone(),
            )],
            t!("redis.rename").into_owned(),
            ok,
        );
    }

    pub(super) fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.guard(cx) {
            return;
        }
        let ok = Self::weak_ok(cx, |this, _, cx| {
            let key = this.key.clone();
            this.structural = true;
            this.deleted = true;
            this.write(vec![Self::cmd(&["DEL", &key])], cx);
        });
        let ok2 = ok.clone();
        confirm(
            window,
            cx,
            t!("redis.delete_title").into_owned(),
            t!("redis.delete_body", key = self.key.clone()).into_owned(),
            t!("redis.delete").into_owned(),
            true,
            Rc::new(move |w, cx| ok2(vec![], w, cx)),
        );
    }

    /// "Add" dialog of the current type.
    fn add_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.guard(cx) {
            return;
        }
        let ty = self.meta.as_ref().map(|m| m.ty.clone()).unwrap_or_default();
        let (title, fields): (String, Vec<Field>) = match ty.as_str() {
            "hash" => (
                t!("redis.add_field").into_owned(),
                vec![
                    Field::line(t!("redis.field").into_owned(), ""),
                    Field::text(t!("redis.value").into_owned(), ""),
                ],
            ),
            "list" => (
                t!("redis.push_tail").into_owned(),
                vec![Field::text(t!("redis.value").into_owned(), "")],
            ),
            "set" => (
                t!("redis.add_member").into_owned(),
                vec![Field::line(t!("redis.member").into_owned(), "")],
            ),
            "zset" => (
                t!("redis.add_member").into_owned(),
                vec![
                    Field::line(t!("redis.member").into_owned(), ""),
                    Field::line(t!("redis.score").into_owned(), "0"),
                ],
            ),
            _ => return,
        };
        let ok = Self::weak_ok(cx, move |this, v, cx| {
            let key = this.key.as_bytes().to_vec();
            let b = |i: usize| v[i].clone().into_bytes();
            let cmd = match ty.as_str() {
                "hash" if !v[0].is_empty() => vec![b"HSET".to_vec(), key, b(0), b(1)],
                "list" => vec![b"RPUSH".to_vec(), key, b(0)],
                "set" if !v[0].is_empty() => vec![b"SADD".to_vec(), key, b(0)],
                "zset" if v[1].trim().parse::<f64>().is_ok() && !v[0].is_empty() => {
                    vec![b"ZADD".to_vec(), key, v[1].trim().as_bytes().to_vec(), b(0)]
                }
                _ => return,
            };
            this.write(vec![cmd], cx);
        });
        prompt(window, cx, title, fields, t!("redis.add").into_owned(), ok);
    }

    fn push_head(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.guard(cx) {
            return;
        }
        let ok = Self::weak_ok(cx, |this, v, cx| {
            let cmd = vec![
                b"LPUSH".to_vec(),
                this.key.clone().into_bytes(),
                v[0].clone().into_bytes(),
            ];
            this.write(vec![cmd], cx);
        });
        prompt(
            window,
            cx,
            t!("redis.push_head").into_owned(),
            vec![Field::text(t!("redis.value").into_owned(), "")],
            t!("redis.add").into_owned(),
            ok,
        );
    }

    fn row_count(&self) -> usize {
        match &self.body {
            Body::Str { lines, .. } => lines.len(),
            Body::Hash { rows, .. } => rows.len(),
            Body::List { rows } | Body::Set { rows, .. } => rows.len(),
            Body::Zset { rows } => rows.len(),
            Body::Stream { rows, .. } => rows.len(),
            _ => 0,
        }
    }

    fn edit_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.guard(cx) {
            return;
        }
        let key = self.key.as_bytes().to_vec();
        match &self.body {
            Body::Hash { rows, .. } => {
                let Some((f, v)) = rows.get(ix).cloned() else {
                    return;
                };
                let Ok(value) = String::from_utf8(v) else {
                    return crate::shell::toast::show(t!("redis.binary_readonly").to_string(), cx);
                };
                let ok = Self::weak_ok(cx, move |this, vals, cx| {
                    this.write(
                        vec![vec![
                            b"HSET".to_vec(),
                            key.clone(),
                            f.clone(),
                            vals[0].clone().into_bytes(),
                        ]],
                        cx,
                    )
                });
                prompt(
                    window,
                    cx,
                    t!("redis.edit_value").into_owned(),
                    vec![Field::text(
                        String::from_utf8_lossy(&rows[ix].0).into_owned(),
                        value,
                    )],
                    t!("redis.save").into_owned(),
                    ok,
                );
            }
            Body::List { rows } => {
                let Some(v) = rows.get(ix).cloned() else {
                    return;
                };
                let Ok(value) = String::from_utf8(v) else {
                    return crate::shell::toast::show(t!("redis.binary_readonly").to_string(), cx);
                };
                let index = (self.page * PAGE + ix as u64).to_string().into_bytes();
                let ok = Self::weak_ok(cx, move |this, vals, cx| {
                    this.write(
                        vec![vec![
                            b"LSET".to_vec(),
                            key.clone(),
                            index.clone(),
                            vals[0].clone().into_bytes(),
                        ]],
                        cx,
                    )
                });
                prompt(
                    window,
                    cx,
                    t!("redis.edit_value").into_owned(),
                    vec![Field::text(
                        format!("[{}]", self.page * PAGE + ix as u64),
                        value,
                    )],
                    t!("redis.save").into_owned(),
                    ok,
                );
            }
            Body::Zset { rows } => {
                let Some((m, s)) = rows.get(ix).cloned() else {
                    return;
                };
                let ok = Self::weak_ok(cx, move |this, vals, cx| {
                    if vals[0].trim().parse::<f64>().is_ok() {
                        this.write(
                            vec![vec![
                                b"ZADD".to_vec(),
                                key.clone(),
                                vals[0].trim().as_bytes().to_vec(),
                                m.clone(),
                            ]],
                            cx,
                        );
                    } else {
                        crate::shell::toast::show(t!("redis.score_invalid").to_string(), cx);
                    }
                });
                prompt(
                    window,
                    cx,
                    t!("redis.edit_score").into_owned(),
                    vec![Field::line(t!("redis.score").into_owned(), s.to_string())],
                    t!("redis.save").into_owned(),
                    ok,
                );
            }
            _ => {}
        }
    }

    // ---- in-place cell edit -------------------------------------------------------------

    /// The bytes of one cell, when that cell can be edited.
    fn cell_bytes(&self, ix: usize, col: u8) -> Option<Vec<u8>> {
        match (&self.body, col) {
            (Body::Hash { rows, .. }, 0) => rows.get(ix).map(|r| r.0.clone()),
            (Body::Hash { rows, .. }, 1) => rows.get(ix).map(|r| r.1.clone()),
            (Body::List { rows }, 1) => rows.get(ix).cloned(),
            (Body::Set { rows, .. }, 1) => rows.get(ix).cloned(),
            (Body::Zset { rows }, 0) => rows.get(ix).map(|r| r.1.to_string().into_bytes()),
            (Body::Zset { rows }, 1) => rows.get(ix).map(|r| r.0.clone()),
            _ => None,
        }
    }

    /// The edit that turns cell (`ix`, `col`) into `new`.
    fn cell_edit(&self, ix: usize, col: u8, new: &str) -> Option<core_edit::CellEdit> {
        use core_edit::CellEdit as E;
        let nb = new.as_bytes().to_vec();
        Some(match (&self.body, col) {
            (Body::Hash { rows, .. }, 1) => E::HashValue {
                field: rows.get(ix)?.0.clone(),
                value: nb,
            },
            (Body::Hash { rows, .. }, 0) if !new.is_empty() => {
                let (old, value) = rows.get(ix)?.clone();
                E::HashField {
                    old,
                    new: nb,
                    value,
                }
            }
            (Body::List { rows }, 1) if ix < rows.len() => E::ListElem {
                index: self.page * PAGE + ix as u64,
                value: nb,
            },
            (Body::Set { rows, .. }, 1) => E::SetMember {
                old: rows.get(ix)?.clone(),
                new: nb,
            },
            (Body::Zset { rows }, 0) => E::ZsetScore {
                member: rows.get(ix)?.0.clone(),
                score: new.to_string(),
            },
            (Body::Zset { rows }, 1) => {
                let (old, score) = rows.get(ix)?.clone();
                E::ZsetMember {
                    old,
                    new: nb,
                    score,
                }
            }
            _ => return None,
        })
    }

    /// Double-click or F2: the cell turns into a text field.
    fn start_cell(&mut self, ix: usize, col: u8, window: &mut Window, cx: &mut Context<Self>) {
        self.sel = Some((ix, col));
        cx.notify();
        if self
            .cell
            .as_ref()
            .is_some_and(|c| c.ix == ix && c.col == col)
        {
            return;
        }
        let Some(bytes) = self.cell_bytes(ix, col) else {
            return;
        };
        if !self.guard(cx) {
            return;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            return crate::shell::toast::show(t!("redis.binary_readonly").to_string(), cx);
        };
        if text.contains(['\n', '\r']) {
            // A one-line field would flatten it: the dialog edits it whole.
            return if col == 1 && matches!(self.body, Body::Hash { .. } | Body::List { .. }) {
                self.edit_row(ix, window, cx)
            } else {
                crate::shell::toast::show(t!("redis.binary_readonly").to_string(), cx)
            };
        }
        let input = cx.new(|cx| {
            let mut s = InputState::new(window, cx);
            s.set_value(text.clone(), window, cx);
            s
        });
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } => this.commit_cell(window, cx),
                InputEvent::Blur => this.cancel_cell(cx),
                _ => {}
            },
        );
        input.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        self.cell = Some(CellEditor {
            ix,
            col,
            input,
            orig: text,
            _sub: sub,
        });
        cx.notify();
    }

    fn cancel_cell(&mut self, cx: &mut Context<Self>) {
        if self.cell.take().is_some() {
            cx.notify();
        }
    }

    /// Enter: sends the edit and reloads. A bad score keeps the field open.
    fn commit_cell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, col, new, orig)) = self.cell.as_ref().map(|c| {
            (
                c.ix,
                c.col,
                c.input.read(cx).value().to_string(),
                c.orig.clone(),
            )
        }) else {
            return;
        };
        let plan = match self.cell_edit(ix, col, &new) {
            Some(e) if new != orig => core_edit::plan_edit(self.key.as_bytes(), &e),
            _ => Ok(core_edit::EditPlan {
                cmds: vec![],
                atomic: false,
            }),
        };
        let Ok(plan) = plan else {
            return crate::shell::toast::show(t!("redis.score_invalid").to_string(), cx);
        };
        self.cell = None;
        self.focus.focus(window, cx);
        self.write_plan(plan, cx);
        cx.notify();
    }

    fn delete_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let key = self.key.as_bytes().to_vec();
        let cmds = match &self.body {
            Body::Hash { rows, .. } => rows
                .get(ix)
                .map(|(f, _)| vec![vec![b"HDEL".to_vec(), key, f.clone()]]),
            Body::Set { rows, .. } => rows
                .get(ix)
                .map(|m| vec![vec![b"SREM".to_vec(), key, m.clone()]]),
            Body::Zset { rows } => rows
                .get(ix)
                .map(|(m, _)| vec![vec![b"ZREM".to_vec(), key, m.clone()]]),
            // LREM removes by value: mark the element at the index, then remove the mark.
            Body::List { rows } if ix < rows.len() => {
                let index = (self.page * PAGE + ix as u64).to_string().into_bytes();
                let mark = format!("__wb_del_{}__", self.gen_).into_bytes();
                Some(vec![
                    vec![b"LSET".to_vec(), key.clone(), index, mark.clone()],
                    vec![b"LREM".to_vec(), key, b"1".to_vec(), mark],
                ])
            }
            _ => None,
        };
        if let Some(c) = cmds {
            self.write(c, cx);
        }
    }

    // ---- rendering ----------------------------------------------------------------------

    fn cells(&self, ix: usize) -> (String, String, bool, bool) {
        // (first column, second column, editable, removable)
        match &self.body {
            Body::Hash { rows, .. } => rows
                .get(ix)
                .map(|(f, v)| (core::cell_text(f, 120), core::cell_text(v, 400), true, true))
                .unwrap_or_default(),
            Body::List { rows } => rows
                .get(ix)
                .map(|v| {
                    (
                        (self.page * PAGE + ix as u64).to_string(),
                        core::cell_text(v, 400),
                        true,
                        true,
                    )
                })
                .unwrap_or_default(),
            Body::Set { rows, .. } => rows
                .get(ix)
                .map(|v| (String::new(), core::cell_text(v, 400), false, true))
                .unwrap_or_default(),
            Body::Zset { rows } => rows
                .get(ix)
                .map(|(m, s)| (s.to_string(), core::cell_text(m, 400), true, true))
                .unwrap_or_default(),
            Body::Stream { rows, .. } => rows
                .get(ix)
                .map(|e| {
                    (
                        e.id.clone(),
                        core::cell_text(
                            e.fields
                                .iter()
                                .map(|(k, v)| format!("{k}={v}"))
                                .collect::<Vec<_>>()
                                .join("  ")
                                .as_bytes(),
                            400,
                        ),
                        false,
                        false,
                    )
                })
                .unwrap_or_default(),
            _ => Default::default(),
        }
    }

    /// One cell of a row: its text, or the text field while it is edited. Double-click edits.
    fn cell_el(
        &self,
        ix: usize,
        col: u8,
        text: String,
        editable: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = crate::shell::theme::tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        // Padding and the negative margin cancel: the highlight reaches past the text, nothing moves.
        let el = div()
            .id(("redis-cell", ix * 2 + col as usize))
            .px(px(3.))
            .mx(px(-3.))
            .rounded(px(R_XS))
            .font_family(mono)
            .text_size(px(12.));
        if let Some(c) = self.cell.as_ref().filter(|c| c.ix == ix && c.col == col) {
            return el
                .on_action(
                    cx.listener(|this, _: &crate::shell::keymap::act::Escape, _, cx| {
                        this.cancel_cell(cx)
                    }),
                )
                .child(Input::new(&c.input).w_full());
        }
        el.truncate()
            .when(self.sel == Some((ix, col)), |d| d.bg(t.on))
            .when(editable, |d| {
                d.on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                    if ev.click_count() >= 2 {
                        this.start_cell(ix, col, window, cx);
                    } else {
                        this.sel = Some((ix, col));
                        cx.notify();
                    }
                }))
            })
            .child(text)
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        if let Body::Str { lines, .. } = &self.body {
            return div()
                .h(px(ROW - 6.))
                .px(px(S6))
                .font_family(mono)
                .text_size(px(12.))
                .text_color(t.tx)
                .whitespace_nowrap()
                .child(lines.get(ix).cloned().unwrap_or_default())
                .into_any_element();
        }
        let (a, b, editable, removable) = self.cells(ix);
        // Hash field names and zset scores are editable too; list indexes and set rows have no first column.
        let can0 = matches!(self.body, Body::Hash { .. } | Body::Zset { .. });
        let first_w = match &self.body {
            Body::List { .. } | Body::Zset { .. } => 90.,
            Body::Set { .. } => 0.,
            Body::Stream { .. } => 170.,
            _ => 220.,
        };
        h_flex()
            .id(("redis-row", ix))
            .w_full()
            .h(px(ROW))
            .px(px(S6))
            .gap(px(S5))
            .items_center()
            .hover(|d| d.bg(t.hv))
            .when(first_w > 0., |d| {
                d.child(
                    self.cell_el(ix, 0, a, can0, cx)
                        .flex_none()
                        .w(px(first_w))
                        .text_color(t.tx2),
                )
            })
            .child(
                self.cell_el(ix, 1, b, editable, cx)
                    .flex_1()
                    .min_w_0()
                    .text_color(t.tx),
            )
            .when(editable, |d| {
                d.child(
                    ib(
                        ("redis-edit", ix),
                        crate::shell::icons::icon("pencil"),
                        22.,
                        false,
                        cx,
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.edit_row(ix, window, cx)),
                    ),
                )
            })
            .when(removable, |d| {
                d.child(
                    ib(
                        ("redis-del", ix),
                        crate::shell::icons::icon("close"),
                        22.,
                        false,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.delete_row(ix, cx))),
                )
            })
            .into_any_element()
    }
}

impl Render for KeyView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        self.ensure_editor(window, cx);
        let ty = self.meta.as_ref().map(|m| m.ty.clone()).unwrap_or_default();
        let len = self.meta.as_ref().map_or(0, |m| m.len);
        let head = self.render_head(cx);
        let tools = self.render_tools(len, cx);
        let n = self.row_count();
        let body = self.render_body(n, &ty, cx);
        let pager = self.render_pager(n, len, cx);
        v_flex()
            .id("redis-key")
            .track_focus(&self.focus)
            .size_full()
            // F2 edits the selected cell.
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "f2"
                    && this.cell.is_none()
                    && let Some((ix, col)) = this.sel
                {
                    this.start_cell(ix, col, window, cx);
                }
            }))
            // Ctrl+S (the Save command, as bound) saves the string editor; elsewhere it passes on.
            .on_action(
                cx.listener(|this, _: &crate::shell::keymap::act::Save, _, cx| {
                    if this.is_dirty(cx) {
                        this.save_string(cx);
                    } else {
                        cx.propagate();
                    }
                }),
            )
            .child(head)
            .children(self.error.as_ref().map(|e| error_box(e, cx)))
            .children(tools.map(|x| {
                div()
                    .flex_none()
                    .px(px(S6))
                    .py(px(S4))
                    .border_b_1()
                    .border_color(t.ln2)
                    .child(x)
            }))
            .child(body)
            .children(pager.map(|p| {
                div()
                    .flex_none()
                    .px(px(S6))
                    .py(px(S3))
                    .border_t_1()
                    .border_color(t.ln)
                    .child(p)
            }))
    }
}

impl KeyView {
    /// Type pill, key name, refresh / copy / delete and the TTL, size and lock chips.
    fn render_head(&self, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let meta = self.meta.clone();
        let ty = meta.as_ref().map(|m| m.ty.clone()).unwrap_or_default();
        let locked = self.ctx.locked();
        let have = !ty.is_empty() && ty != "none";
        let mono = cx.theme().mono_font_family.clone();
        let chip = |text: String| {
            div()
                .flex_none()
                .px(px(S4))
                .h(px(20.))
                .flex()
                .items_center()
                .rounded(px(R_XS))
                .bg(t.hv)
                .text_size(px(TX_XS))
                .text_color(t.tx2)
                .child(text)
        };
        let acts = |id: &'static str, icon: Icon, tip_text: String, danger: bool, cx: &App| {
            let mut b = ib(id, icon, H_CONTROL, false, cx).tooltip(tip(tip_text));
            if danger {
                b = b.text_color(t.red_text);
            }
            b
        };
        let write_tip = |s: String| {
            if locked {
                t!("redis2.locked_tip").into_owned()
            } else {
                s
            }
        };
        let mut title = h_flex().flex_none().gap(px(S4)).items_center();
        if have {
            title = title.child(type_pill(&ty, cx));
        }
        title = title
            .child(
                div()
                    .id("redis-k-name")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .cursor_pointer()
                    .font_family(mono)
                    .text_size(px(TX_MD))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.tx1)
                    .tooltip(tip(write_tip(t!("redis2.click_rename").into_owned())))
                    .on_click(cx.listener(|this, _, window, cx| this.rename(window, cx)))
                    .child(self.key.clone()),
            )
            .child(
                acts(
                    "redis-k-refresh",
                    crate::shell::icons::icon("refresh"),
                    t!("redis.refresh").into_owned(),
                    false,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .when(have, |h| {
                h.child(
                    acts(
                        "redis-k-copy",
                        crate::shell::icons::icon("copy"),
                        t!("redis2.copy_name").into_owned(),
                        false,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(this.key.clone()))
                    })),
                )
                .child(
                    acts(
                        "redis-k-del",
                        crate::shell::icons::icon("trash"),
                        write_tip(t!("redis.delete").into_owned()),
                        true,
                        cx,
                    )
                    .when(locked, |d| d.opacity(0.45))
                    .on_click(cx.listener(|this, _, window, cx| this.delete(window, cx))),
                )
            });
        let mut chips = h_flex().flex_none().gap(px(S3)).items_center().flex_wrap();
        if let Some(m) = meta.clone().filter(|m| m.ty != "none") {
            chips = chips
                .child(
                    chip(format!(
                        "TTL {}",
                        if m.ttl_ms.is_none() {
                            "∞".to_string()
                        } else {
                            core::fmt_ttl(m.ttl_ms)
                        }
                    ))
                    .id("redis-k-ttl")
                    .cursor_pointer()
                    .when(locked, |d| d.opacity(0.45))
                    .tooltip(tip(write_tip(t!("redis2.click_ttl").into_owned())))
                    .on_click(cx.listener(|this, _, window, cx| this.set_ttl(window, cx))),
                )
                .child(chip(match m.ty.as_str() {
                    "string" => t!("redis.bytes", n = locus_core::fmt::count(m.len)).into_owned(),
                    _ => t!("redis.items", n = locus_core::fmt::count(m.len)).into_owned(),
                }))
                .children(
                    self.mem
                        .map(|b| chip(format!("{} {}", t!("redis2.memory"), core::fmt_bytes(b)))),
                );
        }
        if locked {
            chips = chips.child(
                div()
                    .flex_none()
                    .px(px(S4))
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .gap(px(S3))
                    .rounded(px(R_XS))
                    .bg(t.red.opacity(0.12))
                    .text_size(px(TX_XS))
                    .text_color(t.red_text)
                    .child(crate::shell::icons::icon("lock").size(px(10.)))
                    .child(t!("redis2.locked").into_owned()),
            );
        }
        v_flex()
            .flex_none()
            .gap(px(S3))
            .px(px(S6))
            .py(px(S4))
            .border_b_1()
            .border_color(t.ln)
            .child(title)
            .child(chips)
    }

    /// Type-specific tool row.
    fn render_tools(&self, len: u64, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = crate::shell::theme::tokens(cx);
        let btn = |id: &'static str, label: String| Button::new(id).label(label);
        let mode = self.mode;
        let dirty = self.is_dirty(cx);
        match &self.body {
            Body::Str { .. } => Some(
                h_flex()
                    .gap(px(S4))
                    .items_center()
                    .child(
                        btn("redis-s-auto", t!("redis.fmt_auto").into_owned())
                            .selected(mode == StrMode::Auto)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.set_mode(StrMode::Auto, cx)),
                            ),
                    )
                    .child(
                        btn("redis-s-raw", t!("redis.fmt_raw").into_owned())
                            .selected(mode == StrMode::Raw)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.set_mode(StrMode::Raw, cx)),
                            ),
                    )
                    .child(
                        btn("redis-s-hex", "Hex".into())
                            .selected(mode == StrMode::Hex)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.set_mode(StrMode::Hex, cx)),
                            ),
                    )
                    .child(
                        btn("redis-s-viewer", t!("viewer2.open").into_owned()).on_click(
                            cx.listener(|this, _, window, cx| this.open_viewer(window, cx)),
                        ),
                    )
                    .child(div().flex_1())
                    .when(len as usize > STR_MAX, |h| {
                        h.child(
                            div().text_size(px(TX_XS)).text_color(t.tx3).child(
                                t!("redis.str_capped", n = core::fmt_bytes(STR_MAX as u64))
                                    .into_owned(),
                            ),
                        )
                    })
                    // Editable text: Save / Revert act on the inline editor. Binary, partial or Hex
                    // values keep the dialog, which explains why they cannot be edited.
                    .when(self.text.is_none(), |h| {
                        h.child(
                            btn("redis-s-edit", t!("redis.edit_value").into_owned())
                                .primary()
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.edit_string(window, cx)),
                                ),
                        )
                    })
                    .when(self.text.is_some(), |h| {
                        h.when(dirty, |h| {
                            h.child(
                                div()
                                    .text_size(px(TX_XS))
                                    .text_color(t.yel)
                                    .child(format!("● {}", t!("redis2.modified"))),
                            )
                        })
                        .child(
                            btn("redis-s-revert", t!("redis2.revert").into_owned())
                                .disabled(!dirty)
                                .tooltip(t!("redis2.revert_tip").to_string())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.revert_string(window, cx)
                                })),
                        )
                        .child(
                            btn("redis-s-save", t!("redis.save").into_owned())
                                .primary()
                                .disabled(!dirty)
                                .on_click(cx.listener(|this, _, _, cx| this.save_string(cx))),
                        )
                    })
                    .into_any_element(),
            ),
            Body::List { .. } => Some(
                h_flex()
                    .gap(px(S4))
                    .child(
                        btn("redis-l-head", t!("redis.push_head").into_owned()).on_click(
                            cx.listener(|this, _, window, cx| this.push_head(window, cx)),
                        ),
                    )
                    .child(
                        btn("redis-l-tail", t!("redis.push_tail").into_owned())
                            .on_click(cx.listener(|this, _, window, cx| this.add_item(window, cx))),
                    )
                    .into_any_element(),
            ),
            Body::Hash { .. } | Body::Set { .. } | Body::Zset { .. } => Some(
                btn("redis-add", t!("redis.add").into_owned())
                    .on_click(cx.listener(|this, _, window, cx| this.add_item(window, cx)))
                    .into_any_element(),
            ),
            _ => None,
        }
    }

    /// The value list, or a centred message while loading, missing or empty.
    fn render_body(&self, n: usize, ty: &str, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        match &self.body {
            Body::Loading => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.tx3)
                .child(t!("redis.loading").into_owned())
                .into_any_element(),
            Body::Missing => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.tx3)
                .child(t!("redis.key_missing").into_owned())
                .into_any_element(),
            Body::Other => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.tx3)
                .child(t!("redis.type_unsupported", ty = ty.to_string()).into_owned())
                .into_any_element(),
            Body::Str { .. } if self.text.is_some() => div()
                .flex_1()
                .min_h_0()
                .p(px(S4))
                .children(self.text.as_ref().map(|e| {
                    Textarea::new(&e.state)
                        .h(relative(1.))
                        .font_family(mono.clone())
                }))
                .into_any_element(),
            _ if n == 0 => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.tx3)
                .child(t!("redis.empty").into_owned())
                .into_any_element(),
            _ => div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    uniform_list(
                        "redis-value",
                        n,
                        cx.processor(|this, range: Range<usize>, _, cx| {
                            range.map(|i| this.render_row(i, cx)).collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .size_full(),
                )
                .vertical_scrollbar(&self.scroll)
                .into_any_element(),
        }
    }

    /// Pager: list / zset by index range, the others load more.
    fn render_pager(&self, n: usize, len: u64, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = crate::shell::theme::tokens(cx);
        let btn = |id: &'static str, label: String| Button::new(id).label(label);
        match &self.body {
            Body::List { .. } | Body::Zset { .. } => {
                let (from, to) = (self.page * PAGE + 1, (self.page * PAGE + n as u64).min(len));
                Some(
                    h_flex()
                        .gap(px(S4))
                        .items_center()
                        .child(
                            btn("redis-prev", "<".into())
                                .disabled(self.page == 0)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.goto_page(this.page.saturating_sub(1), cx)
                                })),
                        )
                        .child(
                            btn("redis-next", ">".into())
                                .disabled((self.page + 1) * PAGE >= len)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.goto_page(this.page + 1, cx)),
                                ),
                        )
                        .child(
                            div().text_size(px(TX_XS)).text_color(t.tx3).child(
                                t!(
                                    "redis.range",
                                    from = from,
                                    to = to,
                                    total = locus_core::fmt::count(len)
                                )
                                .into_owned(),
                            ),
                        )
                        .into_any_element(),
                )
            }
            Body::Hash { done, .. } | Body::Set { done, .. } | Body::Stream { done, .. } => Some(
                h_flex()
                    .gap(px(S4))
                    .items_center()
                    .when(!*done, |h| {
                        h.child(
                            btn("redis-more", t!("redis.load_more").into_owned())
                                .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                        )
                    })
                    .child(div().text_size(px(TX_XS)).text_color(t.tx3).child(
                        t!("redis.shown", n = n, total = locus_core::fmt::count(len)).into_owned(),
                    ))
                    .into_any_element(),
            ),
            _ => None,
        }
    }
}

/// `JSON.GET key $` answers `[value]`: the value, pretty-printed (the reply as is if it does not parse).
fn unwrap_json(reply: &[u8]) -> Vec<u8> {
    let pretty =
        |v: &serde_json::Value| serde_json::to_string_pretty(v).map(String::into_bytes).ok();
    match serde_json::from_slice::<serde_json::Value>(reply) {
        Ok(serde_json::Value::Array(mut a)) if a.len() == 1 => pretty(&a.remove(0)),
        Ok(v) => pretty(&v),
        Err(_) => None,
    }
    .unwrap_or_else(|| reply.to_vec())
}
