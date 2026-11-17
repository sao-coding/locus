//! Design tokens of the "Strata v3" shell.
//!
//! Dark values are the mockup's (`:root` for the translucent window materials,
//! `body.solid` for the opaque one) with the UI audit's T0 changes. The light set
//! is derived from them: same hues, warm paper surfaces, text levels darkened
//! until each still reads. `apply` copies the tokens onto gpui-component's theme
//! so dialogs, menus, inputs and tables use them too.
//!
//! Surface model (audit S1): the main card is `main` + a little Space colour
//! (`style::card_bg`); the editor, grid, History detail and diagram canvas draw
//! **no** fill of their own and show the card; `ed` is only the header strip /
//! current line a touch lighter; the right panel and rail are `panel` (card +3 %,
//! `style::panel_bg`). Only the sidebar and title bar (the window gradient,
//! `style::window_bg`) are translucent; popups are `pop` at `POP_ALPHA`.
//!
//! Text colours: `tx`..`tx4` are all >= 4.5 : 1 on every surface (tested in
//! `locus_core::colors`); `tx5` is decoration only. Use `acc_text`,
//! `grn_text`, `yel_text`, `red_text` when the colour is *text*; `acc`, `grn`,
//! `yel`, `red` stay for fills, strokes and icons.

use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::*;
use locus_core::colors::{DARK as PD, LIGHT as PL, Pal};
use locus_core::store::{DbKind, Env};
use std::sync::LazyLock;

fn hex(v: u32) -> Hsla {
    Hsla::from(rgb(v))
}

fn hexa(v: u32, a: f32) -> Hsla {
    let mut c = hex(v);
    c.a = a;
    c
}

/// Alpha of popups (dialogs, palette, menus, popovers). gpui-fast has no
/// element-level backdrop blur, so a see-through popup would show sharp text
/// beneath it; 0.95 keeps that under the noise floor.
pub const POP_ALPHA_DARK: f32 = 0.95;
pub const POP_ALPHA_LIGHT: f32 = 0.96;

/// One theme's tokens (`--tx`…`--sx` in the mockup).
pub struct Tokens {
    /// Text levels: body, strongest, then four steps down (`tx5` = decoration only).
    pub tx: Hsla,
    pub tx1: Hsla,
    pub tx2: Hsla,
    pub tx3: Hsla,
    pub tx4: Hsla,
    pub tx5: Hsla,
    /// Hairlines (`--ln`, `--ln2`), hover and "on" fills.
    pub ln: Hsla,
    pub ln2: Hsla,
    pub hv: Hsla,
    pub on: Hsla,
    /// Slightly stronger fills used by pinned tiles and the active tab row.
    pub tile: Hsla,
    pub tile_on: Hsla,
    pub acc: Hsla,
    /// Accent as text (links, active labels); darker than `acc` in light.
    pub acc_text: Hsla,
    /// Text on the accent (Run button).
    pub acc_fg: Hsla,
    /// Grid selection fill and the active-cell outline.
    pub sel: Hsla,
    pub sel_ring: Hsla,
    /// Focus ring of inputs: the accent at 55 %.
    pub focus: Hsla,
    pub red: Hsla,
    /// Red used as text (Production badge, errors).
    pub red_text: Hsla,
    pub grn: Hsla,
    pub grn_text: Hsla,
    pub yel: Hsla,
    pub yel_text: Hsla,
    /// Surfaces, opaque: window, sidebar, main card, editor strip, panel, popover,
    /// schema panel, status bar.
    pub win: Hsla,
    pub side: Hsla,
    pub main: Hsla,
    pub ed: Hsla,
    pub panel: Hsla,
    pub pop: Hsla,
    pub foot: Hsla,
    /// Ring around the main card, and its drop shadow colour.
    pub ring: Hsla,
    pub shadow: Hsla,
    /// Idle status dot.
    pub idle: Hsla,
    /// Staging env badge text.
    pub stg: Hsla,
    /// Tab-kind colours, in `TabKind` order (Query, Table, Schema, History, Ddl, Script, Analysis, Settings).
    pub tab: [Hsla; 8],
    /// Engine badge colours: MariaDB, MySQL, PostgreSQL, Oracle (SQL Server, SQLite, Redis for later).
    pub db: [Hsla; 7],
    pub dark: bool,
}

/// Light-theme version of a mid-tone colour: more saturated, darker.
fn ink(c: Hsla) -> Hsla {
    hsla(c.h, (c.s + 0.1).min(1.0), c.l * 0.6, 1.0)
}

const TAB_COLORS: [u32; 8] = [
    0xb7aea1, 0x8fa7b3, 0xa79bbd, 0xa8a090, 0xbfa27a, 0x9fb58f, 0xc19585, 0x9aa3ad,
];
const DB_COLORS: [u32; 7] = [
    0xb98a62, 0xc79a5b, 0x7d96b4, 0xc46b5a, 0xa68fb0, 0x86a8a4, 0xbf7a7a,
];

fn palette(
    p: &Pal,
) -> (
    Hsla,
    Hsla,
    Hsla,
    Hsla,
    Hsla,
    Hsla,
    Hsla,
    Hsla,
    Hsla,
    Hsla,
    Hsla,
    Hsla,
    Hsla,
    Hsla,
    Hsla,
) {
    (
        hex(p.tx),
        hex(p.tx1),
        hex(p.tx2),
        hex(p.tx3),
        hex(p.tx4),
        hex(p.tx5),
        hex(p.acc_text),
        hex(p.grn_text),
        hex(p.yel_text),
        hex(p.red_text),
        hex(p.side),
        hex(p.main),
        hex(p.ed),
        hex(p.panel),
        hex(p.pop),
    )
}

pub static DARK: LazyLock<Tokens> = LazyLock::new(|| {
    let (
        tx,
        tx1,
        tx2,
        tx3,
        tx4,
        tx5,
        acc_text,
        grn_text,
        yel_text,
        red_text,
        side,
        main,
        ed,
        panel,
        pop,
    ) = palette(&PD);
    let acc = hex(0xe8803a);
    Tokens {
        tx,
        tx1,
        tx2,
        tx3,
        tx4,
        tx5,
        ln: hexa(0xfff4e6, 0.07),
        ln2: hexa(0xfff4e6, 0.11),
        hv: hexa(0xfff4e6, 0.06),
        on: hexa(0xfff4e6, 0.10),
        tile: hexa(0xfff4e6, 0.045),
        tile_on: hexa(0xfff4e6, 0.11),
        acc,
        acc_text,
        acc_fg: hex(0x1a1410),
        sel: acc.opacity(0.16),
        sel_ring: acc.opacity(0.70),
        focus: acc.opacity(0.55),
        red: hex(0xc9503a),
        red_text,
        grn: hex(0x7fb685),
        grn_text,
        yel: hex(0xd9a441),
        yel_text,
        win: hex(0x1b1917),
        side,
        main,
        ed,
        panel,
        pop,
        foot: hex(0x151311),
        ring: hexa(0xfff4e6, 0.07),
        shadow: hsla(0., 0., 0., 0.25),
        idle: hex(0x6b645a),
        stg: hex(0xcdb07a),
        tab: TAB_COLORS.map(hex),
        db: DB_COLORS.map(hex),
        dark: true,
    }
});

/// Light counterpart: warm paper, ink text. `tx`..`tx4` keep at least 4.5:1 on
/// every surface; `tx5` is decoration only.
pub static LIGHT: LazyLock<Tokens> = LazyLock::new(|| {
    let (
        tx,
        tx1,
        tx2,
        tx3,
        tx4,
        tx5,
        acc_text,
        grn_text,
        yel_text,
        red_text,
        side,
        main,
        ed,
        panel,
        pop,
    ) = palette(&PL);
    let acc = hex(0xd46f28);
    Tokens {
        tx,
        tx1,
        tx2,
        tx3,
        tx4,
        tx5,
        ln: hexa(0x3c2814, 0.10),
        ln2: hexa(0x3c2814, 0.18),
        hv: hexa(0x3c2814, 0.05),
        on: hexa(0x3c2814, 0.09),
        tile: hexa(0x3c2814, 0.05),
        tile_on: hexa(0x3c2814, 0.11),
        acc,
        acc_text,
        acc_fg: hex(0x1a1410),
        sel: acc.opacity(0.14),
        sel_ring: acc.opacity(0.70),
        focus: acc.opacity(0.55),
        red: hex(0xc4513c),
        red_text,
        grn: hex(0x4e8a56),
        grn_text,
        yel: hex(0xb07f1f),
        yel_text,
        win: hex(0xebe5dc),
        side,
        main,
        ed,
        panel,
        pop,
        foot: hex(0xf1ece5),
        ring: hexa(0x3c2814, 0.14),
        shadow: hsla(0., 0., 0., 0.10),
        idle: hex(0xa39b8f),
        stg: hex(0x8f6d2a),
        tab: TAB_COLORS.map(|c| ink(hex(c))),
        db: DB_COLORS.map(|c| ink(hex(c))),
        dark: false,
    }
});

pub fn tokens(cx: &App) -> &'static Tokens {
    if cx.theme().is_dark() { &DARK } else { &LIGHT }
}

// ---- database badges (`DB` in the mockup) ----------------------------------------

/// Short label and colour of an engine badge (tokens `db`).
pub fn db_badge(kind: DbKind, mariadb: bool, cx: &App) -> (&'static str, Hsla) {
    let d = &tokens(cx).db;
    match (kind, mariadb) {
        (DbKind::Mysql, true) => ("Ma", d[0]),
        (DbKind::Mysql, false) => ("My", d[1]),
        (DbKind::Postgres, _) => ("Pg", d[2]),
        (DbKind::Oracle, _) => ("Or", d[3]),
        (DbKind::Sqlite, _) => ("Sq", d[4]),
        (DbKind::Redis, _) => ("Re", d[6]),
    }
}

/// Badge colours: background (colour at 15 %), text (the light tokens are already darkened).
pub fn badge_colors(c: Hsla, cx: &App) -> (Hsla, Hsla) {
    let mut bg = c;
    bg.a = if tokens(cx).dark { 0.15 } else { 0.2 };
    let fg = if tokens(cx).dark {
        c
    } else {
        hsla(c.h, (c.s + 0.1).min(1.0), (c.l * 0.62).max(0.2), 1.0)
    };
    (bg, fg)
}

// ---- tab types (`TC`) ------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // Table, Ddl, Analysis: tab types of later tasks.
pub enum TabKind {
    Query,
    Table,
    Schema,
    History,
    Ddl,
    Script,
    Analysis,
    Settings,
}

impl TabKind {
    pub fn color(self, cx: &App) -> Hsla {
        tokens(cx).tab[self as usize]
    }

    /// Icon path (mockup's `IC` glyphs, served by `icons::AppAssets`).
    pub fn icon(self) -> &'static str {
        match self {
            TabKind::Query => "app/query.svg",
            TabKind::Table => "app/table.svg",
            TabKind::Schema => "app/schema.svg",
            TabKind::History => "app/history.svg",
            TabKind::Ddl => "app/ddl.svg",
            TabKind::Script => "app/script.svg",
            TabKind::Analysis => "app/analysis.svg",
            TabKind::Settings => "app/gear.svg",
        }
    }
}

// ---- environment badges ------------------------------------------------------------

/// Background and text colour of an environment badge.
pub fn env_colors(env: Env, cx: &App) -> (Hsla, Hsla) {
    let t = tokens(cx);
    match env {
        Env::Prod => {
            let mut bg = t.red;
            bg.a = if t.dark { 0.16 } else { 0.14 };
            (bg, t.red_text)
        }
        Env::Staging => (t.hv.opacity(1.2), t.stg),
        _ => (
            if t.dark {
                hexa(0xfff4e6, 0.06)
            } else {
                hexa(0x3c2814, 0.07)
            },
            t.tx3,
        ),
    }
}

// ---- connection status dots ---------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnState {
    Ok,
    Slow,
    Connecting,
    Failed,
    Idle,
}

pub fn state_color(s: ConnState, cx: &App) -> Hsla {
    let t = tokens(cx);
    match s {
        ConnState::Ok => t.grn,
        ConnState::Slow | ConnState::Connecting => t.yel,
        ConnState::Failed => t.red,
        ConnState::Idle => t.idle,
    }
}

// ---- fonts ------------------------------------------------------------------------

/// UI fonts in order of preference; only installed ones are used (no bundling).
const UI_FONTS: &[&str] = &[
    "Inter",
    "Segoe UI Variable Text",
    "Segoe UI Variable",
    "Segoe UI",
    "Microsoft JhengHei UI",
    "Microsoft JhengHei",
];
const MONO_FONTS: &[&str] = &[
    "JetBrains Mono",
    "Cascadia Code",
    "Cascadia Mono",
    "Consolas",
];

fn pick(installed: &[String], list: &[&str]) -> Option<SharedString> {
    list.iter()
        .find(|f| installed.iter().any(|n| n == *f))
        .map(|f| SharedString::from(*f))
}

static FONTS: std::sync::OnceLock<(Option<SharedString>, Option<SharedString>)> =
    std::sync::OnceLock::new();

/// (UI, mono) families picked from what is installed.
pub fn fonts(cx: &App) -> &'static (Option<SharedString>, Option<SharedString>) {
    FONTS.get_or_init(|| {
        let installed = cx.text_system().all_font_names();
        (pick(&installed, UI_FONTS), pick(&installed, MONO_FONTS))
    })
}

/// Default UI font size (the mockup's 12.5 px rounds to 13).
pub const UI_FONT_SIZE: f32 = 13.0;

// ---- applying to gpui-component --------------------------------------------------

/// Copies the tokens onto the component theme (after `Theme::change`).
pub fn apply(font_size: Option<f32>, cx: &mut App) {
    let t = tokens(cx);
    let (ui, mono) = fonts(cx).clone();
    let none = gpui_kit::transparent_black();
    Theme::update(cx, |th| {
        // The default (`Scrolling`) hides scrollbars until the wheel moves, so the
        // result grid's horizontal bar was invisible and wide result sets looked cut off.
        th.scrollbar_mode = gpui_kit::component::scroll::ScrollbarMode::Always;
        th.focus_ring = false; // 1 px tinted border instead of the thick outer ring
        let c = &mut th.colors;
        c.background = t.main;
        c.foreground = t.tx;
        c.muted = t.panel;
        c.muted_foreground = t.tx3;
        c.border = t.ln2;
        c.input = t.ln2;
        c.ring = t.focus;
        c.caret = t.acc;
        c.selection = t.acc.opacity(0.28);
        c.primary = t.acc;
        c.primary_hover = t.acc.opacity(0.92);
        c.primary_active = t.acc.opacity(0.85);
        c.primary_foreground = t.acc_fg;
        c.button_primary = t.acc;
        c.button_primary_hover = t.acc.opacity(0.92);
        c.button_primary_active = t.acc.opacity(0.85);
        c.button_primary_foreground = t.acc_fg;
        c.secondary = t.on;
        c.secondary_hover = t.ln2;
        c.secondary_active = t.ln2;
        c.secondary_foreground = t.tx;
        c.accent = t.on;
        c.accent_foreground = t.tx1;
        // Popups: `pop` slightly see-through (see `POP_ALPHA_*`).
        c.popover = t.pop.opacity(if t.dark {
            POP_ALPHA_DARK
        } else {
            POP_ALPHA_LIGHT
        });
        c.popover_foreground = t.tx;
        // Lists and tables inherit the card; only the head strip is a touch lighter.
        c.list = none;
        c.list_hover = t.hv;
        c.list_active = t.sel;
        c.list_active_border = t.sel_ring;
        c.list_even = none;
        c.list_head = t.ed;
        c.table = none;
        c.table_head = t.ed;
        c.table_head_foreground = t.tx2;
        c.table_hover = t.hv;
        c.table_active = t.sel;
        c.table_active_border = t.sel_ring;
        c.table_even = none;
        c.table_row_border = t.ln;
        c.tab_bar = none;
        c.tab = none;
        c.tab_active = none;
        c.tab_foreground = t.tx3;
        c.tab_active_foreground = t.tx1;
        c.title_bar = gpui_kit::transparent_black();
        c.title_bar_border = gpui_kit::transparent_black();
        c.status_bar = t.foot;
        c.status_bar_border = t.ln;
        c.sidebar = t.side;
        c.sidebar_foreground = t.tx2;
        c.sidebar_border = t.ln;
        c.sidebar_accent = t.on;
        c.sidebar_accent_foreground = t.tx1;
        c.danger = t.red;
        c.success = t.grn;
        c.warning = t.yel;
        c.link = t.acc_text;
        c.scrollbar_thumb = t.ln2.opacity(1.3);
        c.overlay = hsla(0.0, 0.0, 0.0, if t.dark { 0.28 } else { 0.14 }); // D6
        c.window_border = t.ln;
        c.drag_border = t.acc;
        c.drop_target = t.acc.opacity(0.12);
        // Editor: no fill (the card shows), a faint current line, the mockup gutter colours.
        let mut ht = (*th.highlight_theme).clone();
        ht.style.editor_background = Some(none);
        ht.style.editor_gutter_background = Some(none);
        ht.style.editor_active_line = Some(if t.dark {
            hexa(0xfff4e6, 0.035)
        } else {
            hexa(0x3c2814, 0.04)
        });
        ht.style.editor_line_number = Some(t.tx5);
        ht.style.editor_active_line_number = Some(t.tx2);
        ht.style.editor_foreground = Some(t.tx);
        th.highlight_theme = std::sync::Arc::new(ht);
        th.radius = px(8.);
        th.radius_lg = px(16.);
        if let Some(f) = ui {
            th.font_family = f;
        }
        if let Some(f) = mono {
            th.mono_font_family = f;
        }
        let size = font_size
            .filter(|f| (10.0..=24.0).contains(f))
            .unwrap_or(UI_FONT_SIZE);
        th.font_size = px(size);
        th.mono_font_size = px(size);
    });
}
