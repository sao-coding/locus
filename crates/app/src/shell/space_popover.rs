//! Space popover: name, colours, icon, delete.
//!
//! The colour part is one card: the light / dark choice beside its label, a pad where one
//! to three colour dots are dragged (x = hue, y = saturation; `−` / `+` at its foot), nine
//! preset swatches and a wave slider for the intensity. Every change previews on the whole
//! window at once; it is saved when the drag ends, and the popover closes (keeping it) on
//! Esc or a click outside.
use super::*;
use locus_core::space_theme::{self as st, Appearance, MAX_COLORS, PRESETS, SpaceColor};
use std::cell::Cell;
use std::rc::Rc;

/// Icons a Space can use instead of its colour dot (from the component set).
/// The icon a Space shows next to its name (title-bar chip, sidebar Space dots);
/// `None` when it has none or the stored key is unknown.
pub(crate) fn space_icon(key: Option<&str>) -> Option<IconName> {
    let key = key?;
    SPACE_ICONS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, i)| i.clone())
}

pub const SPACE_ICONS: &[(&str, IconName)] = &[
    ("folder", IconName::Folder),
    ("star", IconName::Star),
    ("heart", IconName::Heart),
    ("globe", IconName::Globe),
    ("drive", IconName::HardDrive),
    ("inbox", IconName::Inbox),
    ("dashboard", IconName::LayoutDashboard),
    ("book", IconName::BookOpen),
    ("bot", IconName::Bot),
    ("building", IconName::Building2),
    ("calendar", IconName::Calendar),
    ("cpu", IconName::Cpu),
];

// ---- geometry (8 px grid) ------------------------------------------------------------

/// Card width and padding; everything inside is `INNER_W` wide.
const CARD_W: f32 = 304.0;
const CARD_P: f32 = scale::S6;
const INNER_W: f32 = CARD_W - 2.0 * CARD_P;
const PAD_H: f32 = 160.0;
/// The dots' centres stay this far inside the pad's edges (the big dot's radius + 2).
const PAD_INSET: f32 = 16.0;
/// Dot diameters: the selected colour and the others.
const KNOB_ON: f32 = 26.0;
const KNOB: f32 = 20.0;
/// Alpha of the hue hint under the pad and of its grid dots.
const HUE_HINT: f32 = 0.10;
const DOT_ALPHA: f32 = 0.16;
const DOT_STEP: f32 = 16.0;
/// The `−` / `+` pill at the pad's foot.
const PILL_BTN: f32 = 32.0;
const PILL_H: f32 = 24.0;
/// Two full buttons inside its 1 px border.
const fn pill_w() -> f32 {
    2.0 * PILL_BTN + 2.0
}
const WAVE_H: f32 = 32.0;
/// The thumb's centre travels between these insets.
const WAVE_INSET: f32 = 8.0;
const THUMB_W: f32 = 8.0;
const THUMB_H: f32 = 20.0;
/// Swatch cell and its colour dot: 9 × 24 leaves an even 7 px between them.
const SWATCH: f32 = 24.0;
const SWATCH_DOT: f32 = 20.0;
/// The selection ring overhangs the 24 px cell by this much (28 px: dot, 2 px gap, 2 px ring).
const RING_OUT: f32 = 2.0;
/// Icon grid: eight equal cells across the card, a 28 px button centred in each.
const ICON_COLS: f32 = 8.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Drag {
    None,
    Pad,
    Wave,
}

pub struct SpacePopover {
    pub space: usize,
    pub at: Point<Pixels>,
    pub name: Entity<InputState>,
    /// Which colour the pad and swatches set.
    sel: usize,
    drag: Drag,
    /// Window bounds of the pad and the wave, from the last paint.
    pad: Rc<Cell<Bounds<Pixels>>>,
    wave: Rc<Cell<Bounds<Pixels>>>,
    _sub: Subscription,
}

/// A point of a box as 0..1 along its width / height between `inset`s (clamped).
fn unit_in(b: Bounds<Pixels>, p: Point<Pixels>, inset_x: f32, inset_y: f32) -> (f32, f32) {
    let w = (b.size.width.as_f32() - 2.0 * inset_x).max(1.0);
    let h = (b.size.height.as_f32() - 2.0 * inset_y).max(1.0);
    (
        ((p.x - b.origin.x).as_f32() - inset_x) / w,
        ((p.y - b.origin.y).as_f32() - inset_y) / h,
    )
}

/// Centre of a colour's dot, from the pad's top-left corner.
fn knob_center(c: SpaceColor, w: f32) -> (f32, f32) {
    let (x, y) = st::color_to_pad(c);
    (
        PAD_INSET + x * (w - 2.0 * PAD_INSET),
        PAD_INSET + y * (PAD_H - 2.0 * PAD_INSET),
    )
}

/// The dot under a point (from the pad's corner): the selected one first (it is drawn on top).
fn knob_under(colors: &[SpaceColor], sel: usize, x: f32, y: f32) -> Option<usize> {
    let order = std::iter::once(sel).chain((0..colors.len()).rev().filter(move |i| *i != sel));
    for i in order {
        let Some(c) = colors.get(i) else { continue };
        let (kx, ky) = knob_center(*c, INNER_W);
        let r = if i == sel { KNOB_ON } else { KNOB } / 2.0 + 2.0;
        if (kx - x).hypot(ky - y) <= r {
            return Some(i);
        }
    }
    None
}

impl Locus {
    pub(crate) fn open_space_popover(
        &mut self,
        si: usize,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self.spaces[si].name.clone();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, input, ev: &InputEvent, _, cx| {
                if let InputEvent::Change | InputEvent::PressEnter { .. } = ev {
                    let v = input.read(cx).value().trim().to_string();
                    if !v.is_empty()
                        && let Some(s) = this.spaces.get_mut(si)
                    {
                        s.name = v;
                        this.schedule_save(cx);
                    }
                    if let InputEvent::PressEnter { .. } = ev {
                        this.close_space_popover(cx);
                    }
                    cx.notify();
                }
            },
        );
        input.update(cx, |s, cx| s.focus(window, cx));
        self.space_pop = Some(SpacePopover {
            space: si,
            at,
            name: input,
            sel: 0,
            drag: Drag::None,
            pad: Rc::default(),
            wave: Rc::default(),
            _sub: sub,
        });
        cx.notify();
    }

    /// Closes the popover, keeping what it changed (it is already applied).
    pub(crate) fn close_space_popover(&mut self, cx: &mut Context<Self>) {
        if self.space_pop.take().is_some() {
            self.schedule_save(cx);
            cx.notify();
        }
    }

    /// The light / dark the active Space pins, applied to the app theme when it changes
    /// (switching Spaces or picking it in the popover).
    pub(crate) fn sync_space_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use std::sync::atomic::Ordering;
        let code = match self.space().theme.appearance {
            Appearance::Auto => 0,
            Appearance::Light => 1,
            Appearance::Dark => 2,
        };
        if crate::shell::style::SPACE_APPEARANCE.swap(code, Ordering::Relaxed) != code {
            cx.defer_in(window, |_, window, cx| apply_theme(window, cx));
        }
    }

    fn space_theme_mut(&mut self, si: usize) -> Option<&mut st::SpaceTheme> {
        self.spaces.get_mut(si).map(|s| &mut s.theme)
    }

    /// The pad was pressed at `pos` (window coordinates): the dot there becomes the selected
    /// one (else the selected dot jumps there) and follows the drag.
    fn pad_press(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(pop) = self.space_pop.as_mut() else {
            return;
        };
        let si = pop.space;
        let b = pop.pad.get();
        let (x, y) = ((pos.x - b.origin.x).as_f32(), (pos.y - b.origin.y).as_f32());
        if let Some(colors) = self.spaces.get(si).map(|s| s.theme.colors.clone())
            && let Some(i) = knob_under(&colors, pop.sel, x, y)
        {
            pop.sel = i;
        }
        pop.drag = Drag::Pad;
        self.pad_drag_to(pos, cx);
    }

    fn pad_drag_to(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(pop) = self.space_pop.as_ref() else {
            return;
        };
        let (si, sel) = (pop.space, pop.sel);
        let (x, y) = unit_in(pop.pad.get(), pos, PAD_INSET, PAD_INSET);
        let c = st::pad_to_color(x, y);
        if let Some(t) = self.space_theme_mut(si)
            && let Some(slot) = t.colors.get_mut(sel)
            && *slot != c
        {
            *slot = c;
            cx.notify();
        }
    }

    fn wave_drag_to(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(pop) = self.space_pop.as_ref() else {
            return;
        };
        let si = pop.space;
        let (x, _) = unit_in(pop.wave.get(), pos, WAVE_INSET, 0.0);
        let v = (x.clamp(0.0, 1.0) * 100.0).round() / 100.0;
        if let Some(t) = self.space_theme_mut(si)
            && t.intensity != v
        {
            t.intensity = v;
            cx.notify();
        }
    }

    /// Any mouse move / release while the popover is open (registered at paint, so a drag
    /// keeps going outside the pad).
    fn space_pop_mouse(&mut self, pos: Point<Pixels>, released: bool, cx: &mut Context<Self>) {
        let Some(drag) = self.space_pop.as_ref().map(|p| p.drag) else {
            return;
        };
        if released {
            if drag != Drag::None {
                if let Some(p) = self.space_pop.as_mut() {
                    p.drag = Drag::None;
                }
                self.schedule_save(cx);
            }
            return;
        }
        match drag {
            Drag::Pad => self.pad_drag_to(pos, cx),
            Drag::Wave => self.wave_drag_to(pos, cx),
            Drag::None => {}
        }
    }

    pub(crate) fn render_space_popover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pop = self.space_pop.as_ref()?;
        let si = pop.space;
        self.spaces.get(si)?;
        let theme = cx.theme().clone();
        let t = crate::shell::theme::tokens(cx);
        let label = |text: String| {
            div()
                .text_size(px(scale::TX_XS))
                .font_semibold()
                .text_color(t.tx3)
                .child(text.to_uppercase())
        };
        let can_delete = self.spaces.len() > 1;
        let body = v_flex()
            .id("space-popover")
            .w(px(CARD_W))
            .p(px(CARD_P))
            .gap(px(scale::S6))
            .rounded(px(scale::R_XL))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_space_popover(cx)))
            .child(
                v_flex()
                    .gap(px(scale::S4))
                    .child(label(t!("space.name").into_owned()))
                    .child(Input::new(&pop.name).px(px(scale::S5))),
            )
            .child(
                v_flex()
                    .gap(px(scale::S4))
                    .child(
                        h_flex()
                            .h(px(scale::H_CONTROL))
                            .justify_between()
                            .items_center()
                            .child(label(t!("space.color").into_owned()))
                            .child(self.space_appearance(si, cx)),
                    )
                    .child(self.space_pad(pop, cx))
                    .child(self.space_swatches(pop, cx))
                    .child(self.space_wave(pop, cx)),
            )
            .child(
                v_flex()
                    .gap(px(scale::S4))
                    .child(label(t!("space.icon").into_owned()))
                    .child(self.space_icons(si, cx)),
            )
            .when(can_delete, |v| {
                v.child(div().h(px(1.)).bg(theme.border)).child(
                    Button::new("space-delete")
                        .ghost()
                        .w_full()
                        .icon(trash_icon().text_color(theme.danger))
                        .label(t!("space.delete_menu").to_string())
                        .text_color(theme.danger)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.close_space_popover(cx);
                            this.confirm_delete_space(si, window, cx);
                        })),
                )
            });
        let body = if self.reduce_motion {
            body.into_any_element()
        } else {
            body.with_animation(
                "space-pop-in",
                Animation::new(Duration::from_millis(140)).with_easing(ease_out_quint()),
                |d, t| d.opacity(t).mt(px((1.0 - t) * -6.0)),
            )
            .into_any_element()
        };
        Some(
            deferred(
                anchored()
                    .position(pop.at)
                    .snap_to_window_with_margin(px(8.))
                    .child(body),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    /// Follow the app / always light / always dark: a three-icon segmented control.
    fn space_appearance(&self, si: usize, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let cur = self.spaces[si].theme.appearance;
        let items = [
            (Appearance::Auto, "monitor", "space.appearance_auto"),
            (Appearance::Light, "sun", "space.appearance_light"),
            (Appearance::Dark, "moon", "space.appearance_dark"),
        ];
        let mut row = h_flex()
            .h(px(scale::H_CONTROL))
            .p(px(scale::S1))
            .gap(px(scale::S1))
            .rounded(px(scale::R_SM))
            .bg(t.hv);
        for (i, (a, icon, key)) in items.into_iter().enumerate() {
            let on = cur == a;
            row = row.child(
                div()
                    .id(("space-appearance", i))
                    .w(px(scale::S8))
                    .h(px(scale::H_CONTROL - 2.0 * scale::S1))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(scale::R_SM - scale::S1))
                    .cursor_pointer()
                    .text_color(if on { t.tx } else { t.tx3 })
                    .when(on, |d| d.bg(t.on))
                    .when(!on, |d| d.hover(|h| h.bg(t.hv).text_color(t.tx)))
                    .tooltip(tip(t!(key).into_owned()))
                    .child(crate::shell::icons::icon(icon).size(px(14.)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(th) = this.space_theme_mut(si) {
                            th.appearance = a;
                        }
                        this.schedule_save(cx);
                        cx.notify();
                    })),
            );
        }
        row
    }

    /// The hue / saturation pad with the colour dots and the `−` / `+` pill.
    fn space_pad(&self, pop: &SpacePopover, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = crate::shell::theme::tokens(cx);
        let th = &self.spaces[pop.space].theme;
        let n = th.colors.len();
        // A calm surface: the card's ink at a few per cent over the popover (dark in the dark
        // theme, light in the light one), a faint hue hint so position stays discoverable,
        // and the dotted grid. Only the dots carry real colour.
        let surface = mix(t.pop, t.tx, if t.dark { 0.06 } else { 0.035 });
        let hue = |h: f32| hsla(h, 0.9, 0.55, HUE_HINT);
        let span = INNER_W - 2.0 * PAD_INSET;
        let mut spectrum = h_flex()
            .absolute()
            .inset_0()
            .child(div().w(px(PAD_INSET)).h_full().bg(hue(0.0)));
        for k in 0..6 {
            let (a, b) = (k as f32 / 6.0, (k + 1) as f32 / 6.0);
            spectrum = spectrum.child(div().w(px(span / 6.0)).h_full().bg(linear_gradient(
                90.,
                linear_color_stop(hue(a), 0.),
                linear_color_stop(hue(b), 1.),
            )));
        }
        spectrum = spectrum.child(div().flex_1().h_full().bg(hue(0.0)));
        // Saturation: the hint fades out toward the bottom, between the dots' insets.
        let mut clear = surface;
        clear.a = 0.0;
        let (top, bottom) = (PAD_INSET / PAD_H, 1.0 - PAD_INSET / PAD_H);
        let veil = div().absolute().inset_0().bg(linear_gradient(
            180.,
            linear_color_stop(clear, top),
            linear_color_stop(surface, bottom),
        ));
        // Dotted grid and the bounds the drag maps against.
        let ink = t.tx;
        let bounds = pop.pad.clone();
        let ent = cx.entity().downgrade();
        let dots = canvas(
            move |b, _, _| bounds.set(b),
            move |b, _, window, _| {
                let mut c = ink;
                c.a = DOT_ALPHA;
                let (w, h) = (b.size.width.as_f32(), b.size.height.as_f32());
                let mut y = DOT_STEP / 2.0;
                while y < h {
                    let mut x = DOT_STEP / 2.0 + (w % DOT_STEP) / 2.0;
                    while x < w {
                        let o = b.origin + point(px(x - 1.0), px(y - 1.0));
                        window.paint_quad(
                            fill(Bounds::new(o, size(px(2.), px(2.))), c).corner_radii(px(1.)),
                        );
                        x += DOT_STEP;
                    }
                    y += DOT_STEP;
                }
                // The drag follows the pointer anywhere in the window.
                let e = ent.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let held = ev.pressed_button == Some(MouseButton::Left);
                        let _ =
                            e.update(cx, |this, cx| this.space_pop_mouse(ev.position, !held, cx));
                    }
                });
                let e = ent.clone();
                window.on_mouse_event(move |ev: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && ev.button == MouseButton::Left {
                        let _ =
                            e.update(cx, |this, cx| this.space_pop_mouse(ev.position, true, cx));
                    }
                });
            },
        )
        .absolute()
        .inset_0();
        let mut pad = div()
            .id("space-pad")
            .relative()
            .w(px(INNER_W))
            .h(px(PAD_H))
            .rounded(px(scale::R_LG))
            .overflow_hidden()
            .cursor(CursorStyle::Crosshair)
            .bg(surface)
            .child(spectrum)
            .child(veil)
            .child(dots)
            // Hairline as an overlay, so it takes no room from the mapping.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(scale::R_LG))
                    .border_1()
                    .border_color(t.ln),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| this.pad_press(ev.position, cx)),
            );
        // Dots: the others first, the selected one on top.
        let order = (0..n).filter(|i| *i != pop.sel).chain([pop.sel.min(n - 1)]);
        for i in order {
            let c = th.colors[i];
            let d = if i == pop.sel { KNOB_ON } else { KNOB };
            let (x, y) = knob_center(c, INNER_W);
            pad = pad.child(
                div()
                    .absolute()
                    .left(px(x - d / 2.0))
                    .top(px(y - d / 2.0))
                    .size(px(d))
                    .rounded_full()
                    .bg(crate::shell::style::swatch_color(c, cx))
                    .border(px(2.))
                    .border_color(gpui_kit::white())
                    .shadow(vec![BoxShadow {
                        color: hsla(0., 0., 0., 0.22),
                        offset: point(px(0.), px(1.)),
                        blur_radius: px(6.),
                        spread_radius: px(0.),
                        inset: false,
                    }]),
            );
        }
        pad.child(self.space_pill(pop.space, n, cx))
    }

    /// `−` / `+` at the pad's foot, centred.
    fn space_pill(&self, si: usize, n: usize, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let mut bg = t.pop;
        bg.a = 0.92;
        let w = pill_w();
        let btn = |id: &'static str, icon: &'static str, on: bool, key: &'static str| {
            div()
                .id(id)
                // Equal halves of the inside of the border (two fixed 32s would shrink unevenly).
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(if on { t.tx } else { t.tx5 })
                .when(on, |d| d.cursor_pointer().hover(|h| h.bg(t.hv)))
                .tooltip(tip(t!(key).into_owned()))
                .child(crate::shell::icons::icon(icon).size(px(14.)))
        };
        h_flex()
            .absolute()
            .left(px((INNER_W - w) / 2.0))
            .bottom(px(scale::S4))
            .w(px(w))
            .h(px(PILL_H))
            .rounded_full()
            .overflow_hidden()
            .bg(bg)
            .border_1()
            .border_color(t.ln)
            // The pill sits on the pad: a press here is not a pick.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                btn("space-color-remove", "minus", n > 1, "space.remove_color").on_click(
                    cx.listener(move |this, _, _, cx| {
                        let removed = this.space_theme_mut(si).is_some_and(|t| t.remove_color());
                        if removed {
                            if let Some(p) = this.space_pop.as_mut() {
                                p.sel = p.sel.min(n - 2);
                            }
                            this.schedule_save(cx);
                            cx.notify();
                        }
                    }),
                ),
            )
            .child(
                btn("space-color-add", "plus", n < MAX_COLORS, "space.add_color").on_click(
                    cx.listener(move |this, _, _, cx| {
                        if let Some(i) = this.space_theme_mut(si).and_then(|t| t.add_color()) {
                            if let Some(p) = this.space_pop.as_mut() {
                                p.sel = i;
                            }
                            this.schedule_save(cx);
                            cx.notify();
                        }
                    }),
                ),
            )
    }

    /// Nine preset swatches, spread across the card; a click sets the selected colour.
    fn space_swatches(&self, pop: &SpacePopover, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let si = pop.space;
        let cur = self.spaces[si].theme.colors.get(pop.sel).copied();
        let clear = hsla(0., 0., 0., 0.);
        let gap = (INNER_W - PRESETS.len() as f32 * SWATCH) / (PRESETS.len() - 1) as f32;
        let mut row = h_flex().w(px(INNER_W)).gap(px(gap)).items_center();
        for (ci, c) in PRESETS.iter().copied().enumerate() {
            let selected = cur == Some(c);
            let key = format!("space.colors.p{ci}");
            let col = crate::shell::style::swatch_color(c, cx);
            row = row.child(
                div()
                    .id(("swatch", ci))
                    .size(px(SWATCH))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .relative()
                    .cursor_pointer()
                    .tooltip(tip(t!(&key).into_owned()))
                    .child(div().size(px(SWATCH_DOT)).rounded_full().bg(col))
                    // Selected (or hovered): a ring 2 px outside the dot, with a 2 px gap; it
                    // overhangs the cell so the row keeps its spacing.
                    .child(
                        div()
                            .absolute()
                            .left(px(-RING_OUT))
                            .top(px(-RING_OUT))
                            .size(px(SWATCH + 2.0 * RING_OUT))
                            .rounded_full()
                            .border_2()
                            .border_color(if selected { col } else { clear })
                            .when(!selected, |d| d.hover(|d| d.border_color(t.ln2))),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let sel = this.space_pop.as_ref().map_or(0, |p| p.sel);
                        if let Some(slot) =
                            this.space_theme_mut(si).and_then(|t| t.colors.get_mut(sel))
                        {
                            *slot = c;
                            this.schedule_save(cx);
                            cx.notify();
                        }
                    })),
            );
        }
        row
    }

    /// Intensity: a sine wave that grows with the value, solid up to the thumb.
    fn space_wave(&self, pop: &SpacePopover, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = crate::shell::theme::tokens(cx);
        let v = self.spaces[pop.space].theme.intensity;
        let (ink, track) = (t.tx2, t.hv);
        let bounds = pop.wave.clone();
        let wave = canvas(
            move |b, _, _| bounds.set(b),
            move |b, _, window, _| {
                let (w, h) = (b.size.width.as_f32(), b.size.height.as_f32());
                let mid = h / 2.0;
                window.paint_quad(
                    fill(
                        Bounds::new(
                            b.origin + point(px(0.), px(mid - 6.0)),
                            size(px(w), px(12.)),
                        ),
                        track,
                    )
                    .corner_radii(px(6.)),
                );
                let span = w - 2.0 * WAVE_INSET;
                let at = WAVE_INSET + span * v;
                let amp = 1.5 + 5.0 * v;
                let y = |x: f32| {
                    mid - amp * ((x - WAVE_INSET) / span * std::f32::consts::TAU * 6.0).sin()
                };
                let path = |from: f32, to: f32| {
                    let mut p = PathBuilder::stroke(px(2.));
                    let mut x = from;
                    p.move_to(b.origin + point(px(x), px(y(x))));
                    while x < to {
                        x = (x + 2.0).min(to);
                        p.line_to(b.origin + point(px(x), px(y(x))));
                    }
                    p.build().ok()
                };
                let mut faint = ink;
                faint.a *= 0.35;
                if at < w - WAVE_INSET
                    && let Some(p) = path(at, w - WAVE_INSET)
                {
                    window.paint_path(p, faint);
                }
                if at > WAVE_INSET
                    && let Some(p) = path(WAVE_INSET, at)
                {
                    window.paint_path(p, ink);
                }
            },
        )
        .absolute()
        .inset_0();
        let x = WAVE_INSET + (INNER_W - 2.0 * WAVE_INSET) * v;
        div()
            .id("space-wave")
            .relative()
            .w(px(INNER_W))
            .h(px(WAVE_H))
            .cursor_pointer()
            .tooltip(tip(format!(
                "{} {}%",
                t!("space.intensity_tip"),
                (v * 100.0).round()
            )))
            .child(wave)
            .child(
                div()
                    .absolute()
                    .left(px(x - THUMB_W / 2.0))
                    .top(px((WAVE_H - THUMB_H) / 2.0))
                    .w(px(THUMB_W))
                    .h(px(THUMB_H))
                    .rounded_full()
                    .bg(gpui_kit::white())
                    .border_1()
                    .border_color(hsla(0., 0., 0., 0.14))
                    .shadow(vec![BoxShadow {
                        color: hsla(0., 0., 0., 0.18),
                        offset: point(px(0.), px(1.)),
                        blur_radius: px(3.),
                        spread_radius: px(0.),
                        inset: false,
                    }]),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                    if let Some(p) = this.space_pop.as_mut() {
                        p.drag = Drag::Wave;
                    }
                    this.wave_drag_to(ev.position, cx);
                }),
            )
    }

    /// Icon buttons of the Space popover: the colour dot, then the icons, on an even grid.
    fn space_icons(&self, si: usize, cx: &mut Context<Self>) -> Div {
        let s = &self.spaces[si];
        let theme = cx.theme().clone();
        let accent = s.accent(cx);
        let button = |id: ElementId, selected: bool, glyph: AnyElement| {
            div()
                .w(px(INNER_W / ICON_COLS))
                .flex()
                .justify_center()
                .child(
                    div()
                        .id(id)
                        .size(px(scale::H_CONTROL))
                        .rounded_md()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|st| st.bg(theme.muted))
                        .when(selected, |d| {
                            d.bg(theme.muted).border_1().border_color(theme.border)
                        })
                        .child(glyph),
                )
        };
        let mut icons = h_flex().w(px(INNER_W)).flex_wrap().gap_y(px(scale::S2));
        icons = icons.child(
            button(
                "icon-none".into(),
                s.icon.is_none(),
                div()
                    .size(px(10.))
                    .rounded_full()
                    .bg(accent)
                    .into_any_element(),
            )
            .id("icon-none-cell")
            .tooltip(tip(t!("space.icon_none").into_owned()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.spaces[si].icon = None;
                this.schedule_save(cx);
                cx.notify();
            })),
        );
        for (ii, (key, icon)) in SPACE_ICONS.iter().enumerate() {
            let selected = s.icon.as_deref() == Some(*key);
            let k = key.to_string();
            icons = icons.child(
                button(
                    ("space-icon", ii).into(),
                    selected,
                    Icon::new(icon.clone())
                        .small()
                        .text_color(accent)
                        .into_any_element(),
                )
                .id(("space-icon-cell", ii))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.spaces[si].icon = Some(k.clone());
                    this.schedule_save(cx);
                    cx.notify();
                })),
            );
        }
        icons
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `super::*` brings GPUI's `test` macro; plain `#[test]` must stay Rust's.
    use std::prelude::v1::test;

    #[test]
    fn knob_is_centred_on_the_picked_point() {
        // A press at a pad point gives a colour whose dot centre is that point.
        let b = Bounds::new(point(px(100.), px(50.)), size(px(INNER_W), px(PAD_H)));
        let p = point(px(100. + 90.), px(50. + 70.));
        let (x, y) = unit_in(b, p, PAD_INSET, PAD_INSET);
        let (kx, ky) = knob_center(st::pad_to_color(x, y), INNER_W);
        assert!(
            (kx - 90.).abs() < 0.01 && (ky - 70.).abs() < 0.01,
            "{kx},{ky}"
        );
    }

    #[test]
    fn press_picks_the_dot_under_it() {
        let colors = [SpaceColor::new(0.1, 0.5), SpaceColor::new(0.6, 0.9)];
        let (x, y) = knob_center(colors[1], INNER_W);
        assert_eq!(knob_under(&colors, 0, x + 3., y), Some(1));
        assert_eq!(knob_under(&colors, 0, x + 40., y + 40.), None);
    }

    #[test]
    fn grid_is_even() {
        // Pill and thumb centred, card on the 8 px grid.
        assert_eq!(CARD_W % 8.0, 0.0);
        assert_eq!(INNER_W % 8.0, 0.0);
        // The pill sits on whole pixels and each half is a full button.
        assert_eq!((INNER_W - pill_w()) % 2.0, 0.0);
        assert_eq!((pill_w() - 2.0) / 2.0, PILL_BTN);
        assert_eq!((WAVE_H - THUMB_H) / 2.0, 6.0);
    }
}
