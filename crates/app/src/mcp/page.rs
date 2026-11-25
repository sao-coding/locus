//! Settings › MCP: on/off, status, port, URL, token, ready-to-copy client
//! snippets and the per-connection access summary.
use super::{Mcp, Status};
use crate::settings::page::{SettingsPage, SettingsPageEvent, matches_query};
use crate::shell::scale::*;
use crate::shell::*;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::switch::Switch;
use locus_core::store::McpAccess;

/// Monospace chip used for the URL, the token and the snippets.
fn mono_chip(text: String, cx: &App) -> Div {
    let t = crate::shell::theme::tokens(cx);
    div()
        .px(px(S4))
        .py(px(S1))
        .rounded(px(R_SM))
        .bg(t.hv)
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(px(TX_MONO))
        .text_color(t.tx2)
        .child(text)
}

fn copy_button(id: &'static str, text: String, enabled: bool) -> Button {
    Button::new(id)
        .label(t!("mcp.copy").into_owned())
        .disabled(!enabled)
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            crate::shell::toast::show(t!("mcp.copied").into_owned(), cx);
        })
}

pub(crate) fn access_label(a: McpAccess) -> String {
    match a {
        McpAccess::Off => t!("mcp.access_off"),
        McpAccess::ReadOnly => t!("mcp.access_ro"),
        McpAccess::ReadWrite => t!("mcp.access_rw"),
    }
    .into_owned()
}

impl SettingsPage {
    /// Rows of the MCP category, filtered by the search text.
    pub(crate) fn mcp_rows(&self, q: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(m) = super::entity(cx) else {
            return Vec::new();
        };
        let t = crate::shell::theme::tokens(cx);
        let searching = !q.is_empty();
        let shows = |hay: &str| !searching || matches_query(hay, q);
        let (status, token, conns, listening) = {
            let m = m.read(cx);
            (m.status.clone(), m.token.clone(), m.conns(), m.listening())
        };
        let port = Mcp::port(cx);
        let url = Mcp::url_for(port);
        let have_token = !token.is_empty();
        let cat = t!("sp.cat_mcp").into_owned();
        let mut rows = Vec::new();

        // On / off.
        let (l, d) = (
            t!("mcp.enable").into_owned(),
            t!("mcp.enable_note").into_owned(),
        );
        if shows(&format!("{l} {d} {cat}")) {
            let on = crate::settings::get(cx).mcp_enabled;
            let me = cx.entity().downgrade();
            let sw = Switch::new("sw-mcp")
                .checked(on)
                .color(t.acc)
                .on_change(move |next, _, cx| {
                    let next = *next;
                    let _ = me.update(cx, |this, cx| {
                        crate::settings::update(cx, |s| s.mcp_enabled = next);
                        this.reload_settings(cx);
                        cx.emit(SettingsPageEvent::Changed);
                        cx.notify();
                    });
                })
                .into_any_element();
            rows.push(self.row(3000, l, d, sw, cx));
        }

        // Status line.
        let l = t!("mcp.status").into_owned();
        if shows(&format!("{l} {cat}")) {
            let (dot, text, color) = match &status {
                Status::Listening(u) => (
                    t.grn,
                    t!("mcp.listening", url = u.clone()).into_owned(),
                    t.tx2,
                ),
                Status::Off => (t.idle, t!("mcp.off").into_owned(), t.tx3),
                Status::Failed(e) => (
                    t.red,
                    t!("mcp.failed", msg = e.clone()).into_owned(),
                    t.red_text,
                ),
            };
            let ctl = h_flex()
                .gap(px(S3))
                .items_center()
                .child(div().size(px(7.)).rounded_full().bg(dot))
                .child(div().text_color(color).child(text))
                .into_any_element();
            rows.push(self.row(3001, l, String::new(), ctl, cx));
        }

        // Port.
        let (l, d) = (
            t!("mcp.port").into_owned(),
            t!("mcp.port_note").into_owned(),
        );
        if shows(&format!("{l} {d} {cat}")) {
            let input = self.mcp_port.clone();
            let apply = cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.apply_mcp_port(window, cx);
            });
            let ctl = h_flex()
                .gap(px(S3))
                .items_center()
                .child(div().w(px(96.)).child(Input::new(&input).px(px(S5))))
                .child(
                    Button::new("mcp-port-apply")
                        .label(t!("mcp.apply").into_owned())
                        .on_click(apply),
                )
                .into_any_element();
            rows.push(self.row(3002, l, d, ctl, cx));
        }

        // URL.
        let l = t!("mcp.url").into_owned();
        if shows(&format!("{l} {url} {cat}")) {
            let ctl = h_flex()
                .gap(px(S3))
                .items_center()
                .child(mono_chip(url.clone(), cx))
                .child(copy_button("mcp-copy-url", url.clone(), true))
                .into_any_element();
            rows.push(self.row(3003, l, String::new(), ctl, cx));
        }

        // Token.
        let (l, d) = (
            t!("mcp.token").into_owned(),
            t!("mcp.token_note").into_owned(),
        );
        if shows(&format!("{l} {d} {cat}")) {
            let shown = if have_token {
                super::mask(&token)
            } else {
                t!("mcp.token_none").into_owned()
            };
            let me = super::entity(cx);
            let ctl = h_flex()
                .gap(px(S3))
                .items_center()
                .child(mono_chip(shown, cx))
                .child(copy_button("mcp-copy-token", token.clone(), have_token))
                .child(
                    Button::new("mcp-regen")
                        .label(t!("mcp.regenerate").into_owned())
                        .disabled(!have_token)
                        .on_click(move |_, _, cx| {
                            if let Some(m) = &me {
                                m.update(cx, |m, cx| m.regenerate_token(cx));
                            }
                        }),
                )
                .into_any_element();
            rows.push(self.row(3004, l, d, ctl, cx));
        }

        // Snippets: shown with the masked token, copied with the real one.
        let tok_or = |real: bool| {
            if !have_token {
                "<token>".to_string()
            } else if real {
                token.clone()
            } else {
                super::mask(&token)
            }
        };
        for (i, (label, shown, copied)) in [
            (
                t!("mcp.snippet_claude").into_owned(),
                super::claude_command(&url, &tok_or(false)),
                super::claude_command(&url, &tok_or(true)),
            ),
            (
                t!("mcp.snippet_json").into_owned(),
                super::client_json(&url, &tok_or(false)),
                super::client_json(&url, &tok_or(true)),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            if !shows(&format!("{label} {cat}")) {
                continue;
            }
            let id: &'static str = if i == 0 {
                "mcp-copy-claude"
            } else {
                "mcp-copy-json"
            };
            let code = div()
                .w_full()
                .px(px(S5))
                .py(px(S4))
                .rounded(px(R_MD))
                .border_1()
                .border_color(t.ln)
                .bg(t.hv)
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(px(TX_MONO))
                .text_color(t.tx2)
                .whitespace_normal()
                .children(shown.lines().map(|l| {
                    div().child(if l.is_empty() {
                        " ".to_string()
                    } else {
                        l.to_string()
                    })
                }));
            rows.push(
                v_flex()
                    .id(("mcp-snip", i))
                    .gap(px(S3))
                    .py(px(S5))
                    .border_b_1()
                    .border_color(t.ln)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(S7))
                            .child(div().flex_1().min_w_0().text_color(t.tx).child(label))
                            .child(copy_button(id, copied, have_token)),
                    )
                    .child(code)
                    .into_any_element(),
            );
        }

        // Per-connection access.
        let l = t!("mcp.access_title").into_owned();
        let d = t!("mcp.access_note").into_owned();
        let hay = format!(
            "{l} {d} {cat} {}",
            conns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
        if shows(&hay) {
            rows.push(
                v_flex()
                    .pt(px(S7))
                    .pb(px(S4))
                    .gap(px(S1))
                    .child(
                        div()
                            .text_size(px(TX_BASE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.tx1)
                            .child(l),
                    )
                    .child(
                        div()
                            .text_size(px(TX_SM))
                            .text_color(t.tx3)
                            .whitespace_normal()
                            .child(d),
                    )
                    .into_any_element(),
            );
            let sql_conns: Vec<_> = conns
                .iter()
                .filter(|c| c.kind != locus_core::store::DbKind::Redis)
                .collect();
            if sql_conns.is_empty() {
                rows.push(
                    div()
                        .py(px(S5))
                        .text_color(t.tx3)
                        .child(t!("mcp.no_conns").into_owned())
                        .into_any_element(),
                );
            }
            for (i, c) in sql_conns.into_iter().enumerate() {
                let (ebg, efg) = crate::shell::theme::env_colors(c.env, cx);
                let a = c.mcp_access();
                let acolor = match a {
                    McpAccess::Off => t.tx4,
                    McpAccess::ReadOnly => t.tx2,
                    McpAccess::ReadWrite => {
                        if c.env == locus_core::store::Env::Prod {
                            t.red_text
                        } else {
                            t.tx2
                        }
                    }
                };
                let id = c.id.clone();
                rows.push(
                    h_flex()
                        .id(("mcp-acc", i))
                        .h(px(H_ROW + 8.))
                        .gap(px(S5))
                        .items_center()
                        .border_b_1()
                        .border_color(t.ln)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_color(t.tx)
                                .child(c.name.clone()),
                        )
                        .child(
                            div().flex_none().w(px(52.)).flex().justify_center().child(
                                div()
                                    .px(px(S3))
                                    .py(px(1.))
                                    .rounded(px(4.))
                                    .bg(ebg)
                                    .text_color(efg)
                                    .text_size(px(TX_XS))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(c.env.badge()),
                            ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .w(px(110.))
                                .text_color(acolor)
                                .child(access_label(a)),
                        )
                        .child(
                            Button::new(("mcp-edit", i))
                                .ghost()
                                .label(t!("mcp.edit").into_owned())
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(SettingsPageEvent::EditConnection(id.clone()))
                                })),
                        )
                        .into_any_element(),
                );
            }
        }
        let _ = listening;
        rows
    }

    /// Port box → settings (1024..=65535); the server restarts on the new port.
    pub(crate) fn apply_mcp_port(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.mcp_port.read(cx).value().trim().to_string();
        match text.parse::<u16>() {
            Ok(p) if p >= 1024 => {
                crate::settings::update(cx, |s| {
                    s.mcp_port = (p != locus_core::settings::MCP_DEFAULT_PORT).then_some(p)
                });
                self.reload_settings(cx);
                cx.emit(SettingsPageEvent::Changed);
            }
            _ => crate::shell::toast::show(t!("mcp.port_bad").into_owned(), cx),
        }
        cx.notify();
    }
}
