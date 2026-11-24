//! Shared look helpers: window background, Space colour, theme application, small widgets.
use super::*;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

/// Window material index into `locus_core::settings::MATERIALS` (settings).
pub(crate) static MATERIAL: AtomicU8 = AtomicU8::new(0);
/// Translucency choice (settings), f32 bits.
pub(crate) static TRANSLUCENCY: AtomicU32 = AtomicU32::new(0);
/// "Space tint" setting (0..2).
pub(crate) static SPACE_TINT: AtomicU32 = AtomicU32::new(0x3f80_0000);
/// Light / dark pinned by the active Space (0 = follow the setting, 1 = light, 2 = dark).
pub(crate) static SPACE_APPEARANCE: AtomicU8 = AtomicU8::new(0);

pub(crate) fn space_color(hue: f32, cx: &App) -> Hsla {
    if cx.theme().is_dark() {
        hsla(hue, 0.45, 0.50, 1.0)
    } else {
        hsla(hue, 0.6, 0.48, 1.0)
    }
}

/// `a` mixed toward `b` by `w` (0..1), in RGB.
pub(crate) fn mix(a: Hsla, b: Hsla, w: f32) -> Hsla {
    let (x, y) = (Rgba::from(a), Rgba::from(b));
    let w = w.clamp(0.0, 1.0);
    let l = |p: f32, q: f32| p + (q - p) * w;
    Hsla::from(Rgba {
        r: l(x.r, y.r),
        g: l(x.g, y.g),
        b: l(x.b, y.b),
        a: 1.0,
    })
}

/// Alpha of the window gradient now: material, theme and translucency choice.
pub(crate) fn glass_alpha(dark: bool) -> f32 {
    let m = locus_core::settings::MATERIALS
        .get(MATERIAL.load(Ordering::Relaxed) as usize)
        .copied()
        .unwrap_or("mica");
    locus_core::settings::glass_alpha(
        m,
        dark,
        f32::from_bits(TRANSLUCENCY.load(Ordering::Relaxed)),
    )
}

/// Window colour of key colour `c` at tint weight `w`, with the glass alpha.
pub(crate) fn window_stop(
    c: locus_core::space_theme::SpaceColor,
    w: f32,
    strength: f32,
    cx: &App,
) -> Hsla {
    let t = crate::shell::theme::tokens(cx);
    let k = f32::from_bits(SPACE_TINT.load(Ordering::Relaxed))
        * strength
        * if t.dark { 1.0 } else { 0.6 };
    let mut col = mix(t.win, tint_color(c, cx), w * k);
    col.a = glass_alpha(t.dark);
    col
}

fn space_k(dark: bool, strength: f32) -> f32 {
    f32::from_bits(SPACE_TINT.load(Ordering::Relaxed)) * strength * if dark { 1.0 } else { 0.6 }
}

/// Surface of the main card: `main` carrying 6 % of the Space colour. The editor, grid,
/// History detail and diagram canvas draw no fill and show this.
pub(crate) fn card_bg(look: Look, cx: &App) -> Hsla {
    let t = crate::shell::theme::tokens(cx);
    mix(
        t.main,
        tint_color(look.primary(), cx),
        0.06 * space_k(t.dark, look.strength),
    )
}

/// Right panel and rail: the card +3 % lightness (`panel` grey with the same Space tint).
pub(crate) fn panel_bg(look: Look, cx: &App) -> Hsla {
    let t = crate::shell::theme::tokens(cx);
    mix(
        t.panel,
        tint_color(look.primary(), cx),
        0.06 * space_k(t.dark, look.strength),
    )
}

/// Tint weights of the window gradient at its top and bottom. One colour: the mockup's
/// fade (halved in the T0 audit, dark .18 → .05, light .12 → .04 after the light factor).
/// Two or three: a flatter fade so the lower colours show.
fn window_weights(n: usize, dark: bool) -> (f32, f32) {
    match (n, dark) {
        (1, true) => (0.18, 0.05),
        (1, false) => (0.20, 0.067),
        (_, true) => (0.18, 0.12),
        (_, false) => (0.20, 0.14),
    }
}

/// The window gradient's colour at `t` (0 top .. 1 bottom), for what has to blend into it
/// (the sidebar list's fade).
pub(crate) fn window_at(look: Look, t: f32, cx: &App) -> Hsla {
    if look.n == 1 {
        // As before themes: the fade colour of a one-colour Space.
        return window_stop(look.primary(), 0.12, look.strength, cx);
    }
    let (top, bottom) = window_weights(look.n, cx.theme().is_dark());
    let theme = locus_core::space_theme::SpaceTheme {
        colors: look.colors[..look.n].to_vec(),
        intensity: 0.5,
        appearance: Default::default(),
    };
    window_stop(
        theme.color_at(t),
        top + (bottom - top) * t,
        look.strength,
        cx,
    )
}

/// Window background segments, top to bottom (each a two-stop gradient, as GPUI draws
/// them): one for one or two colours, two halves for three. Its alpha is `glass_alpha`: it is
/// the only translucent surface (sidebar, title bar, gutter).
pub(crate) fn window_bg(look: Look, cx: &App) -> Vec<Background> {
    if glass_debug() {
        // LOCUS_GLASS=debug: a flat magenta 30 % fill, to see what is behind the sidebar.
        let m = hsla(0.83, 1.0, 0.5, 0.3);
        return vec![linear_gradient(
            160.,
            linear_color_stop(m, 0.),
            linear_color_stop(m, 1.),
        )];
    }
    let (top, bottom) = window_weights(look.n, crate::shell::theme::tokens(cx).dark);
    let c = &look.colors;
    let st = |c, w| window_stop(c, w, look.strength, cx);
    match look.n {
        1 => vec![linear_gradient(
            160.,
            linear_color_stop(st(c[0], top), 0.),
            linear_color_stop(st(c[0], bottom), 1.),
        )],
        2 => vec![linear_gradient(
            180.,
            linear_color_stop(st(c[0], top), 0.),
            linear_color_stop(st(c[1], bottom), 1.),
        )],
        _ => {
            let mid = (top + bottom) / 2.0;
            vec![
                linear_gradient(
                    180.,
                    linear_color_stop(st(c[0], top), 0.),
                    linear_color_stop(st(c[1], mid), 1.),
                ),
                linear_gradient(
                    180.,
                    linear_color_stop(st(c[1], mid), 0.),
                    linear_color_stop(st(c[2], bottom), 1.),
                ),
            ]
        }
    }
}

/// What the tint helpers need of the active Space's theme (`Copy`, compared every frame).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Look {
    pub colors: [locus_core::space_theme::SpaceColor; 3],
    pub n: usize,
    /// Tint multiplier from the theme's intensity (1.0 = default).
    pub strength: f32,
}

impl Look {
    pub(crate) fn of(t: &locus_core::space_theme::SpaceTheme) -> Look {
        let p = t.primary();
        let mut colors = [p; 3];
        for (i, c) in t.colors.iter().take(3).enumerate() {
            colors[i] = *c;
        }
        Look {
            colors,
            n: t.colors.len().clamp(1, 3),
            strength: t.strength(),
        }
    }

    pub(crate) fn primary(&self) -> locus_core::space_theme::SpaceColor {
        self.colors[0]
    }
}

impl Default for Look {
    fn default() -> Self {
        Look::of(&locus_core::space_theme::SpaceTheme::of(0, None, None))
    }
}

/// A key colour as drawn in the current light / dark theme (`space_color` for a hue-only Space).
pub(crate) fn tint_color(c: locus_core::space_theme::SpaceColor, cx: &App) -> Hsla {
    let (h, s, l) = locus_core::space_theme::tint_hsl(c, cx.theme().is_dark());
    hsla(h, s, l, 1.0)
}

/// Follows the system light / dark setting. `LOCUS_THEME=light|dark`
/// pins it (used for screenshots).
pub fn apply_theme(window: &mut Window, cx: &mut App) {
    use gpui_kit::component::{Theme, ThemeMode};
    let s = crate::settings::get(cx).clone();
    let space_pin = match SPACE_APPEARANCE.load(Ordering::Relaxed) {
        1 => Some("light".to_string()),
        2 => Some("dark".to_string()),
        _ => None,
    };
    let pick = std::env::var("LOCUS_THEME")
        .ok()
        .or(space_pin)
        .or(s.theme.clone())
        .unwrap_or_default();
    match pick.as_str() {
        "light" => Theme::change(ThemeMode::Light, Some(window), cx),
        "dark" => Theme::change(ThemeMode::Dark, Some(window), cx),
        _ => Theme::sync_system_appearance(Some(window), cx),
    }
    crate::shell::theme::apply(s.font_size, cx);
    if s.reduce_motion {
        // Reduce motion: gpui-component's own transitions (dialogs, menus,
        // handles) become instant too.
        let m = &mut Theme::global_mut(cx).motion;
        m.duration_instant = Duration::ZERO;
        m.duration_fast = Duration::ZERO;
        m.duration_normal = Duration::ZERO;
        m.duration_slow = Duration::ZERO;
    }
    MATERIAL.store(
        locus_core::settings::MATERIALS
            .iter()
            .position(|m| *m == s.material())
            .unwrap_or(0) as u8,
        Ordering::Relaxed,
    );
    TRANSLUCENCY.store(s.translucency_value().to_bits(), Ordering::Relaxed);
    SPACE_TINT.store(s.space_tint().to_bits(), Ordering::Relaxed);
    // Window material behind the (partly see-through) sidebar and title bar.
    window.set_background_appearance(match s.material() {
        "blur" => WindowBackgroundAppearance::Blurred,
        "mica" => WindowBackgroundAppearance::MicaBackdrop,
        "mica_alt" => WindowBackgroundAppearance::MicaAltBackdrop,
        _ => WindowBackgroundAppearance::Opaque,
    });
    let glass = s.material() != "opaque" && glass_experiment();
    extend_glass_frame(glass);
    root_backdrop(glass, cx);
    window.refresh();
}

pub(crate) fn tls_warning(tls: Tls, kind: locus_core::store::DbKind) -> Option<String> {
    // A local file has no network: nothing travels in plain text.
    if kind == locus_core::store::DbKind::Sqlite {
        return None;
    }
    match tls {
        Tls::Required => None,
        Tls::Unverified => Some(t!("sidebar.tls_unverified_tip").into_owned()),
        Tls::Disabled => Some(t!("sidebar.tls_disabled_tip").into_owned()),
    }
}

pub(crate) fn tip(text: String) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |window, cx| gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
}

/// Trash can for delete actions (lucide `trash`, shipped in the asset bundle;
/// the component set's `Delete` is a backspace arrow).
pub fn trash_icon() -> Icon {
    Icon::default().path("icons/trash.svg")
}

// ---- v3 widgets ---------------------------------------------------------------

/// The mockup's `.ib`: a square icon button (26 px, radius 6) with hover fill.
pub(crate) fn ib(
    id: impl Into<ElementId>,
    icon: Icon,
    size: f32,
    on: bool,
    cx: &App,
) -> Stateful<Div> {
    let t = crate::shell::theme::tokens(cx);
    div()
        .id(id)
        .flex_none()
        .size(px(size))
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(if on { t.tx } else { t.tx3 })
        .when(on, |d| d.bg(t.on))
        .hover(|s| s.bg(t.on).text_color(t.tx))
        .child(icon.size(px(if size >= 26.0 { 15. } else { 13. })))
}

/// A keyboard hint (`kbd`).
pub(crate) fn kbd(text: &'static str, cx: &App) -> Div {
    let t = crate::shell::theme::tokens(cx);
    div()
        .flex_none()
        .text_size(px(10.5))
        .font_weight(FontWeight::MEDIUM)
        .font_family(cx.theme().mono_font_family.clone())
        .text_color(t.tx4)
        .child(text)
}

/// Kind of a tab, for its icon and colour.
pub(crate) fn tab_kind(t: &QueryTab) -> crate::shell::theme::TabKind {
    use crate::shell::theme::TabKind;
    if let Some(p) = t.pageslot.kind {
        match p {
            locus_core::spaces::TabPage::History => TabKind::History,
            locus_core::spaces::TabPage::Settings => TabKind::Settings,
            locus_core::spaces::TabPage::Redis | locus_core::spaces::TabPage::Activity => {
                TabKind::Table
            }
            locus_core::spaces::TabPage::Saved => TabKind::Script,
        }
    } else if t.diagram.is_some() {
        TabKind::Schema
    } else if t.structure.is_some() {
        TabKind::Table
    } else if t.doc.path.is_some() {
        TabKind::Script
    } else {
        TabKind::Query
    }
}

pub(crate) fn kind_icon(k: crate::shell::theme::TabKind) -> Icon {
    Icon::default().path(k.icon())
}

/// Lets the window backdrop (Mica / acrylic) show through the client area: DWM draws it
/// behind the whole window only when the frame is extended into it. gpui-fast does not do
/// this itself (it only sets the backdrop type), so without it the sidebar is composited over
/// black whatever its alpha is. Margins -1 = the whole window, 0 = back to the default.
#[cfg(windows)]
fn extend_glass_frame(on: bool) {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Controls::MARGINS;
    use windows_sys::Win32::UI::WindowsAndMessaging::EnumThreadWindows;
    unsafe extern "system" fn each(hwnd: HWND, on: LPARAM) -> i32 {
        let v = if on != 0 { -1 } else { 0 };
        let m = MARGINS {
            cxLeftWidth: v,
            cxRightWidth: v,
            cyTopHeight: v,
            cyBottomHeight: v,
        };
        // SAFETY: `hwnd` is a live window handed in by `EnumThreadWindows`; `m` outlives the call.
        unsafe { DwmExtendFrameIntoClientArea(hwnd, &m) };
        1
    }
    // SAFETY: enumerates windows of the calling thread only; `each` matches WNDENUMPROC and
    // never outlives the call.
    unsafe { EnumThreadWindows(GetCurrentThreadId(), Some(each), on as LPARAM) };
}

#[cfg(not(windows))]
fn extend_glass_frame(_on: bool) {}

/// gpui-component's window root paints `theme.tokens.background` under everything, which hides
/// the window material. With a material the token becomes transparent (the shell paints its own
/// opaque surfaces over it: card, editor, status bar); without one it is the card colour again.
/// Dialogs read the same token for their body, so open them with `open_pop_dialog`.
pub(crate) fn root_backdrop(glass: bool, cx: &mut App) {
    use gpui_kit::component::ThemeToken;
    let th = gpui_kit::component::Theme::global_mut(cx);
    let c = if glass {
        gpui_kit::transparent_black()
    } else {
        th.colors.background
    };
    th.tokens.background = ThemeToken::from(c);
}

/// `open_dialog` with the popup surface (`pop` at the popup alpha) as the dialog body.
pub(crate) trait PopDialog {
    fn open_pop_dialog<F>(&mut self, cx: &mut App, build: F)
    where
        F: Fn(
                gpui_kit::component::dialog::Dialog,
                &mut Window,
                &mut App,
            ) -> gpui_kit::component::dialog::Dialog
            + 'static;
}

impl PopDialog for Window {
    fn open_pop_dialog<F>(&mut self, cx: &mut App, build: F)
    where
        F: Fn(
                gpui_kit::component::dialog::Dialog,
                &mut Window,
                &mut App,
            ) -> gpui_kit::component::dialog::Dialog
            + 'static,
    {
        use gpui_kit::component::WindowExt as _;
        self.open_dialog(cx, move |d, w, cx| {
            let bg = popup_bg(cx);
            build(d, w, cx).bg(bg)
        });
    }
}

/// `LOCUS_GLASS=1` (or `debug`): lets the window material reach the sidebar and title bar.
/// Off by default because on this build (gpui-fast 0.1.15, Windows 11) the client area is
/// composited over black whatever the alpha or material is, so the experiment would only turn the
/// sidebar darker. Delete the switch when that is fixed.
pub(crate) fn glass_experiment() -> bool {
    std::env::var_os("LOCUS_GLASS").is_some()
}

fn glass_debug() -> bool {
    std::env::var("LOCUS_GLASS").as_deref() == Ok("debug")
}

/// Look of the active Space, mirrored here so popups (dialogs, menus) can carry its tint.
static SPACE_LOOK: std::sync::Mutex<Option<Look>> = std::sync::Mutex::new(None);

fn space_look() -> Look {
    SPACE_LOOK.lock().ok().and_then(|g| *g).unwrap_or_default()
}

/// Popup surface: `popover` with the Space colour mixed in like the card (same weight as `card_bg`).
pub(crate) fn popup_bg(cx: &App) -> Hsla {
    let t = crate::shell::theme::tokens(cx);
    let look = space_look();
    let a = cx.theme().popover.a;
    let mut c = mix(
        t.pop,
        tint_color(look.primary(), cx),
        0.06 * space_k(t.dark, look.strength),
    );
    c.a = a;
    c
}

/// Called every frame by the shell with the active Space's look; updates the menu colour when
/// it (or the light / dark theme under it) changed.
pub(crate) fn set_space_look(look: Look, cx: &mut App) {
    if let Ok(mut g) = SPACE_LOOK.lock() {
        *g = Some(look);
    }
    let c = popup_bg(cx);
    if cx.theme().colors.popover != c {
        gpui_kit::component::Theme::update(cx, |th| th.colors.popover = c);
    }
}

/// Loading spinner (gpui-component's own, animated only while it is on screen): use it only
/// for a state that is actually waiting, so an idle window schedules no frames.
pub(crate) fn spinner(color: Hsla) -> AnyElement {
    use gpui_kit::component::Sizable as _;
    gpui_kit::component::spinner::Spinner::new()
        .xsmall()
        .color(color)
        .into_any_element()
}
