//! The Run dialog for statements with placeholders: one row per distinct
//! parameter (name, value, type: text / number / date / NULL). Values are
//! remembered per tab and name; Enter runs. The values go to the driver as
//! bound parameters (`Driver::execute`), never into the SQL text.
use crate::shell::scale::*;
use crate::shell::*;
use gpui_kit::component::Disableable as _;
use locus_core::params::{self, ParamType, ParamValue, Placeholder};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

impl Locus {
    pub(crate) fn open_params(
        &mut self,
        tab: Entity<QueryTab>,
        conn: SavedConnection,
        sql: String,
        phs: Vec<Placeholder>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let names = params::names(&phs);
        let prev = tab.read(cx).bind.values.clone();
        let types: Rc<RefCell<Vec<ParamType>>> = Rc::new(RefCell::new(
            names
                .iter()
                .map(|n| prev.get(n).map(|v| v.ty).unwrap_or_default())
                .collect(),
        ));
        let inputs: Vec<Entity<InputState>> = names
            .iter()
            .map(|n| {
                let text = prev.get(n).map(|v| v.text.clone()).unwrap_or_default();
                cx.new(|cx| {
                    let mut s = InputState::new(window, cx);
                    s.set_value(text, window, cx);
                    s
                })
            })
            .collect();
        if let Some(first) = inputs.first() {
            first.read(cx).focus_handle(cx).focus(window, cx);
        }
        let me = cx.entity().downgrade();
        // Collects the values; `None` while a Number row does not parse.
        let collect: Rc<dyn Fn(&App) -> Option<HashMap<String, ParamValue>>> = {
            let (names, inputs, types) = (names.clone(), inputs.clone(), types.clone());
            Rc::new(move |cx: &App| {
                let mut m = HashMap::new();
                for (i, n) in names.iter().enumerate() {
                    let ty = types.borrow()[i];
                    let text = inputs[i].read(cx).value().to_string();
                    if ty == ParamType::Number && !params::number_ok(&text) {
                        return None;
                    }
                    m.insert(n.clone(), ParamValue { ty, text });
                }
                Some(m)
            })
        };
        let run = run_callback(&tab, &conn, &sql, &phs, &me, &collect, &names);
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
            let rows = param_rows(&names, &types, &inputs, cx);
            let run_btn = run_btn.clone();
            let body = v_flex()
                .gap(px(S5))
                .child(
                    div()
                        .text_size(px(TX_SM))
                        .text_color(t.tx3)
                        .child(t!("qp.hint").into_owned()),
                )
                .child(rows)
                .child(
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
    }
}

type Collect = Rc<dyn Fn(&App) -> Option<HashMap<String, ParamValue>>>;

/// The Run button / Enter: stores the values on the tab and hands the bound statement to the gate.
fn run_callback(
    tab: &Entity<QueryTab>,
    conn: &SavedConnection,
    sql: &str,
    phs: &[Placeholder],
    me: &WeakEntity<Locus>,
    collect: &Collect,
    names: &[String],
) -> Rc<dyn Fn(&mut Window, &mut App)> {
    let (tab, conn, sql, phs, me, collect, phs_names) = (
        tab.clone(),
        conn.clone(),
        sql.to_string(),
        phs.to_vec(),
        me.clone(),
        collect.clone(),
        names.to_vec(),
    );
    Rc::new(move |window: &mut Window, cx: &mut App| {
        let Some(values) = collect(cx) else { return };
        window.close_dialog(cx);
        let (tab, conn, sql, phs) = (tab.clone(), conn.clone(), sql.to_string(), phs.to_vec());
        let _ = me.update(cx, |this, cx| {
            tab.update(cx, |t, _| t.bind.values.extend(values.clone()));
            let (send, bound) = params::bind(&sql, &phs, &values);
            let written = phs_names
                .iter()
                .filter_map(|n| {
                    values
                        .get(n)
                        .map(|v| locus_core::history::HistParam::from_value(n, v))
                })
                .collect();
            this.gate_run(tab, conn, sql, send, Vec::new(), bound, written, window, cx);
        });
    })
}

/// One row per placeholder: name, value box, type button and the not-a-number hint.
fn param_rows(
    names: &[String],
    types: &Rc<RefCell<Vec<ParamType>>>,
    inputs: &[Entity<InputState>],
    cx: &App,
) -> Div {
    let t = crate::shell::theme::tokens(cx);
    let mut rows = v_flex().gap(px(S4));
    for (i, n) in names.iter().enumerate() {
        let ty = types.borrow()[i];
        let label = if let Some(num) = n.strip_prefix('?') {
            format!("?{num}")
        } else {
            n.clone()
        };
        let types2 = types.clone();
        let bad = ty == ParamType::Number && !params::number_ok(&inputs[i].read(cx).value());
        rows = rows.child(
            h_flex()
                .gap(px(S4))
                .items_center()
                .child(
                    div()
                        .w(px(120.))
                        .flex_none()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(px(TX_SM))
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&inputs[i]).disabled(ty == ParamType::Null)),
                )
                .child(
                    Button::new(("qp-type", i))
                        .h(px(H_ROW))
                        .w(px(84.))
                        .label(t!(ty.label_key()).to_string())
                        .tooltip(t!("qp.type_tip").to_string())
                        .on_click(move |_, window, _| {
                            let mut v = types2.borrow_mut();
                            v[i] = v[i].next();
                            window.refresh();
                        }),
                )
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
