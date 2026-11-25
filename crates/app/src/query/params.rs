//! The Run dialog for statements with placeholders or undefined variables: one
//! row per distinct name. Two kinds of row:
//!
//! - a **parameter** (`:name`, `?`, `@name`, ...) has a type (text / number /
//!   date / NULL) and goes to the driver as a bound value, never into the SQL;
//! - a **variable** (`${name}` without a value) is text: what is typed is
//!   pasted into the statement before it is checked and run, and kept for the
//!   tab like an `@set` line (`locus_core::vars`).
//!
//! Parameter values are remembered per tab and name. Enter runs, Esc cancels,
//! Tab moves between the rows, and the first empty row has the focus.
use crate::shell::scale::*;
use crate::shell::*;
use gpui_kit::component::Disableable as _;
use locus_core::params::{self, ParamType, ParamValue, Placeholder};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Widths of the kind tag, name and type columns: every row uses the same ones.
const W_KIND: f32 = 76.;
const W_NAME: f32 = 112.;
const W_TYPE: f32 = 84.;

type Values = (HashMap<String, String>, HashMap<String, ParamValue>);
type Collect = Rc<dyn Fn(&App) -> Option<Values>>;

impl Locus {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_params(
        &mut self,
        tab: Entity<QueryTab>,
        conn: SavedConnection,
        raw: String,
        raw_stmts: Vec<String>,
        phs: Vec<Placeholder>,
        undefined: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Variables first (they decide the text), then the bound parameters.
        let n_vars = undefined.len();
        let mut names = undefined;
        names.extend(params::names(&phs));
        let prev = tab.read(cx).bind.values.clone();
        let remembered = |i: usize, n: &String| prev.get(n).filter(|_| i >= n_vars).cloned();
        let types: Rc<RefCell<Vec<ParamType>>> = Rc::new(RefCell::new(
            names
                .iter()
                .enumerate()
                .map(|(i, n)| remembered(i, n).map(|v| v.ty).unwrap_or_default())
                .collect(),
        ));
        let inputs: Vec<Entity<InputState>> = names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let text = remembered(i, n).map(|v| v.text).unwrap_or_default();
                cx.new(|cx| {
                    let mut s = InputState::new(window, cx);
                    s.set_value(text, window, cx);
                    s
                })
            })
            .collect();
        // The first empty row has the focus (the first row when all are filled).
        let first = inputs
            .iter()
            .find(|i| i.read(cx).value().is_empty())
            .or(inputs.first())
            .cloned();
        let me = cx.entity().downgrade();
        // Collects the values; `None` while a Number row does not parse.
        let collect: Collect = {
            let (names, inputs, types) = (names.clone(), inputs.clone(), types.clone());
            Rc::new(move |cx: &App| {
                let (mut vars, mut pars) = (HashMap::new(), HashMap::new());
                for (i, n) in names.iter().enumerate() {
                    let text = inputs[i].read(cx).value().to_string();
                    if i < n_vars {
                        vars.insert(n.clone(), text);
                        continue;
                    }
                    let ty = types.borrow()[i];
                    if ty == ParamType::Number && !params::number_ok(&text) {
                        return None;
                    }
                    pars.insert(n.clone(), ParamValue { ty, text });
                }
                Some((vars, pars))
            })
        };
        let run: Rc<dyn Fn(&mut Window, &mut App)> = {
            let (tab, conn, me, collect) = (tab.clone(), conn.clone(), me.clone(), collect.clone());
            Rc::new(move |window: &mut Window, cx: &mut App| {
                let Some((vars, pars)) = collect(cx) else {
                    return;
                };
                window.close_dialog(cx);
                let (tab, conn, raw, raw_stmts) =
                    (tab.clone(), conn.clone(), raw.clone(), raw_stmts.clone());
                let _ = me.update(cx, |this, cx| {
                    tab.update(cx, |t, _| {
                        t.bind.vars.extend(vars);
                        t.completion.lock().vars = t.bind.vars.keys().cloned().collect();
                        t.bind.values.extend(pars.clone());
                    });
                    this.run_prepared(tab, conn, raw, raw_stmts, Some(pars), window, cx);
                });
            })
        };
        // Enter inside a value box runs, like the dialog's own Enter.
        for i in &inputs {
            let run = run.clone();
            cx.subscribe_in(i, window, move |_, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    // This callback runs inside an update of the shell; `run` updates it too,
                    // so it goes after the current update (updating it here panics).
                    let run = run.clone();
                    window.defer(cx, move |window, cx| run(window, cx));
                }
            })
            .detach();
        }
        let (run_ok, run_btn) = (run.clone(), run.clone());
        window.open_pop_dialog(cx, move |dialog, _, cx| {
            let t = crate::shell::theme::tokens(cx);
            let valid = collect(cx).is_some();
            let rows = param_rows(&names, n_vars, &types, &inputs, cx);
            let run_btn = run_btn.clone();
            let mut hint = v_flex().gap(px(S2)).text_size(px(TX_SM)).text_color(t.tx3);
            if n_vars < names.len() {
                hint = hint.child(t!("qp.hint").into_owned());
            }
            if n_vars > 0 {
                hint = hint.child(t!("qp.hint_vars").into_owned());
            }
            let body = v_flex().gap(px(S5)).child(hint).child(rows).child(
                h_flex()
                    .gap(px(S4))
                    .child(div().flex_1())
                    .child(
                        Button::new("qp-cancel")
                            .h(px(H_ROW))
                            .label(t!("common.cancel").to_string())
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("qp-run")
                            .h(px(H_ROW))
                            .primary()
                            .label(t!("qp.run").to_string())
                            .disabled(!valid)
                            .on_click(move |_, window, cx| run_btn(window, cx)),
                    ),
            );
            let run_ok = run_ok.clone();
            dialog
                .title(t!("qp.title").to_string())
                .w(px(W_DLG_M))
                .child(body)
                .on_ok(move |_, window, cx| {
                    run_ok(window, cx);
                    false
                })
        });
        // After the dialog took the focus for itself.
        if let Some(first) = first {
            window.defer(cx, move |window, cx| {
                first.read(cx).focus_handle(cx).focus(window, cx)
            });
        }
    }
}

/// One row per name: kind tag, name, value box, then the type button (a
/// parameter) or nothing (a variable) and the not-a-number hint.
fn param_rows(
    names: &[String],
    n_vars: usize,
    types: &Rc<RefCell<Vec<ParamType>>>,
    inputs: &[Entity<InputState>],
    cx: &App,
) -> Div {
    let t = crate::shell::theme::tokens(cx);
    let mut rows = v_flex().gap(px(S4));
    for (i, n) in names.iter().enumerate() {
        let is_var = i < n_vars;
        let ty = types.borrow()[i];
        let label = if is_var {
            format!("${{{n}}}")
        } else {
            n.clone()
        };
        let kind = if is_var {
            t!("qp.kind_var").into_owned()
        } else {
            t!("qp.kind_param").into_owned()
        };
        let types2 = types.clone();
        let bad =
            !is_var && ty == ParamType::Number && !params::number_ok(&inputs[i].read(cx).value());
        rows = rows.child(
            h_flex()
                .gap(px(S4))
                .items_center()
                .child(
                    div()
                        .w(px(W_KIND))
                        .flex_none()
                        .truncate()
                        .text_size(px(TX_XS))
                        .text_color(t.tx3)
                        .child(kind),
                )
                .child(
                    div()
                        .w(px(W_NAME))
                        .flex_none()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(px(TX_SM))
                        .child(label),
                )
                .child(
                    div().flex_1().min_w_0().child(
                        Input::new(&inputs[i])
                            .h(px(H_CONTROL))
                            .disabled(!is_var && ty == ParamType::Null),
                    ),
                )
                .child(if is_var {
                    div().w(px(W_TYPE)).flex_none().into_any_element()
                } else {
                    Button::new(("qp-type", i))
                        .h(px(H_CONTROL))
                        .w(px(W_TYPE))
                        .label(t!(ty.label_key()).to_string())
                        .tooltip(t!("qp.type_tip").to_string())
                        .on_click(move |_, window, _| {
                            let mut v = types2.borrow_mut();
                            v[i] = v[i].next();
                            window.refresh();
                        })
                        .into_any_element()
                })
                .children(bad.then(|| {
                    div()
                        .flex_none()
                        .text_size(px(TX_XS))
                        .text_color(t.red_text)
                        .child(t!("qp.not_number").into_owned())
                })),
        );
    }
    rows
}
