//! The dropdown of a result column header (DBeaver style): order by, filter
//! by the selected cell / a typed value / the clipboard, NULL tests and the
//! footer actions. It only builds the menu; the query tab applies the
//! action (`GridUp::Col`) by re-running the query with the filters wrapped
//! around it.

use super::grid::{GridUp, ResultGrid};
use crate::redis::{Field, prompt};
use gpui_kit::component::h_flex;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{TableSelection, TableState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use locus_core::colfilter::{Cond, Op, elide};
use locus_core::copyfmt::{Dialect, Value, sql_literal};
use rust_i18n::t;
use std::rc::Rc;

/// What a header menu entry asks the query tab to do.
#[derive(Clone, Debug)]
pub enum ColAct {
    Order { col: String, desc: bool },
    Add(Cond),
    RemoveCond(usize),
    RemoveOrder(usize),
    Clear,
    Save,
    ResetSaved,
}

/// What the menu of one column depends on, read when it opens.
pub struct MenuCtx {
    pub col: String,
    pub numeric: bool,
    /// The selected cell of this column (`None` = none selected or NULL).
    pub cell: Option<String>,
    pub clip: Option<String>,
    pub saved: bool,
    pub any: bool,
}

/// Longest value shown in a menu label before its middle is elided.
const LABEL_MAX: usize = 32;

/// The selected cell's text when it is in column `col` of the grid.
pub fn selected_cell(t: &TableState<ResultGrid>, col: usize) -> Option<String> {
    match t.selection() {
        TableSelection::Cell(r, c) if c == col => t.delegate().value(r, c),
        _ => None,
    }
}

/// Clipboard text usable as a value: not empty, a single line.
pub fn clipboard_value(cx: &App) -> Option<String> {
    let s = cx.read_from_clipboard()?.text()?;
    (!s.is_empty() && !s.contains(['\n', '\r'])).then_some(s)
}

/// `name = 'v'` / `name LIKE '%v%'` as shown in the menu (value elided).
fn label(col: &str, op: Op, v: &str, numeric: bool) -> String {
    let v = elide(v, LABEL_MAX);
    if op.is_like() {
        return format!("{col} {} '%{}%'", op.symbol(), v.replace('\'', "''"));
    }
    let lit = sql_literal(&Value::Text(v), numeric, Dialect::Postgres);
    format!("{col} {} {lit}", op.symbol())
}

fn cond_for(col: &str, op: Op, v: &str, numeric: bool) -> Cond {
    match op {
        Op::Like => Cond::contains(col, false, v, numeric),
        Op::ILike => Cond::contains(col, true, v, numeric),
        _ => Cond::cmp(col, op, v, numeric),
    }
}

/// Sends `a` to the query tab.
fn send(me: &WeakEntity<TableState<ResultGrid>>, a: ColAct, cx: &mut App) {
    let _ = me.update(cx, |t, cx| t.delegate().up(GridUp::Col(a), cx));
}

/// Width of the icon column: every row reserves it, so all labels start at the same x.
const ICON_W: f32 = 16.;

/// One menu row: a fixed-width icon column (empty when `icon` is `None`), then the text.
/// `head` = muted section title; `dim` = disabled look.
fn row(icon: Option<&'static str>, text: String, head: bool, dim: bool) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        let t = crate::shell::theme::tokens(cx);
        let slot = div()
            .flex_none()
            .w(px(ICON_W))
            .h(px(ICON_W))
            .flex()
            .items_center()
            .justify_center()
            .children(icon.map(|i| crate::shell::icons::icon(i).size(px(14.)).text_color(t.tx3)));
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .when(head, |d| d.text_xs().text_color(t.tx4))
            .when(dim, |d| d.opacity(0.45))
            .child(slot)
            .child(div().min_w_0().truncate().child(text.clone()))
    })
}

fn entry(
    me: &WeakEntity<TableState<ResultGrid>>,
    text: String,
    act: ColAct,
    on: bool,
) -> PopupMenuItem {
    let me = me.clone();
    row(None, text, false, !on)
        .disabled(!on)
        .on_click(move |_, _, cx| send(&me, act.clone(), cx))
}

fn with_icon(icon: &'static str, text: String) -> PopupMenuItem {
    row(Some(icon), text, false, false)
}

fn head(text: String) -> PopupMenuItem {
    row(None, text, true, false)
}
/// Asks for a value, then adds `col <op> value`.
fn ask_value(
    me: &WeakEntity<TableState<ResultGrid>>,
    col: &str,
    op: Op,
    numeric: bool,
    init: String,
    window: &mut Window,
    cx: &mut App,
) {
    let (me, c) = (me.clone(), col.to_string());
    let title = t!("colmenu.value_title", col = col).to_string();
    let field = Field::line(format!("{col} {}", op.symbol()), init);
    prompt(
        window,
        cx,
        title,
        vec![field],
        t!("colmenu.apply").to_string(),
        Rc::new(move |v, _, cx| {
            let v = v.into_iter().next().unwrap_or_default();
            let cond = match op {
                Op::Like | Op::ILike => {
                    // Typed patterns are used as typed (`%` and `_` work).
                    Cond::Cmp {
                        col: c.clone(),
                        op,
                        value: v,
                        numeric,
                        esc: false,
                    }
                }
                _ => Cond::cmp(&c, op, &v, numeric),
            };
            send(&me, ColAct::Add(cond), cx);
        }),
    );
}

/// Asks for a WHERE clause.
fn ask_raw(me: &WeakEntity<TableState<ResultGrid>>, window: &mut Window, cx: &mut App) {
    let me = me.clone();
    prompt(
        window,
        cx,
        t!("colmenu.raw_title").to_string(),
        vec![Field::line(t!("colmenu.raw_label").to_string(), "")],
        t!("colmenu.apply").to_string(),
        Rc::new(move |v, _, cx| {
            let w = v.into_iter().next().unwrap_or_default();
            if !w.trim().is_empty() {
                send(&me, ColAct::Add(Cond::Raw(w)), cx);
            }
        }),
    );
}

/// A section of six comparisons with `v` (cell value or clipboard).
fn value_section(
    mut menu: PopupMenu,
    me: &WeakEntity<TableState<ResultGrid>>,
    ctx: &MenuCtx,
    title: String,
    v: &str,
) -> PopupMenu {
    menu = menu.separator().item(head(title));
    for op in Op::ALL {
        let text = label(&ctx.col, op, v, ctx.numeric);
        menu = menu.item(entry(
            me,
            text,
            ColAct::Add(cond_for(&ctx.col, op, v, ctx.numeric)),
            true,
        ));
    }
    menu
}

/// The six "custom" entries: each asks for its value.
fn custom_section(
    mut menu: PopupMenu,
    me: &WeakEntity<TableState<ResultGrid>>,
    ctx: &MenuCtx,
) -> PopupMenu {
    menu = menu
        .separator()
        .item(head(t!("colmenu.custom").to_string()));
    for op in Op::ALL {
        let (me, col, numeric) = (me.clone(), ctx.col.clone(), ctx.numeric);
        menu = menu.item(
            row(
                None,
                format!("{} {} ..", ctx.col, op.symbol()),
                false,
                false,
            )
            .on_click(move |_, window, cx| {
                ask_value(&me, &col, op, numeric, String::new(), window, cx)
            }),
        );
    }
    menu
}

/// Builds the menu of column `ctx.col`.
pub fn build(
    mut menu: PopupMenu,
    me: &WeakEntity<TableState<ResultGrid>>,
    ctx: &MenuCtx,
) -> PopupMenu {
    let col = ctx.col.clone();
    menu = menu.scrollable(true).max_h(px(520.));
    menu = menu
        .item(
            with_icon(
                "chevron-up",
                t!("colmenu.order_asc", col = col.as_str()).to_string(),
            )
            .on_click({
                let (me, col) = (me.clone(), col.clone());
                move |_, _, cx| {
                    let a = ColAct::Order {
                        col: col.clone(),
                        desc: false,
                    };
                    send(&me, a, cx)
                }
            }),
        )
        .item(
            with_icon(
                "chevron-down",
                t!("colmenu.order_desc", col = col.as_str()).to_string(),
            )
            .on_click({
                let (me, col) = (me.clone(), col.clone());
                move |_, _, cx| {
                    let a = ColAct::Order {
                        col: col.clone(),
                        desc: true,
                    };
                    send(&me, a, cx)
                }
            }),
        )
        .item(
            with_icon("filter", t!("colmenu.by_value").to_string()).on_click({
                let (me, col, numeric) = (me.clone(), col.clone(), ctx.numeric);
                let init = ctx.cell.clone().unwrap_or_default();
                move |_, window, cx| ask_value(&me, &col, Op::Eq, numeric, init.clone(), window, cx)
            }),
        );
    if let Some(v) = &ctx.cell {
        menu = value_section(menu, me, ctx, t!("colmenu.cell_value").to_string(), v);
    }
    menu = custom_section(menu, me, ctx);
    if let Some(v) = &ctx.clip {
        menu = value_section(menu, me, ctx, t!("colmenu.clipboard").to_string(), v);
    }
    menu = menu
        .separator()
        .item(entry(
            me,
            format!("{col} IS NULL"),
            ColAct::Add(Cond::IsNull(col.clone())),
            true,
        ))
        .item(entry(
            me,
            format!("{col} IS NOT NULL"),
            ColAct::Add(Cond::NotNull(col.clone())),
            true,
        ))
        .separator()
        .item(entry(
            me,
            t!("colmenu.save").to_string(),
            ColAct::Save,
            ctx.any,
        ))
        .item(entry(
            me,
            t!("colmenu.reset").to_string(),
            ColAct::ResetSaved,
            ctx.saved,
        ))
        .item(entry(
            me,
            t!("colmenu.clear").to_string(),
            ColAct::Clear,
            ctx.any,
        ));
    let me = me.clone();
    menu.item(
        row(None, t!("colmenu.raw").to_string(), false, false)
            .on_click(move |_, window, cx| ask_raw(&me, window, cx)),
    )
}

/// Label of a condition chip (the SQL it adds, identifiers unquoted).
pub fn chip_text(c: &Cond) -> String {
    c.sql(Dialect::Postgres, &|s| s.to_string())
}

impl ResultGrid {
    /// The small arrow at the right of a column header that opens the menu:
    /// shown on hover, and always while the column is filtered or ordered.
    pub fn header_menu(
        &self,
        col_ix: usize,
        cx: &mut Context<TableState<Self>>,
    ) -> Option<AnyElement> {
        use gpui_kit::component::button::{Button, ButtonVariants as _};
        use gpui_kit::component::{ActiveTheme as _, Sizable as _};
        let view = self.colview.as_ref()?;
        let meta = self.rs.columns().get(col_ix)?;
        let (col, numeric) = (
            meta.name.clone(),
            locus_core::copyfmt::is_numeric_type(&meta.type_name),
        );
        let (filtered, order) = (view.filtered(&col), view.order_of(&col));
        let active = filtered || order.is_some();
        let (icon, tip) = match (filtered, order) {
            (true, _) => ("filter", t!("colmenu.tip_filtered")),
            (_, Some(false)) => ("chevron-up", t!("colmenu.tip_asc")),
            (_, Some(true)) => ("chevron-down", t!("colmenu.tip_desc")),
            _ => ("caret-down", t!("colmenu.tip")),
        };
        let (saved, any) = (self.colsaved, !view.is_empty());
        let me = cx.entity().downgrade();
        let accent = cx.theme().primary;
        let group = SharedString::from(format!("th{col_ix}"));
        let btn = Button::new(("colmenu", col_ix))
            .ghost()
            .xsmall()
            .size(px(20.))
            .icon(
                crate::shell::icons::icon(icon)
                    .size(px(12.))
                    .when(active, |i| i.text_color(accent)),
            )
            .tooltip(tip.to_string())
            .on_click(move |_, window, cx| {
                let cell = me.upgrade().and_then(|t| selected_cell(t.read(cx), col_ix));
                let ctx = MenuCtx {
                    col: col.clone(),
                    numeric,
                    cell,
                    clip: clipboard_value(cx),
                    saved,
                    any,
                };
                // Same mechanism as the right-click menu: the tab shows it at
                // window level (deferred + anchored), so no header clips it.
                let weak = me.clone();
                let menu = PopupMenu::build(window, cx, move |m, _, _| build(m, &weak, &ctx));
                menu.read(cx).focus_handle(cx).focus(window, cx);
                let pos = window.mouse_position();
                let _ = me.update(cx, |t, cx| t.delegate().up(GridUp::ColMenu(menu, pos), cx));
            });
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(20.))
                .when(!active, |d| {
                    d.opacity(0.).group_hover(group, |s| s.opacity(1.))
                })
                .child(btn)
                .into_any_element(),
        )
    }
}
