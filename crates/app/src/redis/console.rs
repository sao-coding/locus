//! Console: a command line (Enter / Ctrl+Enter, Up / Down for history) that
//! sends raw commands and shows the reply as a tree. Dangerous commands ask
//! first; writes are blocked on a locked Production connection.
use super::*;
use locus_core::redis::{self as core, CmdClass, LineKind, ReplyLine};

const MAX_REPLY_LINES: usize = 300;
const MAX_KEPT_LINES: usize = 2_000;
const MAX_HISTORY: usize = 200;

struct Entry {
    cmd: String,
    lines: Vec<ReplyLine>,
    ms: u128,
}

pub struct Console {
    ctx: Ctx,
    input: Entity<InputState>,
    out: Vec<Entry>,
    history: Vec<String>,
    /// Position while walking the history with Up / Down (`history.len()` = the new line).
    pos: usize,
    busy: bool,
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<RedisEvent> for Console {}

impl Console {
    pub fn new(ctx: Ctx, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("redis.console_hint").to_string())
        });
        let sub = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.submit(window, cx);
                }
            },
        );
        Self {
            ctx,
            input,
            out: Vec::new(),
            history: Vec::new(),
            pos: 0,
            busy: false,
            scroll: ScrollHandle::new(),
            _subs: vec![sub],
        }
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }

    fn note(&mut self, cmd: &str, text: String, kind: LineKind, cx: &mut Context<Self>) {
        self.push(
            Entry {
                cmd: cmd.to_string(),
                lines: vec![ReplyLine {
                    depth: 0,
                    label: String::new(),
                    text,
                    kind,
                }],
                ms: 0,
            },
            cx,
        );
    }

    fn push(&mut self, e: Entry, cx: &mut Context<Self>) {
        self.out.push(e);
        let mut total: usize = self.out.iter().map(|e| e.lines.len() + 1).sum();
        while total > MAX_KEPT_LINES && self.out.len() > 1 {
            total -= self.out.remove(0).lines.len() + 1;
        }
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let line = self.input.read(cx).value().trim().to_string();
        if line.is_empty() || self.busy {
            return;
        }
        let args = match core::split_command(&line) {
            Ok(a) if !a.is_empty() => a,
            Ok(_) => return,
            Err(e) => {
                self.note(&line, e, LineKind::Error, cx);
                return;
            }
        };
        if self.history.last() != Some(&line) {
            self.history.push(line.clone());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
        }
        self.pos = self.history.len();
        if let Some(why) = core::unsupported(&args) {
            let k = format!("redis.unsupported_{why}");
            let msg = t!(&k).into_owned();
            self.note(&line, format!("(error) {msg}"), LineKind::Error, cx);
            self.clear_input(window, cx);
            return;
        }
        if core::classify(&args) == CmdClass::Write && self.ctx.locked() {
            self.note(
                &line,
                format!("(error) {}", t!("redis.blocked_prod")),
                LineKind::Error,
                cx,
            );
            cx.emit(RedisEvent::Locked);
            return;
        }
        if let Some(d) = core::danger(&args) {
            let me = cx.entity().downgrade();
            let ok: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |window, cx| {
                let (line, args) = (line.clone(), args.clone());
                let _ = me.update(cx, |this, cx| this.send(line, args, window, cx));
            });
            let k = format!("redis.danger_{d}");
            let body = format!("{}\n\n{}", t!(&k), self.input.read(cx).value());
            confirm(
                window,
                cx,
                t!("redis.danger_title").into_owned(),
                body,
                t!("redis.run_anyway").into_owned(),
                true,
                ok,
            );
            return;
        }
        self.send(line, args, window, cx);
    }

    fn clear_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
    }

    fn send(
        &mut self,
        line: String,
        args: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_input(window, cx);
        self.busy = true;
        let t0 = std::time::Instant::now();
        let echo = line.clone();
        run(
            cx,
            &self.ctx,
            move |d| async move { d.cmd(&args).await },
            move |this: &mut Self, r, cx| {
                this.busy = false;
                let lines = match r {
                    Ok(reply) => core::reply_lines(&reply, MAX_REPLY_LINES),
                    Err(e) => vec![ReplyLine {
                        depth: 0,
                        label: String::new(),
                        text: format!("(error) {e}"),
                        kind: LineKind::Error,
                    }],
                };
                this.push(
                    Entry {
                        cmd: echo,
                        lines,
                        ms: t0.elapsed().as_millis(),
                    },
                    cx,
                );
            },
        )
        .detach();
        cx.notify();
    }

    fn walk_history(&mut self, up: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.history.is_empty() {
            return;
        }
        self.pos = if up {
            self.pos.saturating_sub(1)
        } else {
            (self.pos + 1).min(self.history.len())
        };
        let text = self.history.get(self.pos).cloned().unwrap_or_default();
        self.input.update(cx, |s, cx| s.set_value(text, window, cx));
    }
}

impl Render for Console {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let mut lines = v_flex().gap(px(S1));
        if self.out.is_empty() {
            lines = lines.child(
                div()
                    .text_size(px(TX_SM))
                    .text_color(t.tx3)
                    .child(t!("redis.console_empty").into_owned()),
            );
        }
        for e in &self.out {
            lines = lines.child(
                h_flex()
                    .mt(px(S4))
                    .gap(px(S4))
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.tx1)
                            .child(format!("> {}", e.cmd)),
                    )
                    .when(e.ms > 0, |h| {
                        h.child(
                            div()
                                .text_size(px(TX_XS))
                                .text_color(t.tx4)
                                .child(format!("{} ms", e.ms)),
                        )
                    }),
            );
            for l in &e.lines {
                let color = match l.kind {
                    LineKind::Error => t.red_text,
                    LineKind::Nil => t.tx4,
                    LineKind::Int => t.yel,
                    LineKind::Status => t.grn_text,
                    LineKind::Container => t.tx3,
                    LineKind::Str | LineKind::Index => t.tx,
                };
                lines = lines.child(
                    h_flex()
                        .pl(px(S5 + l.depth as f32 * 16.))
                        .gap(px(S3))
                        .font_family(mono.clone())
                        .text_size(px(12.))
                        .when(!l.label.is_empty(), |h| {
                            h.child(div().flex_none().text_color(t.tx4).child(l.label.clone()))
                        })
                        .child(
                            div()
                                .min_w_0()
                                .text_color(color)
                                .whitespace_normal()
                                .child(l.text.clone()),
                        ),
                );
            }
        }
        let db = self.ctx.cfg.db;
        v_flex()
            .id("redis-console")
            .size_full()
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let k = &ev.keystroke;
                if k.modifiers.control && k.key == "enter" {
                    this.submit(window, cx);
                    cx.stop_propagation();
                } else if !k.modifiers.control
                    && !k.modifiers.alt
                    && (k.key == "up" || k.key == "down")
                {
                    this.walk_history(k.key == "up", window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .id("redis-console-out")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("redis-console-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .px(px(S6))
                            .py(px(S5))
                            .child(lines),
                    )
                    .vertical_scrollbar(&self.scroll),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(S4))
                    .items_center()
                    .px(px(S6))
                    .py(px(S4))
                    .border_t_1()
                    .border_color(t.ln)
                    .child(
                        div()
                            .flex_none()
                            .font_family(mono)
                            .text_size(px(12.))
                            .text_color(t.tx3)
                            .child(format!("{}[{db}]>", self.ctx.name)),
                    )
                    .child(div().flex_1().child(Input::new(&self.input).px(px(S5))))
                    .child(
                        Button::new("redis-run")
                            .primary()
                            .label(t!("redis.run").to_string())
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}
