//! Key browser (left pane of the Redis tab): `SCAN` (MATCH pattern, TYPE filter; never `KEYS`),
//! List / Tree view, type badge + TTL per key (one pipelined `TYPE`/`PTTL` per page), "Load more",
//! `DBSIZE`, DB switch, multi-select bulk delete, "+ key", context menus.
use super::*;
use crate::shell::menu;
use gpui_kit::component::Disableable as _;
use locus_core::redis::{self as core, KeyTree, TreeRow};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// Keys fetched per "Load more" (the scan loop stops after this many new keys).
const PAGE: usize = 500;
/// The tree stops growing here: refine the pattern instead.
const MAX_KEYS: usize = 200_000;
const ROW: f32 = 26.;
const TYPES: [&str; 8] = [
    "",
    "string",
    "hash",
    "list",
    "set",
    "zset",
    "stream",
    "ReJSON-RL",
];

pub struct Browser {
    ctx: Ctx,
    pattern: Entity<InputState>,
    ty: &'static str,
    list_mode: bool,
    dbsize: Option<u64>,
    keys: Vec<String>,
    seen: HashSet<String>,
    tree: KeyTree,
    meta: HashMap<String, (String, Option<i64>)>,
    expanded: HashSet<String>,
    rows: Vec<TreeRow>,
    cursor: u64,
    done: bool,
    loading: bool,
    error: Option<String>,
    selected: Option<String>,
    multi: HashSet<String>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    gen_: u64,
    task: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<RedisEvent> for Browser {}

impl Browser {
    pub fn new(
        ctx: Ctx,
        selected: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let pattern =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("redis.pattern").to_string()));
        let sub = cx.subscribe(&pattern, |this: &mut Self, _, ev: &InputEvent, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.reload(cx);
            }
        });
        let mut me = Self {
            ctx,
            pattern,
            ty: "",
            list_mode: false,
            dbsize: None,
            keys: Vec::new(),
            seen: HashSet::new(),
            tree: KeyTree::new(":"),
            meta: HashMap::new(),
            expanded: HashSet::new(),
            rows: Vec::new(),
            cursor: 0,
            done: false,
            loading: false,
            error: None,
            selected,
            multi: HashSet::new(),
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            gen_: 0,
            task: None,
            _subs: vec![sub],
        };
        me.reload(cx);
        me
    }

    pub fn dbsize(&self) -> Option<u64> {
        self.dbsize
    }

    pub fn select(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        self.selected = key;
        cx.notify();
    }

    /// Starts over: new scan from cursor 0 and a fresh `DBSIZE`.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.keys.clear();
        self.seen.clear();
        self.tree = KeyTree::new(":");
        self.meta.clear();
        self.rows.clear();
        self.multi.clear();
        self.cursor = 0;
        self.done = false;
        self.loading = false;
        self.error = None;
        self.dbsize = None;
        self.gen_ += 1;
        let ctx = self.ctx.clone();
        let g = self.gen_;
        run(
            cx,
            &ctx,
            |d| async move { d.dbsize().await },
            move |this: &mut Self, r, cx| {
                if this.gen_ == g {
                    this.dbsize = r.ok();
                    cx.emit(RedisEvent::Status);
                    cx.notify();
                }
            },
        )
        .detach();
        self.load_more(cx);
    }

    /// Re-reads type and TTL of one key (after the details pane changed it).
    pub fn refresh_meta(&mut self, key: String, cx: &mut Context<Self>) {
        let g = self.gen_;
        let k = key.clone();
        run(
            cx,
            &self.ctx,
            move |d| async move { d.type_ttl(&[k]).await },
            move |this: &mut Self, r, cx| {
                if this.gen_ == g
                    && let Some(m) = r.ok().and_then(|v| v.into_iter().next())
                {
                    this.meta.insert(key, m);
                    cx.notify();
                }
            },
        )
        .detach();
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.loading || self.done {
            return;
        }
        self.loading = true;
        let (cursor, g) = (self.cursor, self.gen_);
        let pattern = self.pattern.read(cx).value().trim().to_string();
        let ty = self.ty;
        let room = MAX_KEYS.saturating_sub(self.keys.len());
        let want = PAGE.min(room.max(1));
        self.task = Some(run(
            cx,
            &self.ctx,
            move |d| async move {
                let mut cursor = cursor;
                let mut keys: Vec<String> = Vec::new();
                for _ in 0..200 {
                    let (next, batch) = d
                        .scan(cursor, &pattern, (!ty.is_empty()).then_some(ty), 1000)
                        .await?;
                    cursor = next;
                    keys.extend(batch);
                    if cursor == 0 || keys.len() >= want {
                        break;
                    }
                }
                keys.sort();
                keys.dedup();
                let meta = d.type_ttl(&keys).await.unwrap_or_default();
                Ok((cursor, keys, meta))
            },
            move |this: &mut Self, r, cx| {
                if this.gen_ != g {
                    return;
                }
                this.loading = false;
                match r {
                    Ok((next, keys, meta)) => {
                        for (k, m) in keys.iter().zip(meta) {
                            this.meta.insert(k.clone(), m);
                        }
                        for k in keys {
                            if this.seen.insert(k.clone()) {
                                this.tree.insert(&k);
                                this.keys.push(k);
                            }
                        }
                        this.cursor = next;
                        this.done = next == 0 || this.keys.len() >= MAX_KEYS;
                        this.rebuild();
                    }
                    Err(e) => {
                        this.done = true;
                        this.error = Some(e);
                    }
                }
                cx.emit(RedisEvent::Status);
                cx.notify();
            },
        ));
        cx.notify();
    }

    fn rebuild(&mut self) {
        self.rows = if self.list_mode {
            core::flat_keys(&self.keys)
                .into_iter()
                .map(|k| TreeRow {
                    depth: 0,
                    label: k.clone(),
                    path: k,
                    is_key: true,
                    is_folder: false,
                    open: false,
                    count: 1,
                })
                .collect()
        } else {
            self.tree.rows(&self.expanded)
        };
    }

    fn toggle(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_string());
        }
        self.rebuild();
        cx.notify();
    }

    fn set_type(&mut self, ty: &'static str, cx: &mut Context<Self>) {
        if self.ty != ty {
            self.ty = ty;
            self.reload(cx);
        }
    }

    fn set_db(&mut self, db: u8, cx: &mut Context<Self>) {
        if self.ctx.cfg.db != db {
            self.ctx = self.ctx.with_db(db);
            self.expanded.clear();
            self.selected = None;
            cx.emit(RedisEvent::DbChanged(db));
            self.reload(cx);
        }
    }

    fn guard(&self, cx: &mut Context<Self>) -> bool {
        if self.ctx.locked() {
            cx.emit(RedisEvent::Locked);
            return false;
        }
        true
    }

    /// Deletes keys in chunks; the pattern mode scans the server so unloaded keys under a prefix go too.
    fn delete_keys(&mut self, keys: Vec<String>, pattern: Option<String>, cx: &mut Context<Self>) {
        if !self.guard(cx) {
            return;
        }
        run(
            cx,
            &self.ctx,
            move |d| async move {
                let mut n = 0u64;
                if let Some(p) = pattern {
                    let mut cursor = 0;
                    loop {
                        let (next, batch) = d.scan(cursor, &p, None, 1000).await?;
                        for chunk in batch.chunks(500) {
                            let mut a: Vec<String> = vec!["UNLINK".into()];
                            a.extend(chunk.iter().cloned());
                            n += d.cmd(&a).await?.int().max(0) as u64;
                        }
                        cursor = next;
                        if cursor == 0 {
                            break;
                        }
                    }
                } else {
                    for chunk in keys.chunks(500) {
                        let mut a: Vec<String> = vec!["UNLINK".into()];
                        a.extend(chunk.iter().cloned());
                        n += d.cmd(&a).await?.int().max(0) as u64;
                    }
                }
                Ok(n)
            },
            |this: &mut Self, r, cx| {
                match r {
                    Ok(n) => crate::shell::toast::show(t!("redis2.deleted", n = n).to_string(), cx),
                    Err(e) => this.error = Some(e),
                }
                cx.emit(RedisEvent::KeyChanged);
                this.reload(cx);
            },
        )
        .detach();
    }

    fn confirm_delete(
        &mut self,
        keys: Vec<String>,
        pattern: Option<String>,
        body: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.guard(cx) {
            return;
        }
        let me = cx.entity().downgrade();
        confirm(
            window,
            cx,
            t!("redis.delete_title").into_owned(),
            body,
            t!("redis.delete").into_owned(),
            true,
            Rc::new(move |_, cx| {
                let (k, p) = (keys.clone(), pattern.clone());
                let _ = me.update(cx, |this, cx| this.delete_keys(k, p, cx));
            }),
        );
    }

    /// "+ key" of one type: asks for name and first value, then writes it.
    fn create_key(&mut self, ty: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.guard(cx) {
            return;
        }
        let mut fields = vec![Field::line(t!("redis2.key_name").into_owned(), "")];
        match ty {
            "hash" => fields.extend([
                Field::line(t!("redis.field").into_owned(), ""),
                Field::text(t!("redis.value").into_owned(), ""),
            ]),
            "zset" => fields.extend([
                Field::line(t!("redis.member").into_owned(), ""),
                Field::line(t!("redis.score").into_owned(), "0"),
            ]),
            "set" => fields.push(Field::line(t!("redis.member").into_owned(), "")),
            _ => fields.push(Field::text(t!("redis.value").into_owned(), "")),
        }
        let me = cx.entity().downgrade();
        let ok: Rc<dyn Fn(Vec<String>, &mut Window, &mut App)> = Rc::new(move |v, _, cx| {
            let key = v[0].clone();
            if key.is_empty() {
                return;
            }
            let args: Vec<String> = match ty {
                "hash" => vec!["HSET".into(), key.clone(), v[1].clone(), v[2].clone()],
                "list" => vec!["RPUSH".into(), key.clone(), v[1].clone()],
                "set" => vec!["SADD".into(), key.clone(), v[1].clone()],
                "zset" if v[2].trim().parse::<f64>().is_ok() => {
                    vec!["ZADD".into(), key.clone(), v[2].trim().into(), v[1].clone()]
                }
                "string" => vec!["SET".into(), key.clone(), v[1].clone(), "NX".into()],
                "ReJSON-RL" if serde_json::from_str::<serde_json::Value>(&v[1]).is_ok() => vec![
                    "JSON.SET".into(),
                    key.clone(),
                    "$".into(),
                    v[1].clone(),
                    "NX".into(),
                ],
                _ => return,
            };
            let _ = me.update(cx, |this, cx| {
                run(
                    cx,
                    &this.ctx,
                    move |d| async move { d.cmd(&args).await.map(|r| r.text()) },
                    move |this: &mut Self, r, cx| {
                        match r {
                            Ok(_) => {
                                this.selected = Some(key.clone());
                                cx.emit(RedisEvent::OpenKey { key });
                                this.reload(cx);
                            }
                            Err(e) => this.error = Some(e),
                        }
                        cx.notify();
                    },
                )
                .detach();
            });
        });
        prompt(
            window,
            cx,
            t!(
                "redis2.new_key",
                ty = if ty == "ReJSON-RL" { "json" } else { ty }
            )
            .into_owned(),
            fields,
            t!("redis.add").into_owned(),
            ok,
        );
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let Some(r) = self.rows.get(ix).cloned() else {
            return div().h(px(ROW)).into_any_element();
        };
        let on = r.is_key
            && (self.selected.as_deref() == Some(r.path.as_str()) || self.multi.contains(&r.path));
        let path = r.path.clone();
        let m = if r.is_key {
            self.meta.get(&r.path).cloned()
        } else {
            None
        };
        let me = cx.entity().downgrade();
        let rp = r.clone();
        let lead: AnyElement = if r.is_folder {
            div()
                .flex_none()
                .w(px(52.))
                .text_color(t.tx3)
                .child(
                    crate::shell::icons::icon(if r.open {
                        "chevron-down"
                    } else {
                        "chevron-right"
                    })
                    .size(px(11.)),
                )
                .into_any_element()
        } else {
            let ty = m.as_ref().map(|m| m.0.clone()).unwrap_or_default();
            div()
                .flex_none()
                .w(px(52.))
                .child(type_pill(&ty, cx))
                .into_any_element()
        };
        let ttl = m
            .as_ref()
            .map(|m| core::fmt_ttl_short(m.1))
            .unwrap_or_default();
        h_flex()
            .id(("redis-key", ix))
            .w_full()
            .h(px(ROW))
            .pl(px(S3 + r.depth as f32 * 12.))
            .pr(px(S4))
            .gap(px(S3))
            .items_center()
            .rounded(px(R_SM))
            .cursor_pointer()
            .when(on, |d| d.bg(t.on))
            .hover(|d| d.bg(t.hv))
            .child(lead)
            .child(
                div()
                    .id(("redis-key-name", ix))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(TX_BASE))
                    .text_color(if r.is_folder { t.tx2 } else { t.tx })
                    .tooltip(tip(r.path.clone()))
                    .child(r.label.clone()),
            )
            .when(r.is_folder, |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(px(TX_XS))
                        .text_color(t.tx4)
                        .child(format!("({})", locus_core::fmt::count(r.count as u64))),
                )
            })
            .when(!ttl.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(px(TX_XS))
                        .text_color(if ttl == "∞" { t.tx4 } else { t.tx2 })
                        .child(ttl),
                )
            })
            .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                if r.is_folder {
                    this.toggle(&path, cx);
                } else if ev.modifiers().control || ev.modifiers().platform {
                    if !this.multi.remove(&path) {
                        this.multi.insert(path.clone());
                    }
                    cx.notify();
                } else {
                    this.multi.clear();
                    this.selected = Some(path.clone());
                    cx.emit(RedisEvent::OpenKey { key: path.clone() });
                    cx.notify();
                }
            }))
            .context_menu(Self::row_menu(me, rp))
            .into_any_element()
    }

    /// Right-click menu of a key row (copy, rename, TTL, delete) or of a folder (delete by prefix).
    fn row_menu(
        me: WeakEntity<Self>,
        rp: locus_core::redis::TreeRow,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        move |menu: PopupMenu, _, _| {
            if rp.is_folder {
                let (me, prefix, count) = (me.clone(), rp.path.clone(), rp.count);
                return menu.item(menu::danger(
                    t!("redis2.delete_prefix", prefix = prefix.clone()).into_owned(),
                    None,
                    move |w, cx| {
                        let prefix = prefix.clone();
                        let _ = me.update(cx, |this, cx| {
                            let body = t!(
                                "redis2.delete_prefix_body",
                                prefix = prefix.clone(),
                                n = count
                            )
                            .into_owned();
                            this.confirm_delete(
                                Vec::new(),
                                Some(core::prefix_pattern(&prefix)),
                                body,
                                w,
                                cx,
                            );
                        });
                    },
                ));
            }
            let key = rp.path.clone();
            let k1 = key.clone();
            let mut menu = menu.item(menu::item(
                t!("redis2.copy_name").into_owned(),
                Some("copy"),
                move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(k1.clone())),
            ));
            for (label, icon, act) in [
                (t!("redis.rename").into_owned(), "pencil", KeyAction::Rename),
                (t!("redis2.set_ttl").into_owned(), "history", KeyAction::Ttl),
            ] {
                let (me, key) = (me.clone(), key.clone());
                menu = menu.item(menu::item(label, Some(icon), move |_, cx| {
                    let _ = me.update(cx, |this, cx| {
                        this.selected = Some(key.clone());
                        cx.emit(RedisEvent::KeyAction {
                            key: key.clone(),
                            action: act,
                        });
                    });
                }));
            }
            let (me2, key2) = (me.clone(), key.clone());
            menu.separator().item(menu::danger(
                t!("redis.delete").into_owned(),
                None,
                move |w, cx| {
                    let key = key2.clone();
                    let _ = me2.update(cx, |this, cx| {
                        let body = t!("redis.delete_body", key = key.clone()).into_owned();
                        this.confirm_delete(vec![key], None, body, w, cx);
                    });
                },
            ))
        }
    }
}

impl Render for Browser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let top = self.render_top(cx);
        let body = self.render_keys(cx);
        let footer = self.render_footer(cx);
        v_flex()
            .id("redis-browser")
            .track_focus(&self.focus)
            .size_full()
            .child(top)
            .children(self.error.as_ref().map(|e| error_box(e, cx)))
            .child(body)
            .child(footer)
    }
}

impl Browser {
    /// DB picker, view mode, refresh and add-key buttons, the pattern field and the type filter.
    fn render_top(&self, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let me = cx.entity().downgrade();
        let db = self.ctx.cfg.db;
        let db_menu = Button::new("redis-db")
            .label(format!("DB {db}"))
            .icon(crate::shell::icons::icon("caret-down").size(px(11.)))
            .dropdown_menu({
                let me = me.clone();
                move |mut menu, _, _| {
                    for i in 0..16u8 {
                        let m = me.clone();
                        menu = menu.item(
                            PopupMenuItem::new(format!("DB {i}"))
                                .checked(i == db)
                                .on_click(move |_, _, cx| {
                                    let _ = m.update(cx, |this, cx| this.set_db(i, cx));
                                }),
                        );
                    }
                    menu
                }
            });
        let add_menu = Button::new("redis-add-key")
            .icon(crate::shell::icons::icon("plus").size(px(12.)))
            .tooltip(t!("redis2.add_key").to_string())
            .dropdown_menu({
                let me = me.clone();
                move |mut menu, _, _| {
                    for ty in &TYPES[1..] {
                        let (m, ty) = (me.clone(), *ty);
                        menu = menu.item(
                            PopupMenuItem::new(if ty == "ReJSON-RL" {
                                "json".to_string()
                            } else {
                                ty.to_string()
                            })
                            .on_click(move |_, w, cx| {
                                let _ = m.update(cx, |this, cx| this.create_key(ty, w, cx));
                            }),
                        );
                    }
                    menu
                }
            });
        let ty = self.ty;
        // Tag-style chips that wrap as a row: one rounded outline each, the active one filled.
        let types = h_flex()
            .flex_wrap()
            .gap(px(S3))
            .children(TYPES.iter().map(|v| {
                let v: &'static str = v;
                let on = ty == v;
                let label = if v.is_empty() {
                    t!("redis.type_all").into_owned()
                } else {
                    core::type_badge(v).to_string()
                };
                div()
                    .id(format!("redis-type-{v}"))
                    .flex_none()
                    .h(px(22.))
                    .px(px(S4))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(R_SM))
                    .border_1()
                    .border_color(if on { t.tx4 } else { t.ln })
                    .cursor_pointer()
                    .text_size(px(TX_XS))
                    .text_color(if on { t.tx } else { t.tx2 })
                    .when(on, |d| d.bg(t.on).font_weight(FontWeight::MEDIUM))
                    .hover(|d| d.bg(t.hv))
                    .child(label)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_type(v, cx)))
            }));
        let list_mode = self.list_mode;
        v_flex()
            .flex_none()
            .gap(px(S3))
            .px(px(S4))
            .py(px(S4))
            .border_b_1()
            .border_color(t.ln)
            .child(
                h_flex()
                    .gap(px(S3))
                    .items_center()
                    .child(db_menu)
                    .child(div().flex_1())
                    .child(
                        ib(
                            "redis-viewmode",
                            crate::shell::icons::icon(if list_mode { "table" } else { "folder" }),
                            H_CONTROL,
                            false,
                            cx,
                        )
                        .tooltip(tip(if list_mode {
                            t!("redis2.view_list")
                        } else {
                            t!("redis2.view_tree")
                        }
                        .into_owned()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.list_mode = !this.list_mode;
                            this.rebuild();
                            cx.notify();
                        })),
                    )
                    .child(
                        ib(
                            "redis-refresh",
                            crate::shell::icons::icon("refresh"),
                            H_CONTROL,
                            false,
                            cx,
                        )
                        .tooltip(tip(t!("redis.refresh").into_owned()))
                        .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    )
                    .child(add_menu),
            )
            .child(
                Input::new(&self.pattern).px(px(S5)).cleanable(true).prefix(
                    crate::shell::icons::icon("search")
                        .size(px(14.))
                        .text_color(t.tx3),
                ),
            )
            .child(types)
    }

    /// The key list, or a centred message while loading or when nothing matches.
    fn render_keys(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let n = self.rows.len();
        if n == 0 {
            let text = if self.loading {
                t!("redis.loading").into_owned()
            } else {
                t!("redis.no_keys").into_owned()
            };
            div()
                .flex_1()
                .min_h(px(120.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(TX_BASE))
                .text_color(t.tx3)
                .child(text)
                .into_any_element()
        } else {
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .px(px(S3))
                .child(
                    uniform_list(
                        "redis-keys",
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
    }

    /// Footer: scan progress or the bulk delete button, and Load more.
    fn render_footer(&self, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let scanned = match self.dbsize {
            Some(total) => t!(
                "redis2.scanned",
                n = locus_core::fmt::count(self.keys.len() as u64),
                total = locus_core::fmt::count(total)
            )
            .into_owned(),
            None => t!(
                "redis.keys_loaded",
                loaded = locus_core::fmt::count(self.keys.len() as u64)
            )
            .into_owned(),
        };
        let selected_n = self.multi.len();
        h_flex()
            .flex_none()
            .min_h(px(H_BAR))
            .px(px(S4))
            .gap(px(S4))
            .items_center()
            .border_t_1()
            .border_color(t.ln)
            .when(selected_n > 0, |f| {
                f.child(
                    Button::new("redis-bulk-del")
                        .danger()
                        .label(t!("redis2.delete_selected", n = selected_n).into_owned())
                        .on_click(cx.listener(|this, _, w, cx| {
                            let keys: Vec<String> = this.multi.iter().cloned().collect();
                            let body = t!("redis2.delete_many_body", n = keys.len()).into_owned();
                            this.confirm_delete(keys, None, body, w, cx);
                        })),
                )
            })
            .when(selected_n == 0, |f| {
                f.child(div().text_size(px(TX_XS)).text_color(t.tx3).child(scanned))
            })
            .child(div().flex_1())
            .when(!self.done, |f| {
                f.child(
                    Button::new("redis-more")
                        .label(if self.loading {
                            t!("redis.loading").into_owned()
                        } else {
                            t!("redis.load_more").into_owned()
                        })
                        .disabled(self.loading)
                        .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                )
            })
            .when(self.done && self.keys.len() >= MAX_KEYS, |f| {
                f.child(
                    div()
                        .text_size(px(TX_XS))
                        .text_color(t.tx3)
                        .child(t!("redis.capped", n = MAX_KEYS).into_owned()),
                )
            })
    }
}
