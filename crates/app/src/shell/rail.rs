//! Right rail (W_RAIL, always visible): one button per registered right panel
//! (see `crate::panels`), then History and Settings, which open as tabs.
use super::*;
use crate::panels::PANELS;

impl Locus {
    pub(crate) fn set_right_panel(&mut self, p: Option<RightPanel>, cx: &mut Context<Self>) {
        self.right_panel = p;
        self.schedule_save(cx);
        cx.notify();
    }

    pub(crate) fn toggle_right_panel(&mut self, p: RightPanel, cx: &mut Context<Self>) {
        let next = if self.right_panel == Some(p) {
            None
        } else {
            Some(p)
        };
        self.set_right_panel(next, cx);
    }

    pub(crate) fn render_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let rp = self.right_panel;
        let page = self.active_tab(cx).read(cx).pageslot.kind;
        let (on_history, on_settings) = (
            page == Some(locus_core::spaces::TabPage::History),
            page == Some(locus_core::spaces::TabPage::Settings),
        );
        let panel_buttons = PANELS.iter().map(|spec| {
            let panel = spec.panel;
            ib(
                spec.rail_id,
                crate::shell::icons::icon(spec.icon),
                scale::H_CONTROL,
                rp == Some(panel),
                cx,
            )
            .tooltip(tip(t!(spec.tip).into_owned()))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_right_panel(panel, cx)))
        });
        v_flex()
            .id("rail")
            .bg(crate::shell::style::panel_bg(self.space().look(), cx))
            .flex_none()
            .w(px(scale::W_RAIL))
            .h_full()
            .py(px(scale::S4))
            .gap(px(scale::S2))
            .items_center()
            .border_l_1()
            .border_color(t.ln)
            .children(panel_buttons)
            .child(
                ib(
                    "rail-history",
                    crate::shell::icons::icon("history"),
                    scale::H_CONTROL,
                    on_history,
                    cx,
                )
                .tooltip(tip(t!("shell.rail_history").into_owned()))
                .on_click(cx.listener(|this, _, window, cx| this.open_history(window, cx))),
            )
            .child(div().flex_1())
            .child(
                ib(
                    "rail-settings",
                    crate::shell::icons::icon("gear"),
                    scale::H_CONTROL,
                    on_settings,
                    cx,
                )
                .tooltip(tip(t!("shell.settings").into_owned()))
                .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx))),
            )
            .into_any_element()
    }
}
