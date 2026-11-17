//! The "SSH" page of the connection form: use a jump host for this connection.
//! State and rendering live here so `form.rs` only holds one field and a tab.

use super::form::{ConnForm, field};
use crate::shell::scale::*;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::v_flex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use locus_core::sshcfg::{SshAuth, SshConfig};
use locus_core::store::SavedConnection;
use rust_i18n::t;

pub struct SshForm {
    pub enabled: bool,
    pub auth: SshAuth,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    key: Entity<InputState>,
    /// Password or key passphrase (masked).
    secret: Entity<InputState>,
    has_secret: bool,
}

impl SshForm {
    pub fn new(
        editing: Option<&SavedConnection>,
        window: &mut Window,
        cx: &mut Context<ConnForm>,
    ) -> Self {
        let s = editing.and_then(|c| c.ssh.as_ref());
        let mut input = |v: String| cx.new(|cx| InputState::new(window, cx).default_value(v));
        let host = input(s.map(|s| s.host.clone()).unwrap_or_default());
        let port = input(s.map(|s| s.port).unwrap_or(22).to_string());
        let user = input(s.map(|s| s.user.clone()).unwrap_or_default());
        let key = input(s.and_then(|s| s.key_path.clone()).unwrap_or_default());
        let secret = cx.new(|cx| InputState::new(window, cx).masked(true));
        let has_secret = editing.is_some_and(|c| {
            matches!(
                locus_core::creds::read(&SshConfig::secret_id(&c.id)),
                Ok(Some(_))
            )
        });
        Self {
            enabled: s.is_some(),
            auth: s.map(|s| s.auth).unwrap_or_default(),
            host,
            port,
            user,
            key,
            secret,
            has_secret,
        }
    }

    /// `Ok(None)` = tunnel off. `Err` = message for the form.
    pub fn collect(&self, cx: &App) -> Result<Option<SshConfig>, String> {
        if !self.enabled {
            return Ok(None);
        }
        let v = |s: &Entity<InputState>| s.read(cx).value().trim().to_string();
        let host = v(&self.host);
        let port: u16 = v(&self.port)
            .parse()
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| t!("ssh.err_port").into_owned())?;
        let user = v(&self.user);
        if host.is_empty() {
            return Err(t!("ssh.err_host").into_owned());
        }
        if user.is_empty() {
            return Err(t!("ssh.err_user").into_owned());
        }
        let key = v(&self.key);
        if self.auth == SshAuth::Key && key.is_empty() {
            return Err(t!("ssh.err_key").into_owned());
        }
        let secret = self.secret.read(cx).value().to_string();
        let secret = (self.auth != SshAuth::Agent && !secret.is_empty()).then_some(secret);
        Ok(Some(SshConfig {
            host,
            port,
            user,
            auth: self.auth,
            key_path: (self.auth == SshAuth::Key).then_some(key),
            secret,
        }))
    }

    pub fn render(&self, cx: &mut Context<ConnForm>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let ix = match self.auth {
            SshAuth::Password => 0,
            SshAuth::Key => 1,
            SshAuth::Agent => 2,
        };
        let secret_label = if self.auth == SshAuth::Key {
            t!("ssh.passphrase")
        } else {
            t!("ssh.password")
        };
        let mut v = v_flex().gap(px(S5)).child(
            Checkbox::new("ssh-on")
                .label(t!("ssh.enable").to_string())
                .checked(self.enabled)
                .on_click(cx.listener(|this, c: &bool, _, cx| {
                    this.ssh.enabled = *c;
                    cx.notify();
                })),
        );
        if self.enabled {
            v = v
                .child(
                    div()
                        .text_size(px(TX_SM))
                        .text_color(t.tx3)
                        .whitespace_normal()
                        .child(t!("ssh.note").to_string()),
                )
                .child(
                    gpui_kit::component::h_flex()
                        .gap(px(S4))
                        .child(div().flex_1().child(field(
                            cx,
                            t!("ssh.host").into_owned(),
                            Input::new(&self.host),
                        )))
                        .child(div().w(px(90.)).child(field(
                            cx,
                            t!("conn.port").into_owned(),
                            Input::new(&self.port),
                        ))),
                )
                .child(field(
                    cx,
                    t!("ssh.user").into_owned(),
                    Input::new(&self.user),
                ))
                .child(field(
                    cx,
                    t!("ssh.auth").into_owned(),
                    RadioGroup::horizontal("ssh-auth")
                        .selected_index(Some(ix))
                        .on_click(cx.listener(|this, ix: &usize, _, cx| {
                            this.ssh.auth = match *ix {
                                0 => SshAuth::Password,
                                1 => SshAuth::Key,
                                _ => SshAuth::Agent,
                            };
                            cx.notify();
                        }))
                        .child(Radio::new("ssh-pw").label(t!("ssh.auth_password").to_string()))
                        .child(Radio::new("ssh-key").label(t!("ssh.auth_key").to_string()))
                        .child(Radio::new("ssh-agent").label(t!("ssh.auth_agent").to_string())),
                ))
                .when(self.auth == SshAuth::Key, |v| {
                    v.child(field(
                        cx,
                        t!("ssh.key_file").into_owned(),
                        Input::new(&self.key),
                    ))
                })
                .when(self.auth != SshAuth::Agent, |v| {
                    v.child(field(
                        cx,
                        secret_label.into_owned(),
                        Input::new(&self.secret),
                    ))
                    .child(
                        div().text_size(px(TX_SM)).text_color(t.tx3).child(
                            if self.has_secret {
                                t!("ssh.secret_keep")
                            } else {
                                t!("ssh.secret_store")
                            }
                            .to_string(),
                        ),
                    )
                })
                .when(self.auth == SshAuth::Agent, |v| {
                    v.child(
                        div()
                            .text_size(px(TX_SM))
                            .text_color(t.tx3)
                            .child(t!("ssh.agent_note").to_string()),
                    )
                });
        }
        v.into_any_element()
    }
}
