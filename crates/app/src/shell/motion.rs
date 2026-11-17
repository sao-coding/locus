//! Sidebar motion: width and content transitions.
use super::*;

// ---- motion ---------------------------------------------------------------------

/// Sidebar transitions: short, ease-out, and finished animations cost nothing.
pub(crate) const SIDEBAR_ANIM: Duration = Duration::from_millis(180);

impl Locus {
    pub(crate) fn sidebar_px(&self) -> f32 {
        match self.sidebar {
            SidebarMode::Full => self.sidebar_width,
            SidebarMode::Icons => 56.0,
            SidebarMode::Hidden => 0.0,
        }
    }

    /// Restarts the sidebar animation: `dir` slides content (+1 from the right,
    /// -1 from the left, 0 = fade), `from_w` is the width before the change.
    pub(crate) fn bump_anim(&mut self, dir: f32, from_w: f32) {
        self.anim_seq += 1;
        self.slide_dir = dir;
        self.anim_from_w = from_w;
    }

    pub(crate) fn set_sidebar_mode(&mut self, mode: SidebarMode, cx: &mut Context<Self>) {
        let from = self.sidebar_px();
        self.sidebar = mode;
        self.bump_anim(0.0, from);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(crate) fn animate_sidebar(&self, el: Stateful<Div>) -> AnyElement {
        if self.reduce_motion || self.anim_seq == 0 || self.resizing_sidebar {
            return el.into_any_element();
        }
        let dir = self.slide_dir;
        let to_w = self.sidebar_px();
        let from_w = if self.anim_from_w < 0.0 {
            to_w
        } else {
            self.anim_from_w
        };
        el.with_animation(
            ("sidebar-anim", self.anim_seq as usize),
            Animation::new(SIDEBAR_ANIM).with_easing(ease_out_quint()),
            move |el, d| {
                let el = if (from_w - to_w).abs() > 0.5 {
                    el.w(px(from_w + (to_w - from_w) * d)).overflow_hidden()
                } else {
                    el
                };
                let _ = dir;
                el
            },
        )
        .into_any_element()
    }
}

impl Locus {
    /// Space switch: the content slides in from the side of the new dot; a
    /// view switch or mode change fades it.
    pub(crate) fn animate_content(&self, el: Stateful<Div>) -> AnyElement {
        if self.reduce_motion || self.anim_seq == 0 || self.resizing_sidebar {
            return el.into_any_element();
        }
        let dir = self.slide_dir;
        el.with_animation(
            ("sidebar-content-anim", self.anim_seq as usize),
            Animation::new(SIDEBAR_ANIM).with_easing(ease_out_quint()),
            move |el, d| {
                let el = if dir != 0.0 {
                    el.relative().left(px(dir * (1.0 - d) * 28.0))
                } else {
                    el
                };
                el.opacity(0.3 + 0.7 * d)
            },
        )
        .into_any_element()
    }
}
