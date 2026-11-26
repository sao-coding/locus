//! Spaces: switch, create, delete, rename (tab and Space), Space menu.
use super::*;

impl Locus {
    // ---- spaces -----------------------------------------------------------

    pub(crate) fn switch_space(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i != self.active_space {
            let dir = if i > self.active_space { 1.0 } else { -1.0 };
            let w = self.sidebar_px();
            self.bump_anim(dir, w);
        }
        self.active_space = i;
        self.renaming = None;
        self.sync_git(cx);
        self.focus_editor(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(crate) fn new_space(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.spaces.len();
        let data = self.blank_tab(None, cx);
        let tab = self.make_tab(&data, window, cx);
        self.spaces.push(Space {
            id: locus_core::store::new_id(),
            name: t!("space.new_name", n = (n + 1).to_string()).into_owned(),
            color: n % SPACE_HUES.len(),
            icon: None,
            theme: locus_core::space_theme::SpaceTheme::of(n % SPACE_HUES.len(), None, None),
            connections: Vec::new(),
            tabs: vec![tab],
            layout: Layout::Tab(data.id),
            groups: Groups::default(),
            focused: 0,
            view: SidebarView::Tabs,
            folder: None,
        });
        self.switch_space(n, window, cx);
        // v3: no Space header in the sidebar; name it in the Space popover.
        let at = window.mouse_position();
        self.open_space_popover(n, at, window, cx);
    }

    pub(crate) fn delete_space(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.spaces.len() < 2 {
            return;
        }
        let s = self.spaces.remove(i);
        for t in &s.tabs {
            t.update(cx, |t, cx| t.cancel(cx));
        }
        let ids: Vec<String> = s.tabs.iter().map(|t| t.read(cx).id.clone()).collect();
        self.tab_subs.retain(|(t, _)| !ids.contains(t));
        drop(s);
        self.active_space = self.active_space.min(self.spaces.len() - 1);
        self.switch_space(self.active_space, window, cx);
    }

    pub(crate) fn start_rename(
        &mut self,
        what: Rename,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = match &what {
            Rename::Tab(id) => self
                .space()
                .tab(id, cx)
                .map(|t| t.read(cx).display_title(cx))
                .unwrap_or_default(),
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_rename(window, cx),
                _ => {}
            },
        );
        input.update(cx, |s, cx| s.focus(window, cx));
        self.renaming = Some((what, input, sub));
        cx.notify();
    }

    pub(crate) fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((what, input, _)) = self.renaming.take() else {
            return;
        };
        let v = input.read(cx).value().trim().to_string();
        match what {
            Rename::Tab(id) => {
                if let Some(t) = self.space().tab(&id, cx) {
                    t.update(cx, |t, cx| {
                        t.title = (!v.is_empty()).then_some(v);
                        cx.notify();
                    });
                }
            }
        }
        self.focus_editor(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(crate) fn confirm_delete_space(
        &mut self,
        i: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self.spaces[i].name.clone();
        let me = cx.entity().downgrade();
        window.open_pop_dialog(cx, move |dialog, _, _| {
            let me = me.clone();
            dialog
                .title(t!("space.delete_title", name = name.clone()).to_string())
                .w(px(scale::W_DLG_S))
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .whitespace_normal()
                                .child(t!("space.delete_body").to_string()),
                        )
                        .child(
                            h_flex().gap_2().child(div().flex_1()).child(
                                Button::new("delete-space-confirm")
                                    .danger()
                                    .small()
                                    .label(t!("space.delete").to_string())
                                    .on_click(move |_, window, cx| {
                                        window.close_dialog(cx);
                                        let _ = me.update(cx, |this, cx| {
                                            this.delete_space(i, window, cx)
                                        });
                                    }),
                            ),
                        ),
                )
        });
    }
}
