//! "Show variables": the client-side variables of the active tab (`@set name =
//! value`, `${name}`; see `locus_core::vars`) in a dialog. Each row can be
//! edited or deleted; a last row adds a new variable. Palette command only.
use crate::shell::scale::*;
use crate::shell::*;
use std::rc::Rc;

const W_NAME: f32 = 132.;

impl Locus {
    pub(crate) fn show_variables(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab(cx);
        if tab.read(cx).diagram.is_some() || tab.read(cx).structure.is_some() {
            return;
        }
        self.open_vars_dialog(tab, window, cx);
    }

    fn open_vars_dialog(
        &mut self,
        tab: Entity<QueryTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let vars: Vec<(String, String)> = tab
            .read(cx)
            .bind
            .vars
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let inputs: Vec<Entity<InputState>> = vars
            .iter()
            .map(|(_, v)| {
                let v = v.clone();
                cx.new(|cx| {
                    let mut s = InputState::new(window, cx);
                    s.set_value(v, window, cx);
                    s
                })
            })
            .collect();
        let new_name =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("vars.new_name").to_string()));
        let new_value =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("vars.new_value").to_string()));
        let me = cx.entity().downgrade();
        // Save: the edited values, plus the new row when it has a valid name.
        let save: Rc<dyn Fn(&mut Window, &mut App)> = {
            let (tab, vars, inputs, new_name, new_value) = (
                tab.clone(),
                vars.clone(),
                inputs.clone(),
                new_name.clone(),
                new_value.clone(),
            );
            Rc::new(move |window: &mut Window, cx: &mut App| {
                let name = new_name.read(cx).value().trim().to_string();
                if !name.is_empty() && !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    crate::shell::toast::show(t!("vars.bad_name").to_string(), cx);
                    return;
                }
                let edits: Vec<(String, String)> = vars
                    .iter()
                    .zip(&inputs)
                    .map(|((k, _), i)| (k.clone(), i.read(cx).value().to_string()))
                    .collect();
                let added =
                    (!name.is_empty()).then(|| (name, new_value.read(cx).value().to_string()));
                tab.update(cx, |t, _| {
                    t.bind.vars.extend(edits);
                    t.bind.vars.extend(added);
                    t.completion.lock().vars = t.bind.vars.keys().cloned().collect();
                });
                window.close_dialog(cx);
            })
        };
        for i in inputs.iter().chain([&new_name, &new_value]) {
            let save = save.clone();
            cx.subscribe_in(i, window, move |_, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    let save = save.clone();
                    window.defer(cx, move |window, cx| save(window, cx));
                }
            })
            .detach();
        }
        let save_btn = save.clone();
        window.open_pop_dialog(cx, move |dialog, _, cx| {
            let t = crate::shell::theme::tokens(cx);
            let mut rows = v_flex().gap(px(S4));
            for (i, (name, _)) in vars.iter().enumerate() {
                let (tab, me, name) = (tab.clone(), me.clone(), name.clone());
                rows = rows.child(
                    h_flex()
                        .gap(px(S4))
                        .items_center()
                        .child(
                            div()
                                .w(px(W_NAME))
                                .flex_none()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .text_size(px(TX_SM))
                                .child(format!("${{{name}}}")),
                        )
                        .child(div().flex_1().min_w_0().child(Input::new(&inputs[i])))
                        .child(
                            Button::new(("var-del", i))
                                .h(px(H_ROW))
                                .w(px(H_ROW))
                                .icon(crate::shell::icons::icon("trash").size(px(14.)))
                                .tooltip(t!("vars.delete").to_string())
                                .on_click(move |_, window, cx| {
                                    tab.update(cx, |t, _| {
                                        t.bind.vars.remove(&name);
                                        t.completion.lock().vars =
                                            t.bind.vars.keys().cloned().collect();
                                    });
                                    window.close_dialog(cx);
                                    let (tab, me) = (tab.clone(), me.clone());
                                    window.defer(cx, move |window, cx| {
                                        let _ = me.update(cx, |this, cx| {
                                            this.open_vars_dialog(tab, window, cx)
                                        });
                                    });
                                }),
                        ),
                );
            }
            let empty = vars.is_empty().then(|| {
                div()
                    .text_size(px(TX_SM))
                    .text_color(t.tx3)
                    .child(t!("vars.empty").into_owned())
            });
            let add = h_flex()
                .gap(px(S4))
                .items_center()
                .child(div().w(px(W_NAME)).flex_none().child(Input::new(&new_name)))
                .child(div().flex_1().min_w_0().child(Input::new(&new_value)))
                .child(div().w(px(H_ROW)).flex_none());
            let save_btn = save_btn.clone();
            let body = v_flex()
                .gap(px(S5))
                .child(
                    div()
                        .text_size(px(TX_SM))
                        .text_color(t.tx3)
                        .child(t!("vars.hint").into_owned()),
                )
                .children(empty)
                .child(rows)
                .child(add)
                .child(
                    h_flex()
                        .gap(px(S4))
                        .child(div().flex_1())
                        .child(
                            Button::new("vars-close")
                                .h(px(H_ROW))
                                .label(t!("common.cancel").to_string())
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("vars-save")
                                .h(px(H_ROW))
                                .primary()
                                .label(t!("common.save").to_string())
                                .on_click(move |_, window, cx| save_btn(window, cx)),
                        ),
                );
            let save_ok = save.clone();
            dialog
                .title(t!("vars.title").to_string())
                .w(px(W_DLG_M))
                .child(body)
                .on_ok(move |_, window, cx| {
                    save_ok(window, cx);
                    false
                })
        });
    }
}
