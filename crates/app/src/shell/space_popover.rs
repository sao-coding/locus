//! Space popover: name, colour, icon, delete.
use super::*;

// ---- Space popover ---------------------------------------------------------------

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

pub struct SpacePopover {
    pub space: usize,
    pub at: Point<Pixels>,
    pub name: Entity<InputState>,
    /// Custom colour (any hue); the preset swatches stay for one-click choices.
    pub picker: Entity<gpui_kit::component::color_picker::ColorPickerState>,
    _sub: Subscription,
    _sub_picker: Subscription,
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
                        this.space_pop = None;
                    }
                    cx.notify();
                }
            },
        );
        input.update(cx, |s, cx| s.focus(window, cx));
        use gpui_kit::component::color_picker::{ColorPickerEvent, ColorPickerState};
        let cur = space_color(self.spaces[si].hue(), cx);
        let picker = cx.new(|cx| ColorPickerState::new(window, cx).default_value(cur));
        let sub_picker = cx.subscribe(&picker, move |this, _, ev: &ColorPickerEvent, cx| {
            let ColorPickerEvent::Change(c) = ev;
            if let (Some(c), Some(s)) = (c, this.spaces.get_mut(si)) {
                s.custom_hue = Some(c.h);
                this.schedule_save(cx);
                cx.notify();
            }
        });
        self.space_pop = Some(SpacePopover {
            space: si,
            at,
            name: input,
            picker,
            _sub: sub,
            _sub_picker: sub_picker,
        });
        cx.notify();
    }

    pub(crate) fn render_space_popover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pop = self.space_pop.as_ref()?;
        let si = pop.space;
        self.spaces.get(si)?;
        let theme = cx.theme().clone();
        let label = |text: String| {
            div()
                .text_xs()
                .font_semibold()
                .text_color(theme.muted_foreground)
                .child(text.to_uppercase())
        };
        let swatches = self.space_swatches(pop, si, cx);
        let icons = self.space_icons(si, cx);
        let can_delete = self.spaces.len() > 1;
        let body = v_flex()
            .id("space-popover")
            .w(px(280.))
            .p_3()
            .gap_3()
            .rounded_lg()
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.space_pop = None;
                cx.notify();
            }))
            .child(
                v_flex()
                    .gap_1()
                    .child(label(t!("space.name").into_owned()))
                    .child(Input::new(&pop.name).px(px(crate::shell::scale::S5))),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(label(t!("space.color").into_owned()))
                    .child(swatches),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(label(t!("space.icon").into_owned()))
                    .child(icons),
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
                            this.space_pop = None;
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

    /// Colour swatches of the Space popover and the custom colour picker.
    fn space_swatches(&self, pop: &SpacePopover, si: usize, cx: &mut Context<Self>) -> Div {
        let s = &self.spaces[si];
        let t = crate::shell::theme::tokens(cx);
        let mut swatches = h_flex().gap(px(scale::S3)).flex_wrap().items_center();
        for (ci, hue) in SPACE_HUES.iter().enumerate() {
            let col = space_color(*hue, cx);
            let selected = s.custom_hue.is_none() && s.color == ci;
            let key = format!("space.colors.c{ci}");
            swatches = swatches.child(
                div()
                    .id(("swatch", ci))
                    .size(px(scale::H_ROW))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .border_2()
                    .border_color(if selected { t.tx } else { hsla(0., 0., 0., 0.) })
                    .hover(|d| d.border_color(t.ln2))
                    .tooltip(tip(t!(&key).into_owned()))
                    .child(
                        div()
                            .size(px(scale::S7 - scale::S1))
                            .rounded_full()
                            .bg(col)
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
                        this.spaces[si].color = ci;
                        this.spaces[si].custom_hue = None;
                        this.schedule_save(cx);
                        cx.notify();
                    })),
            );
        }
        swatches = swatches.child(gpui_kit::component::color_picker::ColorPicker::new(
            &pop.picker,
        ));
        swatches
    }

    /// Icon buttons of the Space popover.
    fn space_icons(&self, si: usize, cx: &mut Context<Self>) -> Div {
        let s = &self.spaces[si];
        let theme = cx.theme().clone();
        let mut icons = h_flex().gap_1().flex_wrap();
        let none_selected = s.icon.is_none();
        icons = icons.child(
            div()
                .id("icon-none")
                .size(px(scale::H_CONTROL))
                .rounded_md()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|st| st.bg(theme.muted))
                .when(none_selected, |d| {
                    d.bg(theme.muted).border_1().border_color(theme.border)
                })
                .tooltip(tip(t!("space.icon_none").into_owned()))
                .child(
                    div()
                        .size(px(10.))
                        .rounded_full()
                        .bg(space_color(s.hue(), cx)),
                )
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
                div()
                    .id(("space-icon", ii))
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
                    .child(
                        Icon::new(icon.clone())
                            .small()
                            .text_color(space_color(s.hue(), cx)),
                    )
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
