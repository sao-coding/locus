//! Redis views: key browser, key tabs, console, server info. Redis is not SQL,
//! so these are page tabs (`TabPage::Redis`, spec in the tab's `sql`) that talk
//! to `db_redis` directly; the shell (Spaces, tabs, connections, Production
//! lock) is shared.
use crate::shell::scale::*;
use crate::shell::*;
use db_redis::{Config, RedisDriver};
use gpui_kit::component::input::{Textarea, TextareaState};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::future::Future;
use std::rc::Rc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

pub mod browser;
pub mod console;
pub mod info;
pub mod key;
pub mod ops;
pub mod workspace;

/// What a Redis view asks the shell for.
pub enum RedisEvent {
    OpenKey {
        key: String,
    },
    /// A write on a locked Production connection: open the unlock dialog.
    Locked,
    /// The loaded key count or connection state changed (status bar).
    Status,
    /// A key was renamed, deleted or changed: the list reloads.
    KeyChanged,
    /// TTL or contents of this key changed: refresh its row in the list.
    MetaChanged(String),
    /// The key in the details pane changed (`None` = nothing); persisted in the tab spec.
    Selected(Option<String>),
    DbChanged(u8),
    /// The key list was resized: the shell saves the width.
    ListWidth,
    /// Context-menu action on a key of the list.
    KeyAction {
        key: String,
        action: KeyAction,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Rename,
    Ttl,
}

/// Connection state of a Redis connection as the last command saw it (the shell's status bar and
/// sidebar dot read it: Redis views do not use the SQL pool).
#[derive(Clone, Debug, Default)]
pub struct Health {
    pub ok: bool,
    pub failed: bool,
    pub version: Option<String>,
    pub latency_ms: Option<u128>,
    pub db: u8,
    pub keys: Option<u64>,
}

static HEALTH: LazyLock<Mutex<HashMap<String, Health>>> = LazyLock::new(Default::default);

pub fn health(conn_id: &str) -> Option<Health> {
    HEALTH.lock().get(conn_id).cloned()
}

pub fn set_health(conn_id: &str, f: impl FnOnce(&mut Health)) {
    f(HEALTH.lock().entry(conn_id.to_string()).or_default());
}

/// Width of the key list, shared by all Redis tabs and saved in spaces.json.
pub const LIST_W_MIN: f32 = 220.;
pub const LIST_W_MAX: f32 = 640.;
/// Bits of 340.0_f32.
static LIST_W: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x43aa_0000);

pub fn list_width() -> f32 {
    f32::from_bits(LIST_W.load(Ordering::Relaxed))
}

pub fn set_list_width(w: f32) {
    let w = if w.is_finite() { w } else { 340. };
    LIST_W.store(w.clamp(LIST_W_MIN, LIST_W_MAX).to_bits(), Ordering::Relaxed);
}

/// Everything a view needs to talk to one connection and DB index.
#[derive(Clone)]
pub struct Ctx {
    pub conn_id: String,
    pub name: String,
    pub cfg: Config,
    pub rt: tokio::runtime::Handle,
    /// The pool's read-only flag: set while a Production connection is locked.
    pub ro: Arc<AtomicBool>,
}

impl Ctx {
    pub fn locked(&self) -> bool {
        self.ro.load(Ordering::SeqCst)
    }

    pub fn with_db(&self, db: u8) -> Ctx {
        let mut c = self.clone();
        c.cfg.db = db;
        c
    }
}

type Slot = Arc<tokio::sync::Mutex<Option<Arc<RedisDriver>>>>;
static CACHE: LazyLock<Mutex<HashMap<(String, u8), (Config, Slot)>>> =
    LazyLock::new(Default::default);

/// One shared driver per (connection, DB index); replaced when the settings changed.
fn slot(ctx: &Ctx) -> Slot {
    let mut m = CACHE.lock();
    let key = (ctx.conn_id.clone(), ctx.cfg.db);
    match m.get(&key) {
        Some((cfg, s)) if *cfg == ctx.cfg => s.clone(),
        _ => {
            let s: Slot = Default::default();
            m.insert(key, (ctx.cfg.clone(), s.clone()));
            s
        }
    }
}

fn connection_lost(e: &str) -> bool {
    [
        "IoError",
        "onnection",
        "broken pipe",
        "reset",
        "closed",
        "timed out",
        "os error",
        "timeout",
    ]
    .iter()
    .any(|k| e.contains(k))
}

/// Runs `op` on the tokio runtime with the shared driver (connecting first).
pub async fn exec<T, F, Fut>(ctx: &Ctx, op: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(Arc<RedisDriver>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, String>> + Send + 'static,
{
    let (slot, cfg, conn_id) = (slot(ctx), ctx.cfg.clone(), ctx.conn_id.clone());
    ctx.rt
        .spawn(async move {
            let d = {
                let mut g = slot.lock().await;
                match g.as_ref() {
                    Some(d) => d.clone(),
                    None => {
                        let db = cfg.db;
                        let d = Arc::new(
                            RedisDriver::connect(cfg)
                                .await
                                .inspect_err(|_| set_health(&conn_id, |h| h.failed = true))?,
                        );
                        let version = d.test().await.ok();
                        set_health(&conn_id, |h| {
                            h.version = version;
                            h.db = db;
                        });
                        *g = Some(d.clone());
                        d
                    }
                }
            };
            let started = std::time::Instant::now();
            let r = op(d.clone()).await;
            set_health(&conn_id, |h| {
                h.ok = r.as_ref().is_ok() || !r.as_ref().is_err_and(|e| connection_lost(e));
                h.failed = r.as_ref().is_err_and(|e| connection_lost(e));
                h.latency_ms = Some(started.elapsed().as_millis());
            });
            if r.as_ref().is_err_and(|e| connection_lost(e)) {
                let mut g = slot.lock().await;
                if g.as_ref().is_some_and(|x| Arc::ptr_eq(x, &d)) {
                    *g = None;
                }
            }
            r
        })
        .await
        .map_err(|e| e.to_string())?
}

/// `exec` from a view: `done` runs on the UI thread with the result. Keep the
/// task to cancel it by replacing it; `.detach()` it for fire-and-forget.
pub fn run<V, T, F, Fut>(
    cx: &mut Context<V>,
    ctx: &Ctx,
    op: F,
    done: impl FnOnce(&mut V, Result<T, String>, &mut Context<V>) + 'static,
) -> Task<()>
where
    V: 'static,
    T: Send + 'static,
    F: FnOnce(Arc<RedisDriver>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, String>> + Send + 'static,
{
    let ctx = ctx.clone();
    cx.spawn(async move |this, cx| {
        let res = exec(&ctx, op).await;
        let _ = this.update(cx, |v, cx| done(v, res, cx));
    })
}

/// `PING` + `INFO server` of a not yet saved connection: the Redis version.
pub async fn test_connection(rt: tokio::runtime::Handle, cfg: Config) -> Result<String, String> {
    rt.spawn(async move {
        let d = RedisDriver::connect(cfg)
            .await
            .map_err(|e| format!("{}: {e}", t!("redis.err_connect")))?;
        d.test()
            .await
            .map_err(|e| format!("{}: {e}", t!("redis.err_command")))
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---- dialogs ----------------------------------------------------------------------------

pub struct Field {
    pub label: String,
    pub value: String,
    pub multiline: bool,
}

impl Field {
    pub fn line(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            multiline: false,
        }
    }

    pub fn text(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            multiline: true,
        }
    }
}

#[derive(Clone)]
enum Fld {
    One(Entity<InputState>),
    Many(Entity<TextareaState>),
}

impl Fld {
    fn value(&self, cx: &App) -> String {
        match self {
            Fld::One(s) => s.read(cx).value().to_string(),
            Fld::Many(s) => s.read(cx).value().to_string(),
        }
    }
}

/// A dialog with labelled fields; `ok` gets the values when the user confirms.
pub fn prompt(
    window: &mut Window,
    cx: &mut App,
    title: String,
    fields: Vec<Field>,
    ok_label: String,
    ok: Rc<dyn Fn(Vec<String>, &mut Window, &mut App)>,
) {
    let states: Vec<(String, bool, Fld)> = fields
        .into_iter()
        .map(|f| {
            let s = if f.multiline {
                Fld::Many(cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .rows(8)
                        .default_value(f.value.clone())
                }))
            } else {
                Fld::One(cx.new(|cx| {
                    let mut s = InputState::new(window, cx);
                    s.set_value(f.value.clone(), window, cx);
                    s
                }))
            };
            (f.label, f.multiline, s)
        })
        .collect();
    if let Some((_, _, first)) = states.first() {
        match first {
            Fld::One(s) => s.read(cx).focus_handle(cx).focus(window, cx),
            Fld::Many(s) => s.read(cx).focus_handle(cx).focus(window, cx),
        }
    }
    window.open_pop_dialog(cx, move |dialog, _, cx| {
        let t = crate::shell::theme::tokens(cx);
        let mut body = v_flex().gap(px(S5));
        for (label, _, fld) in &states {
            let input = match fld {
                Fld::One(s) => Input::new(s).px(px(S5)).into_any_element(),
                Fld::Many(s) => Textarea::new(s).h(px(220.)).into_any_element(),
            };
            body = body.child(
                v_flex()
                    .gap(px(S3))
                    .child(
                        div()
                            .text_size(px(TX_SM))
                            .text_color(t.tx2)
                            .child(label.clone()),
                    )
                    .child(input),
            );
        }
        let (ok, fields) = (
            ok.clone(),
            states.iter().map(|s| s.2.clone()).collect::<Vec<_>>(),
        );
        dialog.title(title.clone()).w(px(W_DLG_M)).child(
            body.child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1())
                    .child(
                        Button::new("redis-prompt-cancel")
                            .label(t!("common.cancel").to_string())
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("redis-prompt-ok")
                            .primary()
                            .label(ok_label.clone())
                            .on_click(move |_, window, cx| {
                                let values: Vec<String> =
                                    fields.iter().map(|f| f.value(cx)).collect();
                                window.close_dialog(cx);
                                ok(values, window, cx);
                            }),
                    ),
            ),
        )
    });
}

/// "Are you sure" dialog; `danger` paints the confirm button red.
pub fn confirm(
    window: &mut Window,
    cx: &mut App,
    title: String,
    body: String,
    ok_label: String,
    danger: bool,
    ok: Rc<dyn Fn(&mut Window, &mut App)>,
) {
    window.open_pop_dialog(cx, move |dialog, _, _| {
        let ok = ok.clone();
        let mut b = Button::new("redis-confirm-ok").label(ok_label.clone());
        b = if danger { b.danger() } else { b.primary() };
        dialog.title(title.clone()).w(px(W_DLG_S)).child(
            v_flex()
                .gap_3()
                .child(div().text_sm().whitespace_normal().child(body.clone()))
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1())
                        .child(
                            Button::new("redis-confirm-cancel")
                                .label(t!("common.cancel").to_string())
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(b.on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            ok(window, cx);
                        })),
                ),
        )
    });
}

// ---- small shared widgets ------------------------------------------------------------------

pub fn type_color(ty: &str, cx: &App) -> Hsla {
    let t = crate::shell::theme::tokens(cx);
    match ty {
        "string" => t.grn_text,
        "hash" => t.tx2,
        "list" => t.yel,
        "set" => t.red_text,
        "zset" => t.tx3,
        "stream" => t.tx2,
        _ => t.tx4,
    }
}

/// Small coloured type pill.
pub fn type_pill(ty: &str, cx: &App) -> Div {
    let t = crate::shell::theme::tokens(cx);
    div()
        .flex_none()
        .px(px(S3))
        .h(px(16.))
        .flex()
        .items_center()
        .rounded(px(R_XS))
        .bg(t.hv)
        .text_color(type_color(ty, cx))
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(locus_core::redis::type_badge(ty).to_string())
}

pub fn error_box(text: &str, cx: &App) -> Div {
    let t = crate::shell::theme::tokens(cx);
    div()
        .flex_none()
        .mx(px(S6))
        .my(px(S4))
        .px(px(S4))
        .py(px(S2))
        .rounded(px(R_SM))
        .bg(t.red.opacity(0.12))
        .text_color(t.red_text)
        .text_size(px(12.))
        .whitespace_normal()
        .child(text.to_string())
}
