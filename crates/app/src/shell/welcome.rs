//! Welcome card shown while a Space has no connection.
use super::*;

// ---- welcome -----------------------------------------------------------------------

impl Locus {
    pub(crate) fn open_form_with(
        &mut self,
        url: Option<String>,
        kind: Option<locus_core::store::DbKind>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_form(None, window, cx);
        if let Some(f) = self.form.clone() {
            f.update(cx, |f, cx| {
                if let Some(k) = kind {
                    f.set_kind(k, window, cx);
                }
                if let Some(u) = &url {
                    f.apply_url(u, window, cx);
                }
            });
        }
    }

    /// Shown instead of the panes while the Space has no connection.
    pub(crate) fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let hv = crate::shell::theme::tokens(cx).hv;
        let action = |id: &'static str,
                      icon: IconName,
                      label: String,
                      keys: String,
                      s: Shortcut,
                      cx: &mut Context<Self>| {
            h_flex()
                .id(id)
                .gap(px(scale::S5))
                .px(px(scale::S5))
                .h(px(scale::H_ROW))
                .items_center()
                .rounded(px(scale::R_MD))
                .cursor_pointer()
                .hover(|st| st.bg(hv))
                .child(Icon::new(icon).small().text_color(theme.muted_foreground))
                .child(div().flex_1().text_sm().child(label))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(keys),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.perform(s, false, window, cx);
                }))
        };
        let card = v_flex()
            .w(px(scale::W_DLG_M))
            .gap(px(scale::S6))
            .px(px(scale::S7))
            .py(px(scale::S6))
            .rounded(px(scale::R_XL))
            .bg(crate::shell::style::panel_bg(self.space().look(), cx))
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            .child(
                v_flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .pb(px(scale::S4))
                            .child(crate::shell::icons::mark(48.)),
                    )
                    .child(
                        div()
                            .text_lg()
                            .font_semibold()
                            .child(t!("welcome.title").to_string()),
                    )
                    .child(div().text_sm().text_color(theme.muted_foreground).child(
                        t!("welcome.subtitle", space = self.space().name.clone()).to_string(),
                    )),
            )
            .child(
                Input::new(&self.welcome_url)
                    .px(px(crate::shell::scale::S5))
                    .prefix(
                        Icon::new(IconName::ExternalLink)
                            .small()
                            .text_color(theme.muted_foreground),
                    ),
            )
            .child(div().h(px(1.)).bg(theme.border))
            .child(
                v_flex()
                    .gap(px(scale::S1))
                    .child(action(
                        "welcome-open",
                        IconName::FileText,
                        t!("actions.open").into_owned(),
                        crate::shell::keymap::first_key(Shortcut::OpenFile, cx),
                        Shortcut::OpenFile,
                        cx,
                    ))
                    .child(action(
                        "welcome-palette",
                        IconName::Search,
                        t!("palette.title").into_owned(),
                        crate::shell::keymap::first_key(Shortcut::Palette, cx),
                        Shortcut::Palette,
                        cx,
                    ))
                    .child(action(
                        "welcome-settings",
                        IconName::Settings,
                        t!("actions.settings").into_owned(),
                        crate::shell::keymap::first_key(Shortcut::Settings, cx),
                        Shortcut::Settings,
                        cx,
                    )),
            );
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(card)
            .into_any_element()
    }
}
