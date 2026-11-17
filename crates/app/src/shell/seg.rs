//! Segmented control (the mock's `.seg`): a `hv` container with radius 6 and 2 px padding,
//! items 28 px high, the selected one on `on` + `tx`. Used for every "pick one" choice
//! (Settings, History filter, connection environment) instead of primary buttons.
use super::scale::*;
use super::*;
use std::rc::Rc;

pub(crate) type SegClick = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

pub(crate) struct SegItem {
    pub id: SharedString,
    pub label: String,
    pub on: bool,
    /// Text colour of the selected item (e.g. red for Production); `None` = `tx`.
    pub tone: Option<Hsla>,
    pub click: SegClick,
}

impl SegItem {
    pub fn new(
        id: impl Into<SharedString>,
        label: impl Into<String>,
        on: bool,
        click: SegClick,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            on,
            tone: None,
            click,
        }
    }
    pub fn tone(mut self, c: Hsla) -> Self {
        self.tone = Some(c);
        self
    }
}

/// Build the control; more than `WRAP_AFTER` items wrap into extra rows.
pub(crate) fn seg(items: Vec<SegItem>, cx: &App) -> AnyElement {
    let t = super::theme::tokens(cx);
    let mut row = h_flex()
        .flex_wrap()
        .gap(px(S1))
        .p(px(S1))
        .rounded(px(R_SM))
        .bg(t.hv)
        .max_w(px(W_SEG_MAX));
    for it in items {
        let click = it.click.clone();
        let sel = it.on;
        let col = if sel { it.tone.unwrap_or(t.tx) } else { t.tx2 };
        row = row.child(
            div()
                .id(it.id)
                .h(px(H_CONTROL - 2.0 * S1))
                .px(px(S5))
                .flex()
                .items_center()
                .rounded(px(R_SM - S1))
                .text_size(px(TX_SM))
                .cursor_pointer()
                .text_color(col)
                .when(sel, |d| d.bg(t.on).font_weight(FontWeight::MEDIUM))
                .when(!sel, |d| d.hover(|h| h.bg(t.hv).text_color(t.tx)))
                .child(it.label)
                .on_click(move |e, w, cx| click(e, w, cx)),
        );
    }
    row.into_any_element()
}
