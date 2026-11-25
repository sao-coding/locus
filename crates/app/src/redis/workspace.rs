//! The Redis tab: one per connection and DB. Master / detail: the key browser on the left, the
//! selected key's details on the right (no tab per key), Console / Server info as a bottom panel.
use super::browser::Browser;
use super::console::Console;
use super::info::{InfoPage, SlowLog};
use super::key::KeyView;
use super::*;

/// The key browser pane is `list_width()` wide (dragged by its edge); the details keep at least this much.
const DETAIL_MIN_W: f32 = 360.;
const EDGE_W: f32 = 5.;
const PANEL_H: f32 = 300.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Console,
    Info,
    Slow,
}

pub struct RedisTab {
    ctx: Ctx,
    browser: Entity<Browser>,
    detail: Option<Entity<KeyView>>,
    key: Option<String>,
    panel: Option<Panel>,
    console: Option<Entity<Console>>,
    info: Option<Entity<InfoPage>>,
    slow: Option<Entity<SlowLog>>,
    /// Dragging the list edge: (pointer x, list width) at the press.
    drag: Option<(f32, f32)>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<RedisEvent> for RedisTab {}

impl RedisTab {
    pub fn new(ctx: Ctx, key: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let browser = cx.new(|cx| Browser::new(ctx.clone(), key.clone(), window, cx));
        let sub = cx.subscribe_in(&browser, window, |this, _, ev: &RedisEvent, window, cx| {
            this.on_browser(ev, window, cx)
        });
        let mut me = Self {
            ctx,
            browser,
            detail: None,
            key: None,
            panel: None,
            console: None,
            info: None,
            slow: None,
            drag: None,
            focus: cx.focus_handle(),
            _subs: vec![sub],
        };
        if let Some(k) = key {
            me.open_key(k, window, cx);
        }
        me
    }

    pub fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }

    pub fn db(&self) -> u8 {
        self.ctx.cfg.db
    }

    pub fn key(&self) -> Option<String> {
        self.key.clone()
    }

    pub fn key_count(&self, cx: &App) -> Option<u64> {
        self.browser.read(cx).dbsize()
    }

    /// Shows `key` in the details pane (also called when an old key tab is opened).
    pub fn open_key(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| KeyView::new(self.ctx.clone(), key.clone(), cx));
        let sub = cx.subscribe_in(&view, window, |this, _, ev: &RedisEvent, _, cx| {
            this.on_detail(ev, cx)
        });
        self._subs.truncate(1 + usize::from(self.console.is_some()));
        self._subs.push(sub);
        self.detail = Some(view);
        self.key = Some(key.clone());
        self.browser
            .update(cx, |b, cx| b.select(Some(key.clone()), cx));
        cx.emit(RedisEvent::Selected(Some(key)));
        cx.notify();
    }

    fn close_key(&mut self, cx: &mut Context<Self>) {
        self.detail = None;
        self.key = None;
        self.browser.update(cx, |b, cx| b.select(None, cx));
        cx.emit(RedisEvent::Selected(None));
        cx.notify();
    }

    fn on_browser(&mut self, ev: &RedisEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            RedisEvent::OpenKey { key, .. } => self.open_key(key.clone(), window, cx),
            RedisEvent::Locked => cx.emit(RedisEvent::Locked),
            RedisEvent::Status => cx.emit(RedisEvent::Status),
            RedisEvent::KeyChanged => {
                // A bulk delete may have removed the open key: show it as missing instead of stale data.
                if let Some(d) = &self.detail {
                    d.update(cx, |d, cx| d.load(cx));
                }
            }
            RedisEvent::DbChanged(db) => {
                self.ctx = self.ctx.with_db(*db);
                self.detail = None;
                self.key = None;
                self.console = None;
                self.info = None;
                self.slow = None;
                self.panel = None;
                self._subs.truncate(1);
                cx.emit(RedisEvent::DbChanged(*db));
                cx.emit(RedisEvent::Selected(None));
                cx.notify();
            }
            RedisEvent::KeyAction { key, action, .. } => {
                self.open_key(key.clone(), window, cx);
                if let Some(d) = self.detail.clone() {
                    let action = *action;
                    d.update(cx, |d, cx| match action {
                        KeyAction::Rename => d.rename(window, cx),
                        KeyAction::Ttl => d.set_ttl(window, cx),
                    });
                }
            }
            RedisEvent::Selected(_) | RedisEvent::MetaChanged(_) | RedisEvent::ListWidth => {}
        }
    }

    fn on_detail(&mut self, ev: &RedisEvent, cx: &mut Context<Self>) {
        match ev {
            RedisEvent::Locked => cx.emit(RedisEvent::Locked),
            RedisEvent::KeyChanged => self.browser.update(cx, |b, cx| b.reload(cx)),
            RedisEvent::MetaChanged(k) => self
                .browser
                .update(cx, |b, cx| b.refresh_meta(k.clone(), cx)),
            RedisEvent::Selected(Some(k)) => {
                self.key = Some(k.clone());
                self.browser
                    .update(cx, |b, cx| b.select(Some(k.clone()), cx));
                cx.emit(RedisEvent::Selected(Some(k.clone())));
            }
            RedisEvent::Selected(None) => self.close_key(cx),
            _ => {}
        }
    }

    fn toggle_panel(&mut self, which: Panel, window: &mut Window, cx: &mut Context<Self>) {
        if self.panel == Some(which) {
            self.panel = None;
        } else {
            match which {
                Panel::Console if self.console.is_none() => {
                    let view = cx.new(|cx| Console::new(self.ctx.clone(), window, cx));
                    let sub = cx.subscribe_in(&view, window, |_, _, ev: &RedisEvent, _, cx| {
                        if matches!(ev, RedisEvent::Locked) {
                            cx.emit(RedisEvent::Locked);
                        }
                    });
                    self._subs.insert(1, sub);
                    self.console = Some(view);
                }
                Panel::Info if self.info.is_none() => {
                    self.info = Some(cx.new(|cx| InfoPage::new(self.ctx.clone(), cx)))
                }
                Panel::Slow if self.slow.is_none() => {
                    self.slow = Some(cx.new(|cx| SlowLog::new(self.ctx.clone(), cx)))
                }
                _ => {}
            }
            self.panel = Some(which);
        }
        cx.notify();
    }
}

impl RedisTab {
    /// The list width as drawn: the saved one, but the details pane keeps room in a narrow window.
    fn left_width(window: &Window) -> f32 {
        let room = f32::from(window.viewport_size().width) - DETAIL_MIN_W - EDGE_W;
        list_width().min(room.max(LIST_W_MIN))
    }

    fn on_drag(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some((x0, w0)) = self.drag else { return };
        if ev.pressed_button != Some(MouseButton::Left) {
            return self.end_drag(cx);
        }
        set_list_width(w0 + f32::from(ev.position.x) - x0);
        cx.notify();
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            cx.emit(RedisEvent::ListWidth);
            cx.notify();
        }
    }
}

impl Render for RedisTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        let count = self.browser.read(cx).dbsize();
        let main: AnyElement = match &self.detail {
            Some(d) => d.clone().into_any_element(),
            None => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(S3))
                .text_color(t.tx3)
                .child(
                    crate::shell::icons::icon("key-round")
                        .size(px(28.))
                        .text_color(t.tx4),
                )
                .child(
                    div()
                        .text_size(px(TX_BASE))
                        .child(t!("redis2.pick_key").into_owned()),
                )
                .children(count.map(|n| {
                    div().text_size(px(TX_XS)).child(
                        t!(
                            "redis2.db_keys",
                            db = self.ctx.cfg.db,
                            n = locus_core::fmt::count(n)
                        )
                        .into_owned(),
                    )
                }))
                .into_any_element(),
        };
        let panel = self.panel;
        let dragging = self.drag.is_some();
        let toggle = |id: &'static str, label: String, which: Panel, cx: &mut Context<Self>| {
            let on = panel == Some(which);
            div()
                .id(id)
                .px(px(S4))
                .h(px(H_BAR - 6.))
                .flex()
                .items_center()
                .rounded(px(R_SM))
                .cursor_pointer()
                .text_size(px(TX_SM))
                .text_color(if on { t.tx } else { t.tx2 })
                .when(on, |d| d.bg(t.on))
                .hover(|d| d.bg(t.hv))
                .child(label)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.toggle_panel(which, window, cx)),
                )
        };
        let bar = h_flex()
            .flex_none()
            .h(px(H_BAR))
            .px(px(S4))
            .gap(px(S3))
            .items_center()
            .border_t_1()
            .border_color(t.ln)
            .child(toggle(
                "redis-panel-console",
                t!("redis.menu_console").into_owned(),
                Panel::Console,
                cx,
            ))
            .child(toggle(
                "redis-panel-info",
                t!("redis.menu_info").into_owned(),
                Panel::Info,
                cx,
            ))
            .child(toggle(
                "redis-panel-slow",
                t!("redis2.menu_slow").into_owned(),
                Panel::Slow,
                cx,
            ));
        let body: Option<AnyElement> = match self.panel {
            Some(Panel::Console) => self.console.clone().map(|c| c.into_any_element()),
            Some(Panel::Info) => self.info.clone().map(|c| c.into_any_element()),
            Some(Panel::Slow) => self.slow.clone().map(|c| c.into_any_element()),
            None => None,
        };
        h_flex()
            .id("redis-tab")
            .track_focus(&self.focus)
            .size_full()
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| this.on_drag(ev, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end_drag(cx)),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(Self::left_width(window)))
                    .h_full()
                    .overflow_hidden()
                    .child(self.browser.clone()),
            )
            // The draggable edge: a hairline centred in a wider hit area. `occlude` keeps the
            // press (and the cursor) from reaching what is under it.
            .child(
                div()
                    .id("redis-list-edge")
                    .flex_none()
                    .w(px(EDGE_W))
                    .h_full()
                    .flex()
                    .justify_center()
                    .occlude()
                    .cursor(CursorStyle::ResizeLeftRight)
                    .group("redis-edge")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                            this.drag = Some((f32::from(ev.position.x), list_width()));
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .w(px(1.))
                            .h_full()
                            .bg(if dragging { t.tx4 } else { t.ln })
                            .group_hover("redis-edge", |d| d.bg(t.tx4)),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(div().flex_1().min_h_0().child(main))
                    .child(bar)
                    .children(body.map(|b| {
                        div()
                            .flex_none()
                            .h(px(PANEL_H))
                            .border_t_1()
                            .border_color(t.ln)
                            .child(b)
                    })),
            )
    }
}
