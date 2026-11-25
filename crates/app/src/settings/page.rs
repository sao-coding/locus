//! The Settings tab ("Strata v3" settings page): a search box and categories
//! on the left, sections of rows "label / description / control" on the right.
//! Only settings that exist in `locus_core::settings` are listed, plus a read-only
//! Shortcuts category generated from the real bindings (`shortcuts::listing`).
//! Every change is saved and applied at once.
use crate::shell::scale::*;
use crate::shell::seg::{SegItem, seg};
use crate::shell::*;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::switch::Switch;
use std::rc::Rc;

pub(crate) enum SettingsPageEvent {
    Language(&'static str),
    /// A setting was saved: re-apply theme, material, motion.
    Changed,
    /// "Restart and update" in About.
    RestartToUpdate,
}

impl EventEmitter<SettingsPageEvent> for SettingsPage {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cat {
    General,
    Appearance,
    Editor,
    Queries,
    Production,
    Shortcuts,
    /// Logo, name, version and licence. Manual check: Ctrl+, then click "About" in the
    /// left list (last entry); the harness cannot click, so `about_is_in_the_nav` guards it.
    About,
}

const CATS: &[(Cat, &str, &str)] = &[
    (Cat::General, "sp.cat_general", "gear"),
    (Cat::Appearance, "sp.cat_appearance", "panel-right"),
    (Cat::Editor, "sp.cat_editor", "query"),
    (Cat::Queries, "sp.cat_queries", "table"),
    (Cat::Production, "sp.cat_production", "lock"),
    (Cat::Shortcuts, "sp.cat_shortcuts", "query"),
    (Cat::About, "sp.cat_about", "info"),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ctl {
    Language,
    Theme,
    FontSize,
    Motion,
    Material,
    Translucency,
    SpaceTint,
    GridStripes,
    RowLimit,
    RowsCap,
    PageIdle,
    Count,
    AutoPlan,
    ConfirmNonprod,
    FormatCase,
    FormatIndent,
    ProdManualTx,
    TxIdle,
    /// Shown in About, not through `DEFS`.
    UpdateCheck,
    UpdateDownload,
    UpdateBeta,
    ParamEnabled,
    ParamColon,
    ParamAt,
    ParamDollar,
    ParamAnon,
}

impl Ctl {
    /// On/off settings, drawn as a switch (their `choices` are [on, off]); the others keep
    /// the segmented control.
    fn is_toggle(self) -> bool {
        matches!(
            self,
            Ctl::Motion
                | Ctl::GridStripes
                | Ctl::Count
                | Ctl::AutoPlan
                | Ctl::ConfirmNonprod
                | Ctl::ProdManualTx
                | Ctl::UpdateCheck
                | Ctl::UpdateDownload
                | Ctl::UpdateBeta
                | Ctl::ParamEnabled
                | Ctl::ParamColon
                | Ctl::ParamAt
                | Ctl::ParamDollar
                | Ctl::ParamAnon
        )
    }
}

/// (category, label key, description key, control)
const DEFS: &[(Cat, &str, &str, Ctl)] = &[
    (Cat::General, "lang.label", "sp.d_language", Ctl::Language),
    (Cat::General, "settings.theme", "sp.d_theme", Ctl::Theme),
    (
        Cat::General,
        "settings.font_size",
        "sp.d_font",
        Ctl::FontSize,
    ),
    (Cat::General, "settings.motion", "sp.d_motion", Ctl::Motion),
    (
        Cat::Appearance,
        "settings.material",
        "sp.d_material",
        Ctl::Material,
    ),
    (
        Cat::Appearance,
        "t0.translucency",
        "t3.d_translucency",
        Ctl::Translucency,
    ),
    (
        Cat::Appearance,
        "shell.space_tint",
        "shell.space_tint_note",
        Ctl::SpaceTint,
    ),
    (
        Cat::Appearance,
        "settings.stripes",
        "sp.d_stripes",
        Ctl::GridStripes,
    ),
    (
        Cat::Editor,
        "format.setting_keywords",
        "sp.d_fmt_case",
        Ctl::FormatCase,
    ),
    (
        Cat::Editor,
        "format.setting_indent",
        "sp.d_fmt_indent",
        Ctl::FormatIndent,
    ),
    (
        Cat::Queries,
        "settings.row_limit",
        "settings.row_limit_note",
        Ctl::RowLimit,
    ),
    (Cat::Queries, "settings.rows_cap", "sp.d_cap", Ctl::RowsCap),
    (
        Cat::Queries,
        "settings.page_idle",
        "sp.d_idle",
        Ctl::PageIdle,
    ),
    (Cat::Queries, "settings.count", "sp.d_count", Ctl::Count),
    (
        Cat::Queries,
        "sp.param_enabled",
        "sp.d_param_enabled",
        Ctl::ParamEnabled,
    ),
    (
        Cat::Queries,
        "sp.param_colon",
        "sp.d_param_colon",
        Ctl::ParamColon,
    ),
    (Cat::Queries, "sp.param_at", "sp.d_param_at", Ctl::ParamAt),
    (
        Cat::Queries,
        "sp.param_dollar",
        "sp.d_param_dollar",
        Ctl::ParamDollar,
    ),
    (
        Cat::Queries,
        "sp.param_anon",
        "sp.d_param_anon",
        Ctl::ParamAnon,
    ),
    (
        Cat::Queries,
        "settings.auto_plan",
        "sp.d_auto_plan",
        Ctl::AutoPlan,
    ),
    (
        Cat::Production,
        "prod.setting_confirm",
        "prod.setting_note",
        Ctl::ConfirmNonprod,
    ),
    (
        Cat::Production,
        "txn.setting_prod_manual",
        "txn.setting_prod_manual_note",
        Ctl::ProdManualTx,
    ),
    (
        Cat::Queries,
        "txn.setting_idle",
        "txn.setting_idle_note",
        Ctl::TxIdle,
    ),
];

/// Every word of `query` appears in `hay` (case-insensitive). Empty matches all.
pub(crate) fn matches_query(hay: &str, query: &str) -> bool {
    let hay = hay.to_lowercase();
    query
        .split_whitespace()
        .all(|w| hay.contains(&w.to_lowercase()))
}

type Apply = Rc<dyn Fn(&mut Settings)>;

struct Choice {
    id: String,
    label: String,
    on: bool,
    lang: Option<&'static str>,
    apply: Apply,
}

pub(crate) struct SettingsPage {
    s: Settings,
    search: Entity<InputState>,
    cat: Cat,
    query: String,
    scroll: ScrollHandle,
    /// Page focus target: the search box is not focused when the tab opens (audit S2).
    focus: FocusHandle,
    _sub: Subscription,
    /// Re-renders About when the update check changes state.
    _upd: Option<Subscription>,
}

impl SettingsPage {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("sp.search").to_string()));
        let sub = cx.subscribe(&search, |this: &mut Self, input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.query = input.read(cx).value().to_string();
                cx.notify();
            }
        });
        Self {
            s: crate::settings::get(cx).clone(),
            search,
            cat: Cat::General,
            query: String::new(),
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            _sub: sub,
            _upd: crate::update::entity(cx).map(|u| cx.observe(&u, |_, _, cx| cx.notify())),
        }
    }

    /// Switches to `cat` (clears the search).
    pub(crate) fn show_cat(&mut self, cat: Cat, window: &mut Window, cx: &mut Context<Self>) {
        self.cat = cat;
        self.query.clear();
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    pub fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }

    fn choice(id: String, label: String, on: bool, f: impl Fn(&mut Settings) + 'static) -> Choice {
        Choice {
            id,
            label,
            on,
            lang: None,
            apply: Rc::new(f),
        }
    }

    /// The [on, off] pair of a switch whose setting stores a plain bool.
    fn on_off(id: &str, on: bool, set: fn(&mut Settings, bool)) -> Vec<Choice> {
        vec![
            Self::choice(
                format!("{id}-on"),
                t!("settings.count_on").into_owned(),
                on,
                move |s| set(s, true),
            ),
            Self::choice(
                format!("{id}-off"),
                t!("settings.count_off").into_owned(),
                !on,
                move |s| set(s, false),
            ),
        ]
    }

    fn choices(&self, ctl: Ctl) -> Vec<Choice> {
        let s = &self.s;
        macro_rules! c {
            ($id:expr, $label:expr, $on:expr, $f:expr) => {
                Self::choice($id, $label, $on, $f)
            };
        }
        match ctl {
            Ctl::Language => {
                let cur = rust_i18n::locale().to_string();
                locus_core::settings::LANGUAGES
                    .iter()
                    .copied()
                    .map(|code| Choice {
                        id: format!("lang-{code}"),
                        label: {
                            let k = format!("lang.{code}");
                            t!(&k).into_owned()
                        },
                        on: cur == code,
                        lang: Some(code),
                        apply: Rc::new(|_| {}),
                    })
                    .collect()
            }
            Ctl::Theme => {
                let cur = s.theme.clone().unwrap_or_else(|| "system".into());
                [
                    ("system", "settings.theme_system"),
                    ("light", "settings.theme_light"),
                    ("dark", "settings.theme_dark"),
                ]
                .into_iter()
                .map(|(v, k)| {
                    c!(
                        format!("theme-{v}"),
                        t!(k).into_owned(),
                        cur == v,
                        move |s: &mut Settings| s.theme = Some(v.into())
                    )
                })
                .collect()
            }
            Ctl::FontSize => {
                let cur = s.font_size.unwrap_or(crate::shell::theme::UI_FONT_SIZE);
                [12.0f32, 13.0, 14.0, 15.0, 16.0, 18.0]
                    .into_iter()
                    .map(|f| {
                        c!(
                            format!("font-{f}"),
                            format!("{f}"),
                            (cur - f).abs() < 0.1,
                            move |s: &mut Settings| s.font_size = Some(f)
                        )
                    })
                    .collect()
            }
            Ctl::Motion => {
                let rm = s.reduce_motion;
                vec![
                    c!(
                        "motion-on".into(),
                        t!("settings.motion_on").into_owned(),
                        !rm,
                        |s: &mut Settings| s.reduce_motion = false
                    ),
                    c!(
                        "motion-off".into(),
                        t!("settings.motion_off").into_owned(),
                        rm,
                        |s: &mut Settings| s.reduce_motion = true
                    ),
                ]
            }
            Ctl::GridStripes => {
                let on = s.grid_stripes();
                vec![
                    c!(
                        "stripes-on".into(),
                        t!("settings.count_on").into_owned(),
                        on,
                        |s: &mut Settings| s.grid_stripes = Some(true)
                    ),
                    c!(
                        "stripes-off".into(),
                        t!("settings.count_off").into_owned(),
                        !on,
                        |s: &mut Settings| s.grid_stripes = Some(false)
                    ),
                ]
            }
            Ctl::Material => {
                let cur = s.material();
                [
                    ("mica", "t0.mat_mica"),
                    ("blur", "settings.mat_blur"),
                    ("mica_alt", "settings.mat_mica_alt"),
                    ("opaque", "settings.mat_opaque"),
                ]
                .into_iter()
                .map(|(v, k)| {
                    c!(
                        format!("mat-{v}"),
                        t!(k).into_owned(),
                        cur == v,
                        move |s: &mut Settings| s.material = Some(v.into())
                    )
                })
                .collect()
            }
            Ctl::Translucency => {
                let cur = s.translucency_value();
                locus_core::settings::TRANSLUCENCY_CHOICES
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| {
                        c!(
                            format!("tr-{i}"),
                            if i == 0 {
                                t!("t0.tr_default").into_owned()
                            } else {
                                format!("+{}%", (v * 100.0).round())
                            },
                            (cur - v).abs() < 0.01,
                            move |s: &mut Settings| s.translucency = Some(v)
                        )
                    })
                    .collect()
            }
            Ctl::SpaceTint => {
                let cur = s.space_tint();
                [0.0f32, 0.5, 1.0, 1.5, 2.0]
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| {
                        c!(
                            format!("st-{i}"),
                            format!("{v}"),
                            (cur - v).abs() < 0.01,
                            move |s: &mut Settings| s.space_tint = Some(v)
                        )
                    })
                    .collect()
            }
            Ctl::RowLimit => {
                let cur = s.row_limit.unwrap_or(locus_core::spaces::DEFAULT_ROW_LIMIT);
                [1_000u64, 10_000, 100_000, 1_000_000, 0]
                    .into_iter()
                    .map(|n| {
                        let label = if n == 0 {
                            t!("limit.unlimited").into_owned()
                        } else {
                            locus_core::fmt::count(n)
                        };
                        c!(
                            format!("limit-{n}"),
                            label,
                            cur == n,
                            move |s: &mut Settings| s.row_limit = Some(n)
                        )
                    })
                    .collect()
            }
            Ctl::RowsCap => {
                let cur = s.max_rows.unwrap_or(1_000_000);
                [100_000u64, 1_000_000, 5_000_000, 0]
                    .into_iter()
                    .map(|n| {
                        let label = if n == 0 {
                            t!("settings.no_cap").into_owned()
                        } else {
                            locus_core::fmt::count(n)
                        };
                        c!(
                            format!("cap-{n}"),
                            label,
                            cur == n,
                            move |s: &mut Settings| s.max_rows = Some(n)
                        )
                    })
                    .collect()
            }
            Ctl::PageIdle => {
                let cur = s.page_idle_minutes.unwrap_or(10);
                [2u64, 10, 30, 60]
                    .into_iter()
                    .map(|m| {
                        c!(
                            format!("idle-{m}"),
                            t!("settings.minutes", n = m.to_string()).into_owned(),
                            cur == m,
                            move |s: &mut Settings| s.page_idle_minutes = Some(m)
                        )
                    })
                    .collect()
            }
            Ctl::UpdateCheck => {
                let on = s.update_check();
                vec![
                    c!(
                        "upd-on".into(),
                        t!("settings.count_on").into_owned(),
                        on,
                        |s: &mut Settings| s.update_check = None
                    ),
                    c!(
                        "upd-off".into(),
                        t!("settings.count_off").into_owned(),
                        !on,
                        |s: &mut Settings| s.update_check = Some(false)
                    ),
                ]
            }
            Ctl::UpdateDownload => {
                let on = s.update_download();
                vec![
                    c!(
                        "upd-dl-on".into(),
                        t!("settings.count_on").into_owned(),
                        on,
                        |s: &mut Settings| s.update_download = None
                    ),
                    c!(
                        "upd-dl-off".into(),
                        t!("settings.count_off").into_owned(),
                        !on,
                        |s: &mut Settings| s.update_download = Some(false)
                    ),
                ]
            }
            // Stored either way: the default depends on the build (on for an RC).
            Ctl::UpdateBeta => {
                let on = s.update_beta();
                vec![
                    c!(
                        "upd-beta-on".into(),
                        t!("settings.count_on").into_owned(),
                        on,
                        |s: &mut Settings| s.update_beta = Some(true)
                    ),
                    c!(
                        "upd-beta-off".into(),
                        t!("settings.count_off").into_owned(),
                        !on,
                        |s: &mut Settings| s.update_beta = Some(false)
                    ),
                ]
            }
            Ctl::ParamEnabled => Self::on_off("pe", s.param_opts().enabled, |s, v| {
                s.param_enabled = Some(v)
            }),
            Ctl::ParamColon => {
                Self::on_off("pc", s.param_opts().colon, |s, v| s.param_colon = Some(v))
            }
            Ctl::ParamAt => Self::on_off("pa", s.param_opts().at, |s, v| s.param_at = Some(v)),
            Ctl::ParamDollar => {
                Self::on_off("pd", s.param_opts().dollar, |s, v| s.param_dollar = Some(v))
            }
            Ctl::ParamAnon => {
                Self::on_off("pq", s.param_opts().anon, |s, v| s.param_anon = Some(v))
            }
            Ctl::Count => {
                let nc = s.no_count;
                vec![
                    c!(
                        "count-on".into(),
                        t!("settings.count_on").into_owned(),
                        !nc,
                        |s: &mut Settings| s.no_count = false
                    ),
                    c!(
                        "count-off".into(),
                        t!("settings.count_off").into_owned(),
                        nc,
                        |s: &mut Settings| s.no_count = true
                    ),
                ]
            }
            Ctl::AutoPlan => {
                let off = s.no_auto_plan;
                vec![
                    c!(
                        "plan-on".into(),
                        t!("settings.count_on").into_owned(),
                        !off,
                        |s: &mut Settings| s.no_auto_plan = false
                    ),
                    c!(
                        "plan-off".into(),
                        t!("settings.count_off").into_owned(),
                        off,
                        |s: &mut Settings| s.no_auto_plan = true
                    ),
                ]
            }
            Ctl::ConfirmNonprod => {
                let cn = s.confirm_nonprod();
                vec![
                    c!(
                        "cn-on".into(),
                        t!("prod.on").into_owned(),
                        cn,
                        |s: &mut Settings| s.confirm_nonprod_writes = Some(true)
                    ),
                    c!(
                        "cn-off".into(),
                        t!("prod.off").into_owned(),
                        !cn,
                        |s: &mut Settings| s.confirm_nonprod_writes = Some(false)
                    ),
                ]
            }
            Ctl::ProdManualTx => {
                let on = s.prod_manual_tx();
                vec![
                    c!(
                        "pmt-on".into(),
                        t!("prod.on").into_owned(),
                        on,
                        |s: &mut Settings| s.prod_manual_tx = Some(true)
                    ),
                    c!(
                        "pmt-off".into(),
                        t!("prod.off").into_owned(),
                        !on,
                        |s: &mut Settings| s.prod_manual_tx = Some(false)
                    ),
                ]
            }
            Ctl::TxIdle => {
                let cur = s.tx_idle_minutes.unwrap_or(10);
                [0u64, 5, 10, 30]
                    .into_iter()
                    .map(|m| {
                        let label = if m == 0 {
                            t!("txn.idle_never").into_owned()
                        } else {
                            t!("settings.minutes", n = m.to_string()).into_owned()
                        };
                        c!(
                            format!("txidle-{m}"),
                            label,
                            cur == m,
                            move |s: &mut Settings| s.tx_idle_minutes = Some(m)
                        )
                    })
                    .collect()
            }
            Ctl::FormatCase => {
                let up = s.format_options().uppercase_keywords;
                vec![
                    c!(
                        "fmt-upper".into(),
                        t!("format.upper").into_owned(),
                        up,
                        |s: &mut Settings| s.format_uppercase = Some(true)
                    ),
                    c!(
                        "fmt-asis".into(),
                        t!("format.as_typed").into_owned(),
                        !up,
                        |s: &mut Settings| s.format_uppercase = Some(false)
                    ),
                ]
            }
            Ctl::FormatIndent => {
                let n = s.format_options().indent;
                vec![
                    c!("fmt-i2".into(), "2".into(), n == 2, |s: &mut Settings| s
                        .format_indent =
                        Some(2)),
                    c!("fmt-i4".into(), "4".into(), n == 4, |s: &mut Settings| s
                        .format_indent =
                        Some(4)),
                ]
            }
        }
    }

    fn cat_name(c: Cat) -> String {
        let k = CATS
            .iter()
            .find(|(x, _, _)| *x == c)
            .map(|(_, k, _)| *k)
            .unwrap_or("");
        t!(k).into_owned()
    }

    /// One row "label / description / control".
    fn row(
        &self,
        id: usize,
        label: String,
        desc: String,
        control: AnyElement,
        cx: &App,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        h_flex()
            .id(("srow", id))
            .gap(px(S7))
            .py(px(S5))
            .items_center()
            .border_b_1()
            .border_color(t.ln)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(S1))
                    .child(div().text_color(t.tx).child(label))
                    .children((!desc.is_empty()).then(|| {
                        div()
                            .text_size(px(TX_SM))
                            .line_height(px(18.))
                            .text_color(t.tx3)
                            .whitespace_normal()
                            .child(desc)
                    })),
            )
            .child(div().flex_none().child(control))
            .into_any_element()
    }

    fn controls(&self, ctl: Ctl, cx: &mut Context<Self>) -> AnyElement {
        if ctl.is_toggle() {
            return self.switch(ctl, cx);
        }
        let mut items = Vec::new();
        for ch in self.choices(ctl) {
            let (apply, lang) = (ch.apply.clone(), ch.lang);
            let l = cx.listener(move |this, _: &ClickEvent, _, cx| {
                match lang {
                    Some(l) => cx.emit(SettingsPageEvent::Language(l)),
                    None => {
                        crate::settings::update(cx, |s| apply(s));
                        this.s = crate::settings::get(cx).clone();
                        cx.emit(SettingsPageEvent::Changed);
                    }
                }
                cx.notify();
            });
            items.push(SegItem::new(
                ch.id,
                ch.label,
                ch.on,
                Rc::new(move |e, w, cx| l(e, w, cx)),
            ));
        }
        seg(items, cx)
    }

    /// An on/off setting as a switch: `choices` gives [on, off], in that order.
    fn switch(&self, ctl: Ctl, cx: &mut Context<Self>) -> AnyElement {
        let mut ch = self.choices(ctl).into_iter();
        let (Some(on), Some(off)) = (ch.next(), ch.next()) else {
            return div().into_any_element();
        };
        let (set_on, set_off) = (on.apply, off.apply);
        let me = cx.entity().downgrade();
        Switch::new(SharedString::from(format!("sw-{ctl:?}")))
            .checked(on.on)
            .color(crate::shell::theme::tokens(cx).acc)
            .on_change(move |next, _, cx| {
                let apply = if *next { &set_on } else { &set_off }.clone();
                let _ = me.update(cx, |this, cx| {
                    crate::settings::update(cx, |s| apply(s));
                    this.s = crate::settings::get(cx).clone();
                    cx.emit(SettingsPageEvent::Changed);
                    cx.notify();
                });
            })
            .into_any_element()
    }

    fn render_cat(
        &self,
        cat: Cat,
        rows: Vec<AnyElement>,
        show_title: bool,
        cx: &App,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        v_flex()
            .mb(px(S8))
            .children(show_title.then(|| {
                div()
                    .pb(px(S4))
                    .text_size(px(TX_LG))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.tx1)
                    .child(Self::cat_name(cat))
            }))
            .children(rows)
            .into_any_element()
    }
}

impl Render for SettingsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        let q = self.query.trim().to_string();
        let searching = !q.is_empty();
        let mut sections: Vec<AnyElement> = Vec::new();
        let mut shown_rows = 0usize;
        for (cat, _, _) in CATS {
            if !searching && *cat != self.cat {
                continue;
            }
            let rows = self.cat_rows(*cat, &q, cx);
            if rows.is_empty() {
                continue;
            }
            shown_rows += rows.len();
            sections.push(self.render_cat(*cat, rows, true, cx));
        }
        let pane: AnyElement = if shown_rows == 0 {
            div()
                .py(px(S8))
                .text_color(t.tx3)
                .child(t!("sp.no_match", q = q.clone()).into_owned())
                .into_any_element()
        } else {
            v_flex().children(sections).into_any_element()
        };
        let nav = self.render_nav(searching, cx);
        h_flex()
            .id("settings-page")
            .track_focus(&self.focus)
            .size_full()
            .child(
                v_flex()
                    .flex_none()
                    .w(px(232.))
                    .h_full()
                    .gap(px(S5))
                    .px(px(S5))
                    .py(px(S6))
                    .border_r_1()
                    .border_color(t.ln)
                    .child(
                        Input::new(&self.search)
                            .px(px(crate::shell::scale::S5))
                            .cleanable(true)
                            .prefix(
                                crate::shell::icons::icon("search")
                                    .size(px(14.))
                                    .text_color(t.tx3),
                            ),
                    )
                    .child(nav),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        div()
                            .id("settings-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .px(px(S8))
                            .pt(px(S7))
                            .pb(px(S8))
                            .child(v_flex().w_full().max_w(px(720.)).mx_auto().child(pane)),
                    )
                    .vertical_scrollbar(&self.scroll),
            )
    }
}

impl SettingsPage {
    /// Rows of one category (shortcut list, About, or its settings), filtered by the search text.
    fn cat_rows(&self, cat: Cat, q: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = crate::shell::theme::tokens(cx);
        let searching = !q.is_empty();
        let mut rows = Vec::new();
        if cat == Cat::Shortcuts {
            for (i, (label, keys)) in crate::shell::shortcuts::listing(cx).into_iter().enumerate() {
                if searching
                    && !matches_query(&format!("{label} {keys} {}", Self::cat_name(cat)), q)
                {
                    continue;
                }
                let keys_el = div()
                    .px(px(S4))
                    .py(px(S1))
                    .rounded(px(R_SM))
                    .bg(t.hv)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(px(TX_MONO))
                    .text_color(t.tx2)
                    .child(keys);
                rows.push(self.row(
                    1000 + i,
                    label,
                    String::new(),
                    keys_el.into_any_element(),
                    cx,
                ));
            }
        } else if cat == Cat::About {
            let line = |id: usize, label: String, v: &'static str| {
                let val = div().text_color(t.tx2).child(v);
                self.row(id, label, String::new(), val.into_any_element(), cx)
            };
            let (name, tag) = (t!("sp.about_name").into_owned(), t!("sp.about_tagline"));
            let (lv, ll) = (t!("sp.about_version").into_owned(), t!("sp.about_license"));
            let hay = format!("{name} {tag} {lv} {ll} {}", Self::cat_name(cat));
            if !searching || matches_query(&hay, q) {
                let head = h_flex()
                    .gap(px(S6))
                    .py(px(S6))
                    .items_center()
                    .border_b_1()
                    .border_color(t.ln)
                    .child(crate::shell::icons::mark(56.))
                    .child(
                        v_flex()
                            .gap(px(S1))
                            .child(
                                div()
                                    .text_size(px(TX_LG))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.tx1)
                                    .child(name),
                            )
                            .child(div().text_color(t.tx3).child(tag.into_owned())),
                    );
                rows.push(head.into_any_element());
                rows.push(line(2000, lv, locus_core::update::CURRENT));
                rows.push(line(2001, ll.into_owned(), "AGPL-3.0-only"));
            }
            let (lu, la) = (t!("upd.label").into_owned(), t!("upd.auto").into_owned());
            let la_d = t!("upd.auto_note").into_owned();
            let (ld, ld_d) = (
                t!("upd.auto_download").into_owned(),
                t!("upd.auto_download_note").into_owned(),
            );
            let (lb, lb_d) = (
                t!("upd.beta").into_owned(),
                t!("upd.beta_note").into_owned(),
            );
            let hay = format!(
                "{lu} {la} {la_d} {ld} {ld_d} {lb} {lb_d} {}",
                Self::cat_name(cat)
            );
            if let Some(u) =
                crate::update::entity(cx).filter(|_| !searching || matches_query(&hay, q))
            {
                let self_updating = u.read(cx).can_install();
                rows.push(self.update_row(&u, lu, cx));
                let ctl = self.controls(Ctl::UpdateCheck, cx);
                rows.push(self.row(2003, la, la_d, ctl, cx));
                if self_updating {
                    let ctl = self.controls(Ctl::UpdateDownload, cx);
                    rows.push(self.row(2004, ld, ld_d, ctl, cx));
                    let ctl = self.controls(Ctl::UpdateBeta, cx);
                    rows.push(self.row(2005, lb, lb_d, ctl, cx));
                }
            }
        } else {
            for (i, (c, lk, dk, ctl)) in DEFS.iter().enumerate() {
                if *c != cat
                    || (matches!(ctl, Ctl::Material | Ctl::Translucency)
                        && !crate::shell::style::glass_experiment())
                {
                    continue;
                }
                let (label, desc) = (t!(*lk).into_owned(), t!(*dk).into_owned());
                if searching && !matches_query(&format!("{label} {desc}"), q) {
                    continue;
                }
                let control = self.controls(*ctl, cx);
                rows.push(self.row(i, label, desc, control, cx));
            }
        }
        rows
    }

    /// About › Updates: what the check knows and the next step. A newer release opens its
    /// page (installer, portable exe) or offers the command (Scoop owns its copy).
    fn update_row(
        &self,
        u: &Entity<crate::update::Updates>,
        label: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ur = u.read(cx);
        let checking = ur.status == crate::update::Status::Checking;
        let avail = ur.available().cloned();
        let hinted = avail.as_ref().is_some_and(|r| ur.state.should_hint(r));
        let cmd = ur.install.command();
        let mut desc = ur.summary();
        let mut ctl = h_flex().gap(px(S3)).items_center();
        if let Some(r) = avail {
            match cmd {
                Some(c) => {
                    desc = format!("{desc} {}", t!("upd.by_command"));
                    let t = crate::shell::theme::tokens(cx);
                    ctl = ctl
                        .child(
                            div()
                                .px(px(S4))
                                .py(px(S1))
                                .rounded(px(R_SM))
                                .bg(t.hv)
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(px(TX_MONO))
                                .text_color(t.tx2)
                                .child(c),
                        )
                        .child(
                            Button::new("upd-copy")
                                .label(t!("upd.copy").into_owned())
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(c.into()));
                                    crate::shell::toast::show(t!("upd.copied").into_owned(), cx);
                                }),
                        );
                }
                None => {
                    use crate::update::Dl;
                    let dl = ur.dl_for_available();
                    // Unsigned releases can only be installed by hand: no retry, the release
                    // page becomes the main action.
                    let by_hand = dl.unsigned();
                    match dl {
                        _ if by_hand => {}
                        Dl::Ready { .. } => {
                            ctl = ctl.child(
                                Button::new("upd-restart")
                                    .primary()
                                    .label(t!("upd.restart").into_owned())
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(SettingsPageEvent::RestartToUpdate)
                                    })),
                            );
                        }
                        Dl::Downloading { .. } => {}
                        failed @ (Dl::Idle | Dl::Failed { .. }) => {
                            let label = if matches!(failed, Dl::Failed { .. }) {
                                t!("upd.retry")
                            } else {
                                t!("upd.download_update")
                            };
                            let u = u.clone();
                            ctl = ctl.child(
                                Button::new("upd-get")
                                    .primary()
                                    .label(label.into_owned())
                                    .on_click(move |_, _, cx| {
                                        u.update(cx, |u, cx| u.download(true, cx))
                                    }),
                            );
                        }
                    }
                    // The release page: notes, and the files for doing it by hand.
                    let url = r.url.clone();
                    let open = Button::new("upd-open")
                        .label(t!("upd.release_page").into_owned())
                        .on_click(move |_, _, cx| cx.open_url(&url));
                    ctl = ctl.child(if by_hand {
                        open.primary()
                    } else {
                        open.ghost()
                    });
                }
            }
            if hinted {
                let u = u.clone();
                ctl = ctl.child(
                    Button::new("upd-dismiss")
                        .ghost()
                        .label(t!("upd.dismiss").into_owned())
                        .on_click(move |_, _, cx| u.update(cx, |u, cx| u.dismiss(cx))),
                );
            }
        }
        let u = u.clone();
        ctl = ctl.child(
            Button::new("upd-check")
                .label(t!("upd.check_now").into_owned())
                .disabled(checking)
                .on_click(move |_, _, cx| u.update(cx, |u, cx| u.check(true, cx))),
        );
        self.row(2002, label, desc, ctl.into_any_element(), cx)
    }

    /// Category list on the left.
    fn render_nav(&self, searching: bool, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let mut nav = v_flex().gap(px(S1));
        for (cat, key, icon) in CATS {
            let on = !searching && *cat == self.cat;
            let cat = *cat;
            nav = nav.child(
                h_flex()
                    .id(("scat", cat as usize))
                    .h(px(H_ROW))
                    .px(px(S4))
                    .gap(px(S4))
                    .items_center()
                    .rounded(px(R_MD))
                    .cursor_pointer()
                    .text_color(if on { t.tx1 } else { t.tx2 })
                    .when(on, |r| r.bg(t.on))
                    .hover(|r| r.bg(t.hv))
                    .child(
                        crate::shell::icons::icon(icon)
                            .size(px(14.))
                            .text_color(t.tx3),
                    )
                    .child(t!(*key).into_owned())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.cat = cat;
                        this.query.clear();
                        this.search.update(cx, |s, cx| s.set_value("", window, cx));
                        cx.notify();
                    })),
            );
        }
        nav
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn search_matches_every_word_in_any_order() {
        assert!(matches_query(
            "Row limit of new tabs Existing tabs keep their own limit",
            "limit"
        ));
        assert!(matches_query("Row limit of new tabs", "TABS row"));
        assert!(!matches_query("Row limit of new tabs", "row cap"));
        assert!(matches_query("anything", "   "));
    }

    #[::core::prelude::v1::test]
    fn about_is_in_the_nav() {
        let e = CATS
            .iter()
            .find(|c| c.0 == Cat::About)
            .expect("About entry");
        assert_eq!(e.1, "sp.cat_about");
        assert_eq!(CATS.iter().filter(|c| c.0 == Cat::About).count(), 1);
        for k in ["sp.cat_about", "sp.about_version", "sp.about_license"] {
            assert_ne!(t!(k), k, "missing string {k}");
        }
    }

    #[::core::prelude::v1::test]
    fn every_setting_is_listed_once() {
        for ctl in [
            Ctl::Language,
            Ctl::Theme,
            Ctl::FontSize,
            Ctl::Motion,
            Ctl::Material,
            Ctl::Translucency,
            Ctl::SpaceTint,
            Ctl::GridStripes,
            Ctl::RowLimit,
            Ctl::RowsCap,
            Ctl::PageIdle,
            Ctl::Count,
            Ctl::AutoPlan,
            Ctl::ConfirmNonprod,
            Ctl::FormatCase,
            Ctl::FormatIndent,
        ] {
            assert_eq!(DEFS.iter().filter(|d| d.3 == ctl).count(), 1, "{ctl:?}");
        }
    }
}
