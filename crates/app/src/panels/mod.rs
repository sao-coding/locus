//! Right-panel registry: the panels the right rail opens next to the panes.
//!
//! This is the extension slot for side panels (a future Git or AI / MCP
//! panel, see `docs/extensions.md`). To add one:
//! 1. add a variant to `RightPanel` (`locus_core::spaces`; its name is saved in
//!    `spaces.json`, an unknown name loads as `RightPanel::Unknown`),
//! 2. add a `PanelSpec` to `PANELS` (rail button and title),
//! 3. add its content to `Locus::panel_content`.
use crate::shell::{scale, *};

/// One right panel: its rail button and its title.
pub(crate) struct PanelSpec {
    pub panel: RightPanel,
    /// Element id of the rail button.
    pub rail_id: &'static str,
    /// Icon name (`shell/icons.rs`).
    pub icon: &'static str,
    /// Locale key of the rail tooltip.
    pub tip: &'static str,
    /// Locale key of the panel title.
    pub title: &'static str,
}

/// Every panel, in rail order.
pub(crate) const PANELS: &[PanelSpec] = &[PanelSpec {
    panel: RightPanel::Schema,
    rail_id: "rail-schema",
    icon: "db",
    tip: "shell.rail_schema",
    title: "shell.panel_schema",
}];

/// True when `p` is a registered panel (spaces.json may name one this build lacks).
pub(crate) fn is_registered(p: RightPanel) -> bool {
    PANELS.iter().any(|s| s.panel == p)
}

/// What a panel puts into the shared frame: header buttons and the rows below the header.
struct PanelContent {
    actions: Vec<AnyElement>,
    rows: Vec<AnyElement>,
}

impl Locus {
    /// The open right panel (W_RIGHT_PANEL), or `None` when it is closed.
    pub(crate) fn render_right_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let spec = PANELS.iter().find(|s| Some(s.panel) == self.right_panel)?;
        let content = self.panel_content(spec.panel, cx)?;
        let t = crate::shell::theme::tokens(cx);
        let header = h_flex()
            .flex_none()
            .h(px(scale::H_TAB))
            .pl(px(scale::S6))
            .pr(px(scale::S5))
            .items_center()
            .text_size(px(scale::TX_XS))
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.tx3)
            .child(div().flex_1().child(t!(spec.title).to_uppercase()))
            .children(content.actions);
        Some(
            v_flex()
                .id("right-panel")
                .bg(crate::shell::style::panel_bg(self.space().hue(), cx))
                .flex_none()
                .relative()
                .w(px(self.right_width))
                .h_full()
                .border_l_1()
                .border_color(t.ln)
                .child(
                    div()
                        .id("right-edge")
                        .absolute()
                        .left(px(-3.))
                        .top_0()
                        .bottom_0()
                        .w(px(6.))
                        .cursor(CursorStyle::ResizeLeftRight)
                        .hover(|s| s.bg(t.ln2))
                        .when(self.resizing_right, |d| d.bg(t.ln2))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.resizing_right = true;
                                cx.notify();
                            }),
                        ),
                )
                .child(header)
                .children(content.rows)
                .into_any_element(),
        )
    }

    fn panel_content(&self, panel: RightPanel, cx: &mut Context<Self>) -> Option<PanelContent> {
        match panel {
            RightPanel::Schema => Some(self.schema_panel(cx)),
            RightPanel::History | RightPanel::Unknown => None,
        }
    }

    /// The database-object tree with a search box.
    fn schema_panel(&self, cx: &mut Context<Self>) -> PanelContent {
        let t = crate::shell::theme::tokens(cx);
        let refresh = ib(
            "schema-refresh",
            crate::shell::icons::icon("refresh"),
            scale::H_COMPACT,
            false,
            cx,
        )
        .tooltip(tip(t!("objects.refresh").into_owned()))
        .on_click(cx.listener(|this, _, _, cx| this.objects.update(cx, |o, cx| o.refresh(cx))))
        .into_any_element();
        let search = div()
            .flex_none()
            .px(px(scale::S5))
            .pb(px(scale::S4))
            .child(
                Input::new(&self.schema_search)
                    .px(px(crate::shell::scale::S5))
                    .prefix(
                        crate::shell::icons::icon("search")
                            .size(px(14.))
                            .text_color(t.tx3),
                    ),
            )
            .into_any_element();
        let tree = div()
            .flex_1()
            .min_h_0()
            .px(px(scale::S4))
            .pb(px(scale::S4))
            .child(self.objects.clone())
            .into_any_element();
        PanelContent {
            actions: vec![refresh],
            rows: vec![search, tree],
        }
    }
}
