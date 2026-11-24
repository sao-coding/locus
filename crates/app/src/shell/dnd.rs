//! Dragging a tab onto a pane: drop zones and the resulting split.
use super::*;

// ---- drag a tab onto a pane ---------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropZone {
    Left,
    Right,
    Top,
    Bottom,
    Center,
}

impl Layout {
    /// Replaces the leaf showing `target` with `f(leaf)`. Returns true if found.
    pub(crate) fn map_tab(&mut self, target: &str, f: &mut dyn FnMut(Layout) -> Layout) -> bool {
        match self {
            Layout::Tab(t) if t == target => {
                let me = std::mem::replace(self, Layout::Tab(String::new()));
                *self = f(me);
                true
            }
            Layout::Tab(_) => false,
            Layout::Split { children, .. } => children.iter_mut().any(|c| c.map_tab(target, f)),
        }
    }
}

impl Locus {
    /// A tab dropped on a pane: an edge splits that pane, the centre shows the tab there.
    pub(crate) fn drop_on_pane(
        &mut self,
        tab_id: String,
        target: String,
        zone: DropZone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if tab_id == target {
            return;
        }
        if self.space().tab(&tab_id, cx).is_none() {
            return;
        }
        // A page (History / Settings) never joins a split, and nothing splits it.
        let page = self.is_page(&tab_id, cx) || self.is_page(&target, cx);
        if page && zone != DropZone::Center {
            return;
        }
        if self.is_page(&tab_id, cx) {
            self.show_tab(&tab_id, window, cx);
            return;
        }
        // The group is full: ask (replace the pane / open as a new group).
        if zone != DropZone::Center
            && self.space().groups.is_full(&target)
            && !self.space().panes().contains(&tab_id)
        {
            self.group_full_dialog(&tab_id, window, cx);
            return;
        }
        // A tab that is already on screen leaves its old pane.
        let panes = self.space().panes();
        if let Some(ix) = panes.iter().position(|p| p == &tab_id)
            && panes.len() > 1
        {
            self.space_mut().layout.remove_leaf(ix, &mut 0);
        }
        let state = cx.new(|_| ResizableState::default());
        let id = tab_id.clone();
        let (axis, before) = match zone {
            DropZone::Left => (SplitAxis::Row, true),
            DropZone::Right => (SplitAxis::Row, false),
            DropZone::Top => (SplitAxis::Column, true),
            DropZone::Bottom => (SplitAxis::Column, false),
            DropZone::Center => (SplitAxis::Row, false),
        };
        self.space_mut().layout.map_tab(&target, &mut |leaf| {
            if zone == DropZone::Center {
                return Layout::Tab(id.clone());
            }
            let new = Layout::Tab(id.clone());
            Layout::Split {
                axis,
                children: if before {
                    vec![new, leaf]
                } else {
                    vec![leaf, new]
                },
                sizes: Vec::new(),
                state: state.clone(),
            }
        });
        let panes = self.space().panes();
        self.space_mut().focused = panes.iter().position(|p| p == &tab_id).unwrap_or(0);
        self.after_layout_change(cx);
        self.focus_editor(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// While a tab is dragged: one overlay over the pane that follows the
    /// pointer (outer quarter = that edge, middle = centre) and previews the
    /// exact region the tab will take: a half of the pane, or all of it.
    pub(crate) fn drop_zones(&self, target: &str, cx: &mut Context<Self>) -> AnyElement {
        let _theme = cx.theme().clone();
        let hint = self
            .drop_hint
            .as_ref()
            .filter(|(t, _)| t == target)
            .map(|(_, z)| *z);
        let accent = self.space().accent(cx);
        let t_move = target.to_string();
        let t_drop = target.to_string();
        let preview = hint.map(|z| {
            let (l, t, w, h) = match z {
                DropZone::Left => (0.0, 0.0, 0.5, 1.0),
                DropZone::Right => (0.5, 0.0, 0.5, 1.0),
                DropZone::Top => (0.0, 0.0, 1.0, 0.5),
                DropZone::Bottom => (0.0, 0.5, 1.0, 0.5),
                DropZone::Center => (0.0, 0.0, 1.0, 1.0),
            };
            div()
                .absolute()
                .left(relative(l))
                .top(relative(t))
                .w(relative(w))
                .h(relative(h))
                .p_1()
                .child(
                    div()
                        .size_full()
                        .rounded_lg()
                        .border_2()
                        .border_color(accent)
                        .bg(accent.opacity(0.14)),
                )
        });
        div()
            .id(SharedString::from(format!("drop-{target}")))
            .absolute()
            .inset_0()
            .on_drag_move::<TabDrag>(cx.listener(
                move |this, ev: &DragMoveEvent<TabDrag>, _, cx| {
                    let b = ev.bounds;
                    let p = ev.event.position;
                    let mine = this.drop_hint.as_ref().is_some_and(|(t, _)| t == &t_move);
                    if !b.contains(&p) {
                        if mine {
                            this.drop_hint = None;
                            cx.notify();
                        }
                        return;
                    }
                    let fx = f32::from(p.x - b.origin.x) / f32::from(b.size.width).max(1.0);
                    let fy = f32::from(p.y - b.origin.y) / f32::from(b.size.height).max(1.0);
                    let z = if fx < 0.25 {
                        DropZone::Left
                    } else if fx > 0.75 {
                        DropZone::Right
                    } else if fy < 0.25 {
                        DropZone::Top
                    } else if fy > 0.75 {
                        DropZone::Bottom
                    } else {
                        DropZone::Center
                    };
                    if this.drop_hint.as_ref() != Some(&(t_move.clone(), z)) {
                        this.drop_hint = Some((t_move.clone(), z));
                        cx.notify();
                    }
                },
            ))
            .on_drop(cx.listener(move |this, d: &TabDrag, window, cx| {
                let z = this
                    .drop_hint
                    .take()
                    .filter(|(t, _)| t == &t_drop)
                    .map(|(_, z)| z)
                    .unwrap_or(DropZone::Center);
                this.drop_on_pane(d.id.clone(), t_drop.clone(), z, window, cx);
            }))
            .children(preview)
            .into_any_element()
    }
}
