//! Sidebar: connection rows (badge, name, shared link, env badge, status dot;
//! hover shows "new query here" and "…") and the schema list of an expanded row.
use crate::shell::scale::*;
use crate::shell::*;
use locus_core::store::DbKind;

impl Locus {
    pub(crate) fn render_conn_row(
        &self,
        i: usize,
        c: &SavedConnection,
        _collapsed: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let active = self.active_tab(cx).read(cx).conn_id.as_deref() == Some(c.id.as_str());
        let shared = self
            .spaces
            .iter()
            .filter(|s| s.connections.contains(&c.id))
            .count()
            > 1;
        let (state, state_label) = self.conn_state(&c.id, cx);
        let (env_bg, env_fg) = crate::shell::theme::env_colors(c.env, cx);
        let group = SharedString::from(format!("conn-row-{i}"));
        let id = c.id.clone();
        let id_q = c.id.clone();
        let tooltip = match tls_warning(c.tls, c.kind) {
            Some(w) => format!("{}\n{}\n{}", c.subtitle(), state_label, w),
            None => format!("{}\n{}", c.subtitle(), state_label),
        };
        let me = cx.entity().downgrade();
        let menu = self.conn_menu(c.id.clone(), me.clone());
        let menu2 = self.conn_menu(c.id.clone(), me);
        h_flex()
            .id(("conn", i))
            .group(group.clone())
            .h(px(H_ROW))
            .mx(px(S4))
            .mb(px(S1))
            .px(px(S4))
            .gap(px(S3))
            .items_center()
            .rounded(px(R_MD))
            .cursor_pointer()
            .when(active, |d| d.bg(t.hv.opacity(1.2)))
            .hover(|st| st.bg(t.hv))
            .tooltip(tip(tooltip))
            .child(self.conn_badge(c, cx))
            .child(
                div()
                    .flex_shrink(1.)
                    .min_w_0()
                    .truncate()
                    .text_color(if active { t.tx1 } else { t.tx2 })
                    .child(c.name.clone()),
            )
            .when(shared, |r| {
                r.child(
                    div()
                        .flex_none()
                        .text_color(t.tx4)
                        .group_hover(group.clone(), |s| s.invisible())
                        .child(crate::shell::icons::icon("link").size(px(11.))),
                )
            })
            // S6: the no-TLS warning shows on hover / the selected row only (the tooltip keeps the text).
            .when(tls_warning(c.tls, c.kind).is_some(), |r| {
                r.child(
                    div()
                        .flex_none()
                        .text_color(t.yel)
                        .when(!active, |d| {
                            d.invisible().group_hover(group.clone(), |s| s.visible())
                        })
                        .child(Icon::new(IconName::TriangleAlert).size(px(11.))),
                )
            })
            .child(div().flex_1())
            // Hover actions take the env badge's place. Only visibility
            // changes on hover (a display change on group hover panics in
            // gpui-fast: "must call prepaint before paint").
            .child(
                div()
                    .relative()
                    .self_stretch()
                    .flex()
                    .items_center()
                    .flex_none()
                    .child(
                        div()
                            .px(px(S3))
                            .py(px(1.))
                            .rounded(px(4.))
                            .bg(env_bg)
                            .text_color(env_fg)
                            .text_size(px(10.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .group_hover(group.clone(), |s| s.invisible())
                            .child(c.env.badge()),
                    )
                    .child(
                        h_flex()
                            .absolute()
                            .right(px(-2.))
                            .top_0()
                            .bottom_0()
                            .items_center()
                            .gap(px(2.))
                            .invisible()
                            .group_hover(group.clone(), |s| s.visible())
                            .child(
                                ib(
                                    ("conn-q", i),
                                    crate::shell::icons::icon("plus"),
                                    20.,
                                    false,
                                    cx,
                                )
                                .tooltip(tip(t!("shell.new_query_here").into_owned()))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.new_tab_on(Some(id_q.clone()), window, cx);
                                    },
                                )),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .child(
                                        Button::new(("conn-more", i))
                                            .ghost()
                                            .w(px(20.))
                                            .h(px(20.))
                                            .p_0()
                                            .icon(Icon::new(IconName::Ellipsis).size(px(13.)))
                                            .tooltip(t!("shell.conn_options").to_string())
                                            .dropdown_menu(move |m, w, cx| menu(m, w, cx)),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .size(px(7.))
                    .rounded_full()
                    .bg(crate::shell::theme::state_color(state, cx)),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.use_connection(id.clone(), cx)))
            .context_menu(move |m, w, cx| menu2(m, w, cx))
            .into_any_element()
    }

    /// The right-click / "…" menu of a connection row.
    fn conn_menu(
        &self,
        cid: String,
        me: WeakEntity<Locus>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        use crate::shell::menu::{danger, item};
        let redis_kind = self
            .conns
            .iter()
            .find(|c| c.id == cid)
            .map(|c| c.kind)
            .unwrap_or_default();
        let spaces: Vec<(usize, String)> = self
            .spaces
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != self.active_space)
            .map(|(j, s)| (j, s.name.clone()))
            .collect();
        move |mut menu: PopupMenu, window, cx| {
            // Run the action on the shell (the menu closes itself).
            let act = |f: Box<dyn Fn(&mut Locus, &mut Window, &mut Context<Locus>)>| {
                let me = me.clone();
                move |window: &mut Window, cx: &mut App| {
                    let _ = me.update(cx, |this, cx| f(this, window, cx));
                }
            };
            // Group 1: use it.
            let c = cid.clone();
            menu = menu.item(item(
                t!("shell.new_query_here").into_owned(),
                Some("query"),
                act(Box::new(move |this, w, cx| {
                    this.new_tab_on(Some(c.clone()), w, cx)
                })),
            ));
            let c = cid.clone();
            menu = menu.item(item(
                t!("shell.use_in_tab").into_owned(),
                Some("link"),
                act(Box::new(move |this, _, cx| {
                    this.use_connection(c.clone(), cx)
                })),
            ));
            let c = cid.clone();
            menu = menu.item(item(
                t!("erv3.conn_open").into_owned(),
                Some("schema"),
                act(Box::new(move |this, w, cx| {
                    this.open_conn_diagram(c.clone(), w, cx)
                })),
            ));
            if redis_kind != DbKind::Sqlite {
                menu = Self::activity_menu_item(menu, &cid, &me);
            }
            if redis_kind == DbKind::Redis {
                menu = Self::redis_menu_items(menu, &cid, &me).separator();
            }
            // Group 2: Spaces (the list of Spaces is a submenu, not nine flat rows).
            menu = menu.separator();
            if !spaces.is_empty() {
                let (cid2, me2, spaces2) = (cid.clone(), me.clone(), spaces.clone());
                menu = menu.submenu_with_icon(
                    Some(crate::shell::icons::icon("folder")),
                    t!("t4.menu_move_space").to_string(),
                    window,
                    cx,
                    move |mut sub, _, _| {
                        for (j, name) in spaces2.clone() {
                            let (c, me) = (cid2.clone(), me2.clone());
                            sub = sub.item(item(name, None, move |_, cx| {
                                let _ =
                                    me.update(cx, |this, cx| this.move_conn_to_space(&c, j, cx));
                            }));
                        }
                        sub
                    },
                );
            }
            let c = cid.clone();
            menu = menu.item(item(
                t!("sidebar.remove_conn").into_owned(),
                Some("minus"),
                act(Box::new(move |this, _, cx| {
                    this.remove_conn_from_space(&c, cx)
                })),
            ));
            // Group 3: edit.
            let c = cid.clone();
            menu = menu.separator().item(item(
                t!("sidebar.edit").into_owned(),
                Some("pencil"),
                act(Box::new(move |this, w, cx| {
                    if let Some(x) = this.conns.iter().find(|x| x.id == c).cloned() {
                        this.open_form(Some(x), w, cx);
                    }
                })),
            ));
            // Destructive last.
            let c = cid.clone();
            menu.separator().item(danger(
                t!("conn.delete_menu").into_owned(),
                Some("close"),
                act(Box::new(move |this, w, cx| {
                    this.confirm_delete_connection(c.clone(), w, cx)
                })),
            ))
        }
    }
}
