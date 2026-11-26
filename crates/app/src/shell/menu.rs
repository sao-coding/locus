//! Context-menu rows (audit pass T4): one layout for every menu. Label left, optional icon,
//! shortcut right-aligned in `tx4` mono, destructive rows in `red_text`, one row height.
//! Group rows with `PopupMenu::separator()` (safe actions first, destructive last) and put
//! long lists into `PopupMenu::submenu_with_icon`.
use super::scale::*;
use super::*;

pub(crate) fn row(
    label: String,
    icon: Option<&'static str>,
    kbd: Option<&'static str>,
    danger: bool,
    f: impl Fn(&mut Window, &mut App) + 'static,
) -> PopupMenuItem {
    let it = PopupMenuItem::element(move |_, cx| {
        let t = crate::shell::theme::tokens(cx);
        h_flex()
            .w_full()
            .min_w(px(168.))
            .px(px(S2))
            .h(px(H_ROW - 2.))
            .gap(px(S5))
            .justify_between()
            .items_center()
            .text_size(px(TX_BASE))
            .when(danger, |d| d.text_color(t.red_text))
            .child(div().child(label.clone()))
            .children(kbd.map(|k| div().text_size(px(TX_MONO)).text_color(t.tx4).child(k)))
    })
    .on_click(move |_, w, cx| f(w, cx));
    match icon {
        Some(i) => it.icon(crate::shell::icons::icon(i)),
        None => it,
    }
}

/// A plain row.
pub(crate) fn item(
    label: String,
    icon: Option<&'static str>,
    f: impl Fn(&mut Window, &mut App) + 'static,
) -> PopupMenuItem {
    row(label, icon, None, false, f)
}

/// A destructive row (Delete …): red, last in the menu behind a separator.
pub(crate) fn danger(
    label: String,
    icon: Option<&'static str>,
    f: impl Fn(&mut Window, &mut App) + 'static,
) -> PopupMenuItem {
    row(label, icon, None, true, f)
}

/// Width of the check column at the right edge of a [`choice`] row. It is there on every
/// row, checked or not, so names line up.
const CHECK_COL: f32 = 14.;
/// The leading icon of [`choice`] / [`lead_item`] rows: drawn by the row itself at 16 px
/// (the component's own icon slot is 12 px) in a 16 px column shared by all rows of the menu.
const LEAD: f32 = 16.;
/// Diameter of the filled dot standing in for a missing icon.
const DOT: f32 = 8.;

/// What stands in the icon column.
#[derive(Clone)]
pub(crate) enum Lead {
    Icon(Icon, Hsla),
    Dot(Hsla),
}

fn lead_cell(lead: &Lead) -> Div {
    let cell = div()
        .w(px(LEAD))
        .h(px(LEAD))
        .flex_none()
        .flex()
        .items_center()
        .justify_center();
    match lead {
        Lead::Icon(i, c) => cell.child(i.clone().size(px(LEAD)).text_color(*c)),
        Lead::Dot(c) => cell.child(div().size(px(DOT)).rounded_full().bg(*c)),
    }
}

fn lead_row(
    label: String,
    lead: Lead,
    checked: Option<bool>,
    f: impl Fn(&mut Window, &mut App) + 'static,
) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        let t = crate::shell::theme::tokens(cx);
        h_flex()
            .w_full()
            .min_w(px(168.))
            .px(px(S2))
            .h(px(H_ROW - 2.))
            .gap(px(S4))
            .items_center()
            .text_size(px(TX_BASE))
            .child(lead_cell(&lead))
            .child(div().flex_1().child(label.clone()))
            .when_some(checked, |d, on| {
                d.child(
                    div()
                        .w(px(CHECK_COL))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_end()
                        .when(on, |d| {
                            d.child(
                                crate::shell::icons::icon("check")
                                    .size(px(14.))
                                    .text_color(t.tx2),
                            )
                        }),
                )
            })
    })
    .on_click(move |_, w, cx| f(w, cx))
}

/// A choice row (one of several, e.g. a Space): the lead, the name, and the check mark of the
/// current one in its own fixed column on the right.
pub(crate) fn choice(
    label: String,
    lead: Lead,
    checked: bool,
    f: impl Fn(&mut Window, &mut App) + 'static,
) -> PopupMenuItem {
    lead_row(label, lead, Some(checked), f)
}

/// An action row in the same layout as [`choice`] (same 16 px icon column), for menus that
/// mix both.
pub(crate) fn lead_item(
    label: String,
    lead: Lead,
    f: impl Fn(&mut Window, &mut App) + 'static,
) -> PopupMenuItem {
    lead_row(label, lead, None, f)
}
