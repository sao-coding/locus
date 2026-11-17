//! Toasts. gpui-component's notification pins its close button to the top-right corner,
//! which looks off on a one-line message, so the app draws its own: bottom-right, the
//! close button centred on the row. `show` works from any context; `ToastHost` is
//! rendered once by `Locus`.
use super::scale::*;
use super::*;

const LIFETIME: Duration = Duration::from_secs(4);
const MAX: usize = 4;

pub(crate) struct ToastHost {
    items: Vec<(u64, String)>,
    next: u64,
}

struct HostGlobal(Entity<ToastHost>);
impl Global for HostGlobal {}

pub(crate) fn init(cx: &mut App) -> Entity<ToastHost> {
    let host = cx.new(|_| ToastHost {
        items: Vec::new(),
        next: 0,
    });
    cx.set_global(HostGlobal(host.clone()));
    host
}

/// Shows `text` for a few seconds.
pub(crate) fn show(text: impl Into<String>, cx: &mut App) {
    let Some(host) = cx.try_global::<HostGlobal>().map(|g| g.0.clone()) else {
        return;
    };
    let text = text.into();
    host.update(cx, |h, cx| h.push(text, cx));
}

/// Like `show`, but a given `key` only once per run (a failing save would otherwise repeat on every change).
pub(crate) fn show_once(key: &'static str, text: impl Into<String>, cx: &mut App) {
    static SHOWN: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let first = SHOWN.lock().is_ok_and(|mut s| {
        let new = !s.contains(&key);
        if new {
            s.push(key);
        }
        new
    });
    if first {
        show(text, cx);
    }
}

impl ToastHost {
    fn push(&mut self, text: String, cx: &mut Context<Self>) {
        let id = self.next;
        self.next += 1;
        self.items.push((id, text));
        if self.items.len() > MAX {
            self.items.remove(0);
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LIFETIME).await;
            let _ = this.update(cx, |h, cx| h.dismiss(id, cx));
        })
        .detach();
        cx.notify();
    }

    fn dismiss(&mut self, id: u64, cx: &mut Context<Self>) {
        self.items.retain(|(i, _)| *i != id);
        cx.notify();
    }
}

impl Render for ToastHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        let mut col = v_flex()
            .absolute()
            .right(px(S6))
            .bottom(px(H_BAR + S6))
            .gap(px(S4))
            .items_end();
        for (id, text) in self.items.iter().cloned() {
            col = col.child(
                h_flex()
                    .id(("toast", id as usize))
                    .occlude()
                    .min_w(px(260.))
                    .max_w(px(420.))
                    .pl(px(S6))
                    .pr(px(S4))
                    .py(px(S5))
                    .gap(px(S5))
                    .items_center()
                    .rounded(px(R_LG))
                    .bg(t.pop)
                    .border_1()
                    .border_color(t.ln2)
                    .shadow(vec![BoxShadow {
                        color: t.shadow,
                        offset: point(px(0.), px(8.)),
                        blur_radius: px(24.),
                        spread_radius: px(0.),
                        inset: false,
                    }])
                    .text_size(px(TX_BASE))
                    .text_color(t.tx)
                    .child(div().flex_1().min_w_0().child(text))
                    .child(
                        ib(
                            ("toast-x", id as usize),
                            crate::shell::icons::icon("close"),
                            H_BADGE,
                            false,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.dismiss(id, cx))),
                    ),
            );
        }
        col
    }
}
