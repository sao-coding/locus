//! Title bar (36 px): sidebar toggle, logo, Space chip, "Space — tab title",
//! window caption buttons (drawn by the component's `TitleBar`).
use super::*;

impl Locus {
    pub(crate) fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        let s = self.space();
        let col = s.accent(cx);
        let tab = self.active_tab(cx).read(cx).display_title(cx);
        let title = format!("{} — {}", s.name, tab);
        let mut chip_bg = col;
        chip_bg.a = 0.14;
        TitleBar::new()
            .h(px(scale::H_TITLEBAR))
            .pl(px(scale::S4))
            .bg(gpui_kit::transparent_black())
            .border_b_0()
            .child(
                h_flex()
                    .gap(px(scale::S4))
                    .items_center()
                    .min_w_0()
                    .child(crate::shell::icons::mark(16.))
                    .child(self.space_chip(col, chip_bg, cx))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(scale::TX_BASE))
                            .text_color(t.tx2)
                            .child(title),
                    ),
            )
    }
}

impl Locus {
    /// The Space chip: click for the list of Spaces (switch, new, options).
    fn space_chip(&self, col: Hsla, chip_bg: Hsla, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity().downgrade();
        let spaces: Vec<(usize, String, Option<String>, Hsla)> = self
            .spaces
            .iter()
            .enumerate()
            .map(|(i, s)| (i, s.name.clone(), s.icon.clone(), s.accent(cx)))
            .collect();
        let active = self.active_space;
        let icon = crate::shell::space_popover::space_icon(self.space().icon.as_deref());
        let mut button = Button::new("space-chip");
        if let Some(i) = icon {
            button = button.icon(Icon::new(i).text_color(col));
        }
        let button = button
            .ghost()
            .h(px(scale::H_BADGE))
            .px(px(scale::S4))
            .rounded_full()
            .bg(chip_bg)
            .text_color(col)
            .text_size(px(scale::TX_XS))
            .font_weight(FontWeight::MEDIUM)
            .label(self.space().name.clone())
            .tooltip(t!("space.menu").to_string())
            .dropdown_menu(move |mut menu, _, _| {
                for (i, name, key, c) in spaces.iter().cloned() {
                    let me = me.clone();
                    let icon = match crate::shell::space_popover::space_icon(key.as_deref()) {
                        Some(n) => Icon::new(n),
                        None => crate::shell::icons::icon("space-dot"),
                    }
                    .text_color(c);
                    menu = menu.item(crate::shell::menu::choice(
                        name,
                        icon,
                        i == active,
                        move |window, cx| {
                            let _ = me.update(cx, |this, cx| this.switch_space(i, window, cx));
                        },
                    ));
                }
                let (m1, m2) = (me.clone(), me.clone());
                menu.separator()
                    .item(crate::shell::menu::item(
                        t!("space.new").into_owned(),
                        Some("plus"),
                        move |window, cx| {
                            let _ = m1.update(cx, |this, cx| this.new_space(window, cx));
                        },
                    ))
                    .item(crate::shell::menu::item(
                        t!("space.menu").into_owned(),
                        Some("gear"),
                        move |window, cx| {
                            let at = window.mouse_position();
                            let _ = m2.update(cx, |this, cx| {
                                this.open_space_popover(this.active_space, at, window, cx)
                            });
                        },
                    ))
            });
        div().flex_none().occlude().child(button)
    }
}

impl Locus {
    /// Show / hide the sidebar (Ctrl+B). Lives in each page header and stays put, so the
    /// same button that hides the sidebar brings it back.
    pub(crate) fn sidebar_toggle(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let on = self.sidebar != SidebarMode::Hidden;
        ib(
            "toggle-sidebar",
            crate::shell::icons::icon("sidebar"),
            scale::H_CONTROL,
            on,
            cx,
        )
        .tooltip(tip(t!("shell.toggle_sidebar").into_owned()))
        .on_click(cx.listener(|this, _, _, cx| {
            let m = if this.sidebar == SidebarMode::Hidden {
                SidebarMode::Full
            } else {
                SidebarMode::Hidden
            };
            this.set_sidebar_mode(m, cx);
        }))
    }
}
