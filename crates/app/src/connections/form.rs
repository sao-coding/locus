//! Add / edit a saved connection. A wide form: database engines on the left;
//! a pasted URL that fills the fields; General / TLS / Appearance pages on the
//! right; Test connection on the left of the footer, Cancel / Save on the
//! right. The form is an entity shown inside a component `Dialog` and reports
//! what happened through `ConnFormEvent`.

use crate::shell::scale::*;
use crate::shell::seg::{SegItem, seg};
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use locus_core::store::{DbKind, SavedConnection, Tls, new_id, parse_url};
use rust_i18n::t;
use std::rc::Rc;

pub enum ConnFormEvent {
    /// Saved connection plus a new password (`None` = keep the stored one).
    Saved(SavedConnection, Option<String>),
    #[allow(dead_code)]
    Deleted(String),
    Cancelled,
    /// Add a connection that already exists (another Space) to this Space.
    UseExisting(String),
    /// Test these settings (password `None` = use the stored one).
    Test(SavedConnection, Option<String>),
}

enum TestState {
    Running,
    Ok(String),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    General,
    Ssh,
    Tls,
    Appearance,
}

/// Engines the form offers: kind, label, badge letter, badge hue.
pub const ENGINES: &[(DbKind, &str, &str, f32)] = &[
    (DbKind::Mysql, "MySQL / MariaDB", "M", 0.55),
    (DbKind::Postgres, "PostgreSQL", "P", 0.62),
    (DbKind::Oracle, "Oracle", "O", 0.02),
    (DbKind::Sqlite, "SQLite", "S", 0.58),
    (DbKind::Redis, "Redis", "R", 0.0),
];

/// Accent palette for connections (#rrggbb).
pub const CONN_COLORS: &[&str] = &[
    "#3b82f6", "#06b6d4", "#14b8a6", "#10b981", "#84cc16", "#f59e0b", "#f97316", "#ef4444",
    "#ec4899", "#a855f7", "#8b5cf6", "#64748b",
];

/// Icons a connection can use instead of its initials (component icon set).
pub const CONN_ICONS: &[(&str, IconName)] = &[
    ("drive", IconName::HardDrive),
    ("folder", IconName::Folder),
    ("globe", IconName::Globe),
    ("network", IconName::Network),
    ("building", IconName::Building2),
    ("cpu", IconName::Cpu),
    ("inbox", IconName::Inbox),
    ("dashboard", IconName::LayoutDashboard),
    ("terminal", IconName::SquareTerminal),
    ("book", IconName::BookOpen),
    ("bot", IconName::Bot),
    ("user", IconName::User),
    ("star", IconName::Star),
    ("heart", IconName::Heart),
    ("bell", IconName::Bell),
    ("calendar", IconName::Calendar),
    ("map", IconName::Map),
    ("palette", IconName::Palette),
    ("moon", IconName::Moon),
    ("sun", IconName::Sun),
    ("search", IconName::Search),
    ("settings", IconName::Settings),
    ("file", IconName::FileText),
    ("memory", IconName::MemoryStick),
];

pub fn conn_icon(name: &Option<String>) -> Option<IconName> {
    let n = name.as_deref()?;
    CONN_ICONS
        .iter()
        .find(|(k, _)| *k == n)
        .map(|(_, i)| i.clone())
}

/// `#rrggbb` -> colour.
pub fn parse_hex(s: &str) -> Option<Hsla> {
    let h = s.trim().strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(Hsla::from(rgb(v)))
}

pub fn to_hex(c: Hsla) -> String {
    let r = Rgba::from(c);
    let b = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(r.r), b(r.g), b(r.b))
}

pub struct ConnForm {
    editing: Option<SavedConnection>,
    has_password: bool,
    url: Entity<InputState>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    database: Entity<InputState>,
    icon_search: Entity<InputState>,
    /// Connections of other Spaces that this one can reuse (new-connection dialog only).
    pub existing: Vec<SavedConnection>,
    picker: Entity<ColorPickerState>,
    tls: Tls,
    kind: DbKind,
    env: locus_core::store::Env,
    read_only: bool,
    color: Option<String>,
    icon: Option<String>,
    page: Page,
    error: Option<String>,
    url_note: Option<String>,
    rt: tokio::runtime::Handle,
    test: Option<TestState>,
    _test_task: Option<Task<()>>,
    pub(super) ssh: super::ssh::SshForm,
    /// Set when the test stopped at an unknown SSH host key: (host, port) to trust.
    trust: Option<(String, u16)>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ConnFormEvent> for ConnForm {}

impl ConnForm {
    pub fn new(
        editing: Option<SavedConnection>,
        has_password: bool,
        rt: tokio::runtime::Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let e = editing.as_ref();
        let kind = e.map(|c| c.kind).unwrap_or_default();
        let mut input = |v: String| cx.new(|cx| InputState::new(window, cx).default_value(v));
        let name = input(e.map(|c| c.name.clone()).unwrap_or_default());
        let host = input(
            e.map(|c| c.host.clone())
                .unwrap_or_else(|| "127.0.0.1".into()),
        );
        let port = input(
            e.map(|c| c.port.to_string())
                .unwrap_or_else(|| kind.default_port().to_string()),
        );
        let user = input(e.map(|c| c.user.clone()).unwrap_or_default());
        let database = input(e.and_then(|c| c.database.clone()).unwrap_or_default());
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let url = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("conn.url_placeholder").to_string())
        });
        let icon_search = cx
            .new(|cx| InputState::new(window, cx).placeholder(t!("conn.icon_search").to_string()));
        let color = e.and_then(|c| c.color.clone());
        let picker = cx.new(|cx| {
            let s = ColorPickerState::new(window, cx);
            match color.as_deref().and_then(parse_hex) {
                Some(c) => s.default_value(c),
                None => s,
            }
        });
        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(
            &url,
            window,
            |this: &mut Self, url, ev: &InputEvent, window, cx| {
                if let InputEvent::Change = ev {
                    let text = url.read(cx).value().to_string();
                    this.apply_url(&text, window, cx);
                }
            },
        ));
        subs.push(
            cx.subscribe(&picker, |this: &mut Self, _, ev: &ColorPickerEvent, cx| {
                let ColorPickerEvent::Change(c) = ev;
                if let Some(c) = c {
                    this.color = Some(to_hex(*c));
                    cx.notify();
                }
            }),
        );
        subs.push(
            cx.subscribe(&icon_search, |_: &mut Self, _, _: &InputEvent, cx| {
                cx.notify()
            }),
        );
        for inp in [&name, &host, &port, &user] {
            subs.push(cx.subscribe(inp, |_: &mut Self, _, _: &InputEvent, cx| cx.notify()));
        }
        Self {
            tls: e.map(|c| c.tls).unwrap_or_default(),
            icon: e.and_then(|c| c.icon.clone()),
            editing: editing.clone(),
            has_password,
            url,
            name,
            host,
            port,
            user,
            password,
            database,
            icon_search,
            existing: Vec::new(),
            picker,
            kind,
            env: e.map(|c| c.env).unwrap_or_default(),
            read_only: e.is_some_and(|c| c.read_only),
            color,
            page: Page::General,
            error: None,
            url_note: None,
            rt,
            test: None,
            _test_task: None,
            ssh: super::ssh::SshForm::new(editing.as_ref(), window, cx),
            trust: None,
            _subs: subs,
        }
    }

    pub fn is_edit(&self) -> bool {
        self.editing.is_some()
    }

    pub fn first_focus(&self, cx: &App) -> FocusHandle {
        if self.is_edit() {
            self.name.read(cx).focus_handle(cx)
        } else {
            self.url.read(cx).focus_handle(cx)
        }
    }

    /// A pasted URL fills engine, host, port, user, password and database.
    pub fn apply_url(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if text.trim().is_empty() {
            self.url_note = None;
            cx.notify();
            return;
        }
        let Some((c, pw)) = parse_url(text) else {
            self.url_note = Some(t!("conn.url_invalid").into_owned());
            cx.notify();
            return;
        };
        self.set_kind(c.kind, window, cx);
        let set =
            |s: &Entity<InputState>, v: String, window: &mut Window, cx: &mut Context<Self>| {
                s.update(cx, |s, cx| s.set_value(v, window, cx))
            };
        if c.kind == DbKind::Sqlite {
            // Only the file path: host, user and password stay untouched.
            set(
                &self.database,
                c.database.clone().unwrap_or_default(),
                window,
                cx,
            );
            if self.name.read(cx).value().trim().is_empty() {
                set(&self.name, c.name.clone(), window, cx);
            }
            self.url_note = Some(t!("conn.url_filled").into_owned());
            cx.notify();
            return;
        }
        set(&self.host, c.host.clone(), window, cx);
        set(&self.port, c.port.to_string(), window, cx);
        set(&self.user, c.user.clone(), window, cx);
        set(
            &self.database,
            c.database.clone().unwrap_or_default(),
            window,
            cx,
        );
        if !pw.is_empty() {
            set(&self.password, pw, window, cx);
        }
        if self.name.read(cx).value().trim().is_empty() {
            let n = match &c.database {
                Some(db) => format!("{} · {}", c.host, db),
                None => c.host.clone(),
            };
            set(&self.name, n, window, cx);
        }
        self.url_note = Some(t!("conn.url_filled").into_owned());
        cx.notify();
    }

    pub fn set_kind(&mut self, new: DbKind, window: &mut Window, cx: &mut Context<Self>) {
        if new == self.kind {
            return;
        }
        // Keep a port the user typed; replace the previous engine's default.
        let cur = self.port.read(cx).value().to_string();
        if cur.is_empty() || cur == self.kind.default_port().to_string() {
            self.port.update(cx, |s, cx| {
                s.set_value(new.default_port().to_string(), window, cx)
            });
        }
        if new == DbKind::Oracle || new == DbKind::Sqlite || new == DbKind::Redis {
            self.tls = Tls::Disabled;
        }
        if new == DbKind::Sqlite && self.page == Page::Ssh {
            self.page = Page::General;
        }
        if new == DbKind::Sqlite && self.page == Page::Tls {
            self.page = Page::General;
        }
        self.kind = new;
        cx.notify();
    }

    /// Native file picker for an existing database file.
    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("sqlite.browse_prompt").to_string().into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(p) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| this.set_file(&p, window, cx));
        })
        .detach();
    }

    /// Native save dialog, then an empty database file is created there.
    fn new_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = std::env::var_os("USERPROFILE")
            .map(|h| std::path::PathBuf::from(h).join("Documents"))
            .unwrap_or_default();
        let rx = cx.prompt_for_new_path(&dir, Some("database.db"));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(p))) = rx.await else { return };
            let _ =
                this.update_in(
                    cx,
                    |this, window, cx| match locus_core::driver::create_sqlite_file(&p) {
                        Ok(()) => this.set_file(&p, window, cx),
                        Err(e) => this
                            .set_error(t!("sqlite.err_create", msg = e.message).into_owned(), cx),
                    },
                );
        })
        .detach();
    }

    fn set_file(&mut self, p: &std::path::Path, window: &mut Window, cx: &mut Context<Self>) {
        let path = p.to_string_lossy().into_owned();
        self.database
            .update(cx, |s, cx| s.set_value(path, window, cx));
        if self.name.read(cx).value().trim().is_empty() {
            let n = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.name.update(cx, |s, cx| s.set_value(n, window, cx));
        }
        self.error = None;
        cx.notify();
    }

    pub(crate) fn save(&mut self, cx: &mut Context<Self>) {
        if let Some((c, pw)) = self.collect(cx) {
            cx.emit(ConnFormEvent::Saved(c, pw));
        }
    }

    fn test(&mut self, cx: &mut Context<Self>) {
        if let Some((c, pw)) = self.collect(cx) {
            cx.emit(ConnFormEvent::Test(c, pw));
        }
    }

    /// Connects with `c`, runs `SELECT VERSION()`, and shows the result.
    pub fn run_test(
        &mut self,
        c: SavedConnection,
        password: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        let password = match password {
            Ok(p) => p,
            Err(e) => {
                self.test = Some(TestState::Failed(e));
                cx.notify();
                return;
            }
        };
        self.test = Some(TestState::Running);
        self.trust = None;
        let ssh_target = c.ssh.as_ref().map(|s| (s.host.clone(), s.port));
        let rt = self.rt.clone();
        if c.kind == DbKind::Redis {
            let (rt2, cfg) = (rt.clone(), locus_core::redis::config(&c, password.clone()));
            self._test_task = Some(cx.spawn(async move |this, cx| {
                let t0 = std::time::Instant::now();
                let res = crate::redis::test_connection(rt2, cfg).await;
                let ms = t0.elapsed();
                let _ = this.update(cx, |f, cx| {
                    f.test = Some(match res {
                        Ok(v) => TestState::Ok(
                            t!(
                                "conn.test_ok",
                                version = format!("Redis {v}"),
                                time = locus_core::fmt::duration(ms)
                            )
                            .into_owned(),
                        ),
                        Err(e) => TestState::Failed(e),
                    });
                    cx.notify();
                });
            }));
            cx.notify();
            return;
        }
        let t0 = std::time::Instant::now();
        let fut = locus_core::driver::AnyDriver::connect(&rt, &c, password, Default::default());
        self._test_task = Some(cx.spawn(async move |this, cx| {
            let res = match fut.await {
                Err(e) => Err(e),
                Ok(d) => {
                    use db_core::Driver as _;
                    let version_sql = if d.is_oracle() {
                        "SELECT VERSION FROM PRODUCT_COMPONENT_VERSION WHERE ROWNUM = 1"
                    } else if d.is_sqlite() {
                        "SELECT sqlite_version()"
                    } else {
                        "SELECT VERSION()"
                    };
                    let mut h = d.query(version_sql.into(), Default::default());
                    let mut version = String::new();
                    let mut err = None;
                    while let Some(ev) = h.next().await {
                        match ev {
                            db_core::QueryEvent::Batch(b) if b.rows() > 0 => {
                                if let db_core::Cell::Text(v) = b.cell(0, 0) {
                                    version = v.to_string();
                                }
                            }
                            db_core::QueryEvent::Finished(db_core::Outcome::Failed(e)) => {
                                err = Some(e)
                            }
                            _ => {}
                        }
                    }
                    // Keep the driver alive until the query is done, then close it.
                    drop(d);
                    match err {
                        Some(e) => Err(e),
                        None => Ok(version),
                    }
                }
            };
            let ms = t0.elapsed();
            let _ = this.update(cx, |f, cx| {
                f.test = Some(match res {
                    Ok(v) => TestState::Ok(
                        t!(
                            "conn.test_ok",
                            version = v,
                            time = locus_core::fmt::duration(ms)
                        )
                        .into_owned(),
                    ),
                    Err(e) => {
                        if e.code == Some(locus_core::tunnel::CODE_HOST_UNKNOWN) {
                            f.trust = ssh_target.clone();
                        }
                        let detail = match e.code {
                            Some(code) if code > -9000 => format!("[{code}] {}", e.message),
                            _ => e.message.clone(),
                        };
                        TestState::Failed(format!(
                            "{}: {detail}",
                            locus_core::status::error_title(e.kind)
                        ))
                    }
                });
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn collect(&mut self, cx: &mut Context<Self>) -> Option<(SavedConnection, Option<String>)> {
        let v = |s: &Entity<InputState>, cx: &App| s.read(cx).value().trim().to_string();
        let name = v(&self.name, cx);
        let host = v(&self.host, cx);
        let user = v(&self.user, cx);
        let db = v(&self.database, cx);
        let port: Option<u16> = v(&self.port, cx).parse().ok().filter(|p| *p > 0);
        let sqlite = self.kind == DbKind::Sqlite;
        self.error = if name.is_empty() {
            Some(t!("conn.err_name").into_owned())
        } else if sqlite && db.is_empty() {
            Some(t!("sqlite.err_file").into_owned())
        } else if self.kind == DbKind::Redis
            && !db.is_empty()
            && db.parse::<u8>().map_or(true, |d| d > 15)
        {
            Some(t!("redis.err_db").into_owned())
        } else if sqlite {
            None
        } else if host.is_empty() {
            Some(t!("conn.err_host").into_owned())
        } else if port.is_none() {
            Some(t!("conn.err_port").into_owned())
        } else if user.is_empty() && self.kind != DbKind::Redis {
            Some(t!("conn.err_user").into_owned())
        } else {
            None
        };
        if self.error.is_some() {
            self.page = Page::General;
            cx.notify();
            return None;
        }
        let ssh = if sqlite {
            None
        } else {
            match self.ssh.collect(cx) {
                Ok(s) => s,
                Err(e) => {
                    self.error = Some(e);
                    self.page = Page::Ssh;
                    cx.notify();
                    return None;
                }
            }
        };
        let pw = self.password.read(cx).value().to_string();
        // A SQLite file has no password: nothing is typed, nothing is stored.
        let password = if sqlite || (pw.is_empty() && self.has_password) {
            None
        } else {
            Some(pw)
        };
        let c = SavedConnection {
            id: self
                .editing
                .as_ref()
                .map(|c| c.id.clone())
                .unwrap_or_else(new_id),
            name,
            host: if sqlite { String::new() } else { host },
            port: if sqlite { 0 } else { port.unwrap() },
            user: if sqlite { String::new() } else { user },
            database: if self.kind == DbKind::Redis {
                Some(if db.is_empty() { "0".into() } else { db })
            } else {
                (!db.is_empty()).then_some(db)
            },
            tls: self.tls,
            kind: self.kind,
            color: self.color.clone(),
            icon: self.icon.clone(),
            env: self.env,
            read_only: self.kind == DbKind::Sqlite && self.read_only,
            ssh,
        };
        cx.notify();
        Some((c, password))
    }

    pub fn set_error(&mut self, msg: String, cx: &mut Context<Self>) {
        self.error = Some(msg);
        cx.notify();
    }
}

pub(super) fn field(cx: &App, label: String, input: impl IntoElement) -> Div {
    v_flex()
        .gap(px(S3))
        .min_w_0()
        .child(
            div()
                .text_size(px(TX_SM))
                .text_color(crate::shell::theme::tokens(cx).tx2)
                .child(label),
        )
        .child(input)
}

/// Badge for a connection: its icon or initials on its accent colour.
pub fn badge(name: &str, color: Hsla, icon: Option<IconName>, size: f32) -> Div {
    let initial: String = name
        .chars()
        .find(|c| !c.is_whitespace())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    div()
        .flex_none()
        .size(px(size))
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .font_semibold()
        .text_color(gpui_kit::white())
        .bg(color)
        .child(match icon {
            Some(i) => Icon::new(i)
                .small()
                .text_color(gpui_kit::white())
                .into_any_element(),
            None => initial.into_any_element(),
        })
}

impl ConnForm {
    fn accent(&self) -> Hsla {
        self.color
            .as_deref()
            .and_then(parse_hex)
            .unwrap_or(hsla(0.6, 0.6, 0.5, 1.0))
    }

    fn render_general(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let db_label = match self.kind {
            DbKind::Oracle => t!("conn.service"),
            DbKind::Postgres => t!("conn.database_pg"),
            DbKind::Mysql => t!("conn.database"),
            DbKind::Sqlite => t!("sqlite.file"),
            DbKind::Redis => t!("redis.db_index"),
        };
        if self.kind == DbKind::Sqlite {
            return v_flex()
                .gap(px(S6))
                .child(field(
                    cx,
                    t!("conn.name").into_owned(),
                    Input::new(&self.name).px(px(crate::shell::scale::S5)),
                ))
                .child(self.render_env(cx))
                .child(field(
                    cx,
                    t!("sqlite.file").into_owned(),
                    h_flex()
                        .gap(px(S3))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&self.database).px(px(crate::shell::scale::S5))),
                        )
                        .child(
                            Button::new("sqlite-browse")
                                .outline()
                                .icon(IconName::FolderOpen)
                                .label(t!("sqlite.browse").to_string())
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.browse(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("sqlite-new")
                                .outline()
                                .icon(IconName::Plus)
                                .label(t!("sqlite.new_file").to_string())
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.new_file(window, cx)),
                                ),
                        ),
                ))
                .child(
                    v_flex()
                        .gap(px(S3))
                        .child(
                            gpui_kit::component::checkbox::Checkbox::new("sqlite-ro")
                                .label(t!("sqlite.read_only").to_string())
                                .checked(self.read_only)
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.read_only = *checked;
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(t!("sqlite.read_only_note").to_string()),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t!("sqlite.note").to_string()),
                )
                .into_any_element();
        }
        v_flex()
            .gap(px(S6))
            .child(field(
                cx,
                t!("conn.name").into_owned(),
                Input::new(&self.name).px(px(crate::shell::scale::S5)),
            ))
            .child(self.render_env(cx))
            .child(
                h_flex()
                    .gap(px(S6))
                    .items_start()
                    .child(
                        field(
                            cx,
                            t!("conn.host").into_owned(),
                            Input::new(&self.host).px(px(crate::shell::scale::S5)),
                        )
                        .flex_1(),
                    )
                    .child(
                        field(
                            cx,
                            t!("conn.port").into_owned(),
                            Input::new(&self.port).px(px(crate::shell::scale::S5)),
                        )
                        .w(px(110.))
                        .flex_none(),
                    ),
            )
            .child(
                h_flex()
                    .gap(px(S6))
                    .items_start()
                    .child(
                        field(
                            cx,
                            t!("conn.user").into_owned(),
                            Input::new(&self.user).px(px(crate::shell::scale::S5)),
                        )
                        .flex_1(),
                    )
                    .child(
                        field(
                            cx,
                            t!("conn.password").into_owned(),
                            Input::new(&self.password)
                                .px(px(crate::shell::scale::S5))
                                .mask_toggle(),
                        )
                        .flex_1(),
                    ),
            )
            .child(
                div()
                    .mt_neg_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        if self.has_password {
                            t!("conn.password_note_keep")
                        } else {
                            t!("conn.password_note")
                        }
                        .to_string(),
                    ),
            )
            .child(field(
                cx,
                db_label.into_owned(),
                Input::new(&self.database).px(px(crate::shell::scale::S5)),
            ))
            .into_any_element()
    }

    fn render_tls(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let tls_ix = match self.tls {
            Tls::Required => 0,
            Tls::Unverified => 1,
            Tls::Disabled => 2,
        };
        let warning_text = match self.tls {
            Tls::Required => None,
            Tls::Unverified => Some(t!("conn.tls_unverified_warning")),
            Tls::Disabled => Some(t!("conn.tls_warning")),
        };
        let warning = warning_text.map(|text| {
            h_flex()
                .id("tls-warning")
                .gap_2()
                .items_start()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(theme.warning)
                .bg(theme.warning.opacity(0.12))
                .text_sm()
                .child(
                    Icon::new(IconName::TriangleAlert)
                        .text_color(theme.warning)
                        .flex_shrink_0()
                        .mt_0p5(),
                )
                .child(div().flex_1().min_w_0().child(text.to_string()))
        });
        v_flex()
            .gap(px(S6))
            .child(field(
                cx,
                t!("conn.tls").into_owned(),
                RadioGroup::vertical("tls")
                    .selected_index(Some(tls_ix))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.tls = match *ix {
                            0 => Tls::Required,
                            1 => Tls::Unverified,
                            _ => Tls::Disabled,
                        };
                        cx.notify();
                    }))
                    .child(Radio::new("tls-required").label(t!("conn.tls_required").to_string()))
                    .child(
                        Radio::new("tls-unverified").label(t!("conn.tls_unverified").to_string()),
                    )
                    .child(Radio::new("tls-disabled").label(t!("conn.tls_disabled").to_string())),
            ))
            .children(warning)
            .when(self.kind == DbKind::Oracle, |v| {
                v.child(
                    div()
                        .text_size(px(TX_SM))
                        .text_color(crate::shell::theme::tokens(cx).tx3)
                        .child(t!("conn.tls_oracle_note").to_string()),
                )
            })
            .into_any_element()
    }

    fn render_appearance(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let accent = self.accent();
        let swatches = self.color_swatches(cx);
        let q = self.icon_search.read(cx).value().to_lowercase();
        let mut icons = h_flex().gap_1().flex_wrap();
        let mut none = div()
            .id("conn-icon-none")
            .size(px(30.))
            .rounded_md()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .text_xs()
            .hover(|s| s.bg(theme.muted))
            .tooltip(|w, cx| {
                gpui_kit::component::tooltip::Tooltip::new(t!("conn.icon_initials").to_string())
                    .build(w, cx)
            })
            .child("Aa")
            .on_click(cx.listener(|this, _, _, cx| {
                this.icon = None;
                cx.notify();
            }));
        if self.icon.is_none() {
            none = none.bg(theme.muted).border_1().border_color(accent);
        }
        icons = icons.child(none);
        for (i, (key, icon)) in CONN_ICONS
            .iter()
            .enumerate()
            .filter(|(_, (k, _))| q.is_empty() || k.contains(&q))
        {
            let selected = self.icon.as_deref() == Some(*key);
            let k = key.to_string();
            icons = icons.child(
                div()
                    .id(("conn-icon", i))
                    .size(px(30.))
                    .rounded_md()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.muted))
                    .when(selected, |d| {
                        d.bg(theme.muted).border_1().border_color(accent)
                    })
                    .tooltip({
                        let k = k.clone();
                        move |w, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(k.clone()).build(w, cx)
                        }
                    })
                    .child(Icon::new(icon.clone()).small())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.icon = Some(k.clone());
                        cx.notify();
                    })),
            );
        }
        // Live preview: how the connection will look in the sidebar.
        let name = self.name.read(cx).value().to_string();
        let name = if name.trim().is_empty() {
            t!("conn.preview_name").into_owned()
        } else {
            name
        };
        let sub = format!(
            "{}@{}:{}",
            self.user.read(cx).value(),
            self.host.read(cx).value(),
            self.port.read(cx).value()
        );
        let preview = h_flex()
            .gap_2()
            .p_2()
            .rounded_lg()
            .bg(crate::shell::theme::tokens(cx).hv)
            .border_1()
            .border_color(theme.border)
            .child(badge(&name, accent, conn_icon(&self.icon), 28.0))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().font_medium().truncate().child(name))
                    .child(
                        div()
                            .text_size(px(TX_SM))
                            .text_color(crate::shell::theme::tokens(cx).tx3)
                            .truncate()
                            .child(sub),
                    ),
            );
        v_flex()
            .gap(px(S6))
            .child(field(cx, t!("conn.color").into_owned(), swatches))
            .child(field(
                cx,
                t!("conn.icon").into_owned(),
                v_flex()
                    .gap_2()
                    .child(
                        Input::new(&self.icon_search)
                            .px(px(crate::shell::scale::S5))
                            .cleanable(true),
                    )
                    .child(icons),
            ))
            .child(field(cx, t!("conn.preview").into_owned(), preview))
            .into_any_element()
    }

    /// Colour swatches of the Appearance page and the custom colour picker.
    fn color_swatches(&self, cx: &mut Context<Self>) -> Div {
        let mut swatches = h_flex().gap(px(S3)).flex_wrap().items_center();
        for (i, hex) in CONN_COLORS.iter().enumerate() {
            let c = parse_hex(hex).unwrap();
            let selected = self.color.as_deref() == Some(*hex);
            let h = hex.to_string();
            swatches = swatches.child(
                div()
                    .id(("conn-color", i))
                    .size(px(H_ROW))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .border_2()
                    .border_color(if selected {
                        crate::shell::theme::tokens(cx).tx
                    } else {
                        hsla(0., 0., 0., 0.)
                    })
                    .child(
                        div()
                            .size(px(S7 - S1))
                            .rounded_full()
                            .bg(c)
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(selected, |d| {
                                d.child(
                                    crate::shell::icons::icon("check")
                                        .size(px(13.))
                                        .text_color(gpui_kit::white()),
                                )
                            }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.color = Some(h.clone());
                        cx.notify();
                    })),
            );
        }
        swatches = swatches.child(
            ColorPicker::new(&self.picker)
                .small()
                .label(t!("conn.custom_color").to_string()),
        );
        swatches
    }
}

impl Render for ConnForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let left = self.render_left(cx);
        let right = self.render_right(cx);

        let error = self
            .error
            .clone()
            .map(|e| div().text_sm().text_color(theme.danger).child(e));
        let test = self.render_test_result(cx);
        let footer = self.render_footer(cx);

        v_flex()
            .id("conn-form")
            .gap(px(S6))
            .child(h_flex().gap_4().items_start().child(left).child(right))
            .children(error)
            .children(test)
            .children(self.trust.clone().map(|(host, port)| {
                Button::new("ssh-trust")
                    .outline()
                    .label(t!("ssh.trust", host = host.clone()).to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let path =
                            locus_core::sshcfg::default_known_hosts_path().unwrap_or_default();
                        match locus_core::tunnel::trust_pending(&path, &host, port) {
                            Ok(_) => {
                                this.trust = None;
                                this.test(cx);
                            }
                            Err(e) => this.test = Some(TestState::Failed(e)),
                        }
                        cx.notify();
                    }))
            }))
            .child(footer)
    }
}

impl ConnForm {
    /// Left column: engine list and the existing connections to reuse.
    fn render_left(&self, cx: &mut Context<Self>) -> Div {
        // Left: engines.
        let mut engines = v_flex()
            .w(px(184.))
            .flex_none()
            .gap_1()
            .p_2()
            .rounded_lg()
            .bg(crate::shell::theme::tokens(cx).hv);
        engines = engines.child(
            div()
                .px_2()
                .pb_1()
                .text_size(px(TX_XS))
                .font_semibold()
                .text_color(crate::shell::theme::tokens(cx).tx3)
                .child(t!("conn.engine").to_uppercase()),
        );
        for (i, (kind, label, letter, hue)) in ENGINES.iter().enumerate() {
            let selected = self.kind == *kind;
            let k = *kind;
            engines = engines.child(
                h_flex()
                    .id(("engine", i))
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .items_center()
                    .when(selected, |d| d.bg(crate::shell::theme::tokens(cx).on))
                    .when(!selected, |d| {
                        d.hover(|s| s.bg(crate::shell::theme::tokens(cx).hv))
                    })
                    .child(
                        div()
                            .size(px(24.))
                            .rounded_md()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .font_semibold()
                            .text_color(gpui_kit::white())
                            .bg(hsla(*hue, 0.6, 0.45, 1.0))
                            .child(*letter),
                    )
                    .child(div().text_sm().truncate().child(*label))
                    .on_click(cx.listener(move |this, _, window, cx| this.set_kind(k, window, cx))),
            );
        }
        let t3 = crate::shell::theme::tokens(cx);
        let mut left = v_flex().w(px(200.)).flex_none().gap(px(S5)).child(engines);
        if !self.existing.is_empty() {
            let mut list = v_flex()
                .id("existing-conns")
                .max_h(px(180.))
                .overflow_y_scroll()
                .gap(px(S1));
            for (i, c) in self.existing.iter().enumerate() {
                let id = c.id.clone();
                list = list.child(
                    h_flex()
                        .id(("existing", i))
                        .h(px(H_ROW))
                        .px(px(S4))
                        .gap(px(S4))
                        .items_center()
                        .rounded(px(R_MD))
                        .cursor_pointer()
                        .hover(|s| s.bg(t3.hv))
                        .child(
                            crate::shell::icons::icon("db")
                                .size(px(14.))
                                .text_color(t3.tx3),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(TX_BASE))
                                .child(c.name.clone()),
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(ConnFormEvent::UseExisting(id.clone()))
                        })),
                );
            }
            left = left.child(
                v_flex()
                    .gap(px(S2))
                    .child(
                        div()
                            .px(px(S4))
                            .text_size(px(TX_XS))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t3.tx3)
                            .child(t!("t4.existing").to_string().to_uppercase()),
                    )
                    .child(list),
            );
        }
        left
    }

    /// Right column: URL field, page tabs and the page in front.
    fn render_right(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let tab = |id: &'static str, label: String, page: Page, cx: &mut Context<Self>| {
            Button::new(id)
                .ghost()
                .selected(self.page == page)
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.page = page;
                    cx.notify();
                }))
        };
        let tabs = h_flex()
            .gap_1()
            .pb_2()
            .border_b_1()
            .border_color(theme.border)
            .child(tab(
                "page-general",
                t!("conn.page_general").into_owned(),
                Page::General,
                cx,
            ))
            .when(self.kind != DbKind::Sqlite, |v| {
                v.child(tab("page-ssh", t!("ssh.page").into_owned(), Page::Ssh, cx))
            })
            .when(self.kind != DbKind::Sqlite, |v| {
                v.child(tab(
                    "page-tls",
                    t!("conn.page_tls").into_owned(),
                    Page::Tls,
                    cx,
                ))
            })
            .child(tab(
                "page-appearance",
                t!("conn.page_appearance").into_owned(),
                Page::Appearance,
                cx,
            ));
        let page = match self.page {
            Page::General => self.render_general(cx),
            Page::Ssh => self.ssh.render(cx),
            Page::Tls => self.render_tls(cx),
            Page::Appearance => self.render_appearance(cx),
        };
        let url = v_flex()
            .gap(px(S3))
            .child(
                Input::new(&self.url)
                    .px(px(crate::shell::scale::S5))
                    .prefix(
                        Icon::new(IconName::ExternalLink)
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .cleanable(true),
            )
            .when_some(self.url_note.clone(), |v, n| {
                v.child(
                    div()
                        .text_size(px(TX_SM))
                        .text_color(crate::shell::theme::tokens(cx).tx3)
                        .child(n),
                )
            });
        v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(S5))
            .child(url)
            .child(tabs)
            .child(div().id("conn-page").min_h(px(320.)).child(page))
    }

    /// Result of "Test": spinner, tick or cross with its message.
    fn render_test_result(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let theme = cx.theme().clone();
        self.test.as_ref().map(|s| {
            let (icon, text): (AnyElement, String) = match s {
                TestState::Running => (
                    gpui_kit::component::spinner::Spinner::new()
                        .small()
                        .color(theme.muted_foreground)
                        .into_any_element(),
                    t!("conn.testing").into_owned(),
                ),
                TestState::Ok(m) => (
                    Icon::new(IconName::CircleCheck)
                        .text_color(theme.success)
                        .into_any_element(),
                    m.clone(),
                ),
                TestState::Failed(m) => (
                    Icon::new(IconName::CircleX)
                        .text_color(theme.danger)
                        .into_any_element(),
                    m.clone(),
                ),
            };
            h_flex()
                .id("test-result")
                .gap_2()
                .items_start()
                .text_sm()
                .child(div().flex_none().mt_0p5().child(icon))
                .child(div().flex_1().min_w_0().whitespace_normal().child(text))
        })
    }

    /// Test / Cancel / Save.
    fn render_footer(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        h_flex()
            .gap_2()
            .pt(px(S5))
            .items_center()
            .border_t_1()
            .border_color(theme.border)
            .child(
                Button::new("test")
                    .outline()
                    .icon(IconName::RefreshCw)
                    .label(t!("conn.test").to_string())
                    .loading(matches!(self.test, Some(TestState::Running)))
                    .disabled(matches!(self.test, Some(TestState::Running)))
                    .on_click(cx.listener(|this, _, _, cx| this.test(cx))),
            )
            .child(div().flex_1())
            .child(
                Button::new("cancel")
                    .ghost()
                    .label(t!("common.cancel").to_string())
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ConnFormEvent::Cancelled))),
            )
            .child(
                Button::new("save")
                    .primary()
                    .label(t!("common.save").to_string())
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            )
    }
}

impl ConnForm {
    /// Environment: five-way segmented control, with a note for Production.
    fn render_env(&self, cx: &mut Context<Self>) -> AnyElement {
        use locus_core::store::Env;
        let t = crate::shell::theme::tokens(cx);
        let mut items = Vec::new();
        for (i, (env, key)) in [
            (Env::Local, "prod.env_local"),
            (Env::Dev, "prod.env_dev"),
            (Env::Staging, "prod.env_staging"),
            (Env::Team, "prod.env_team"),
            (Env::Prod, "prod.env_prod"),
        ]
        .into_iter()
        .enumerate()
        {
            let l = cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.env = env;
                cx.notify();
            });
            let mut it = SegItem::new(
                SharedString::from(format!("conn-env-{i}")),
                t!(key).to_string(),
                self.env == env,
                Rc::new(move |e, w, cx| l(e, w, cx)),
            );
            if env == Env::Prod {
                it = it.tone(t.red_text);
            }
            items.push(it);
        }
        let desc = match self.env {
            Env::Local => "t4.env_desc_local",
            Env::Dev => "t4.env_desc_dev",
            Env::Staging => "t4.env_desc_staging",
            Env::Team => "t4.env_desc_team",
            Env::Prod => "t4.env_desc_prod",
        };
        let prod = self.env == Env::Prod;
        v_flex()
            .gap(px(S3))
            .child(
                div()
                    .text_size(px(TX_SM))
                    .text_color(t.tx2)
                    .child(t!("prod.env").into_owned()),
            )
            .child(
                div()
                    .text_size(px(TX_SM))
                    .text_color(t.tx3)
                    .whitespace_normal()
                    .child(t!("t4.env_help").to_string()),
            )
            .child(seg(items, cx))
            .child(
                div()
                    .p(px(S4))
                    .rounded(px(R_MD))
                    .bg(if prod { t.red.opacity(0.10) } else { t.hv })
                    .text_size(px(TX_SM))
                    .text_color(if prod { t.red_text } else { t.tx2 })
                    .whitespace_normal()
                    .child(t!(desc).to_string()),
            )
            .into_any_element()
    }
}
