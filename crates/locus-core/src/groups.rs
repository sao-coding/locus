//! Split groups (Strata v3): a group is 2..=4 tabs shown side by side. The
//! sidebar shows it as one row. Pure rules, no GPUI: membership, the 4-pane
//! cap, add / remove / replace / dissolve and where focus goes afterwards.
//! The view keeps the layout tree of the group on screen and calls
//! `sync_visible` after it changed that tree.

use crate::spaces::{LayoutData, SplitAxis};
use serde::{Deserialize, Serialize};

/// A group holds at most this many panes.
pub const MAX_PANES: usize = 4;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    /// Tab ids, in layout (leaf) order.
    pub panes: Vec<String>,
    /// The pane that had focus last.
    #[serde(default)]
    pub focus: String,
    /// Sidebar row expanded: one sub-row per pane.
    #[serde(default)]
    pub open: bool,
    /// Saved layout tree (sizes included); `None` = derive one from `panes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<LayoutData>,
    /// Temporarily maximised focused pane. Never saved.
    #[serde(skip)]
    pub zoom: bool,
}

/// The groups of one Space.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Groups(pub Vec<Group>);

/// Why a tab could not be added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddError {
    /// The group already has `MAX_PANES` panes: ask replace / new group.
    Full,
    /// Same tab, or the tab is already in that group.
    Nothing,
}

/// What `remove` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removal {
    pub group: String,
    /// One pane was left, so the group is gone.
    pub dissolved: bool,
    /// The pane that has focus in the group now (or the last pane when dissolved).
    pub focus: String,
}

impl Groups {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, gid: &str) -> Option<&Group> {
        self.0.iter().find(|g| g.id == gid)
    }

    pub fn get_mut(&mut self, gid: &str) -> Option<&mut Group> {
        self.0.iter_mut().find(|g| g.id == gid)
    }

    pub fn group_of(&self, tab: &str) -> Option<&Group> {
        self.0.iter().find(|g| g.panes.iter().any(|p| p == tab))
    }

    pub fn group_of_mut(&mut self, tab: &str) -> Option<&mut Group> {
        self.0.iter_mut().find(|g| g.panes.iter().any(|p| p == tab))
    }

    /// The group is full (adding a tab would need the dialog).
    pub fn is_full(&self, tab: &str) -> bool {
        self.group_of(tab)
            .is_some_and(|g| g.panes.len() >= MAX_PANES)
    }

    /// Puts `tab` next to `anchor` (before or after it). The anchor starts a
    /// group when it is alone. A tab that belongs to another group leaves it
    /// first. Returns the group id.
    pub fn add(&mut self, anchor: &str, tab: &str, before: bool) -> Result<String, AddError> {
        if anchor == tab
            || self
                .group_of(anchor)
                .is_some_and(|g| g.panes.iter().any(|p| p == tab))
        {
            return Err(AddError::Nothing);
        }
        if self.is_full(anchor) {
            return Err(AddError::Full);
        }
        // Leave the old group first (it may dissolve).
        self.remove(tab);
        let gid = match self.group_of(anchor) {
            Some(g) => g.id.clone(),
            None => {
                let id = new_gid();
                self.0.push(Group {
                    id: id.clone(),
                    panes: vec![anchor.to_string()],
                    focus: anchor.to_string(),
                    ..Default::default()
                });
                id
            }
        };
        let g = self.get_mut(&gid).unwrap();
        let i = g.panes.iter().position(|p| p == anchor).unwrap_or(0);
        g.panes
            .insert(if before { i } else { i + 1 }, tab.to_string());
        g.focus = tab.to_string();
        g.layout = None;
        g.zoom = false;
        Ok(gid)
    }

    /// Takes `tab` out of its group, keeping the tab itself. A group left with
    /// one pane is dissolved. Focus goes to the pane that took its place.
    pub fn remove(&mut self, tab: &str) -> Option<Removal> {
        let gi = self
            .0
            .iter()
            .position(|g| g.panes.iter().any(|p| p == tab))?;
        let g = &mut self.0[gi];
        let i = g.panes.iter().position(|p| p == tab).unwrap();
        g.panes.remove(i);
        g.layout = None;
        g.zoom = false;
        if g.focus == tab {
            g.focus = g
                .panes
                .get(i)
                .or(g.panes.last())
                .cloned()
                .unwrap_or_default();
        }
        let (group, focus) = (g.id.clone(), g.focus.clone());
        let dissolved = g.panes.len() <= 1;
        if dissolved {
            self.0.remove(gi);
        }
        Some(Removal {
            group,
            dissolved,
            focus,
        })
    }

    /// `tab` takes the place of `old` in `old`'s group; `old` stays a tab.
    pub fn replace(&mut self, old: &str, tab: &str) -> Result<(), AddError> {
        if old == tab
            || self
                .group_of(old)
                .is_some_and(|g| g.panes.iter().any(|p| p == tab))
        {
            return Err(AddError::Nothing);
        }
        if self.group_of(old).is_none() {
            return Err(AddError::Nothing);
        }
        // `tab` leaves its group; that cannot dissolve `old`'s group, which is another one.
        self.remove(tab);
        let g = self.group_of_mut(old).ok_or(AddError::Nothing)?;
        let i = g.panes.iter().position(|p| p == old).unwrap();
        g.panes[i] = tab.to_string();
        if g.focus == old {
            g.focus = tab.to_string();
        }
        g.layout = None;
        Ok(())
    }

    /// Ends the group; its tabs stay. Returns its panes.
    pub fn dissolve(&mut self, gid: &str) -> Vec<String> {
        match self.0.iter().position(|g| g.id == gid) {
            Some(i) => self.0.remove(i).panes,
            None => Vec::new(),
        }
    }

    /// The tab has focus now: remember it in its group.
    pub fn set_focus(&mut self, tab: &str) {
        if let Some(g) = self.group_of_mut(tab) {
            g.focus = tab.to_string();
        }
    }

    /// Maximise / restore the group of `tab`. Returns the new state, `None` when not in a group.
    pub fn toggle_zoom(&mut self, tab: &str) -> Option<bool> {
        let g = self.group_of_mut(tab)?;
        g.zoom = !g.zoom;
        Some(g.zoom)
    }

    pub fn clear_zoom(&mut self) {
        self.0.iter_mut().for_each(|g| g.zoom = false);
    }

    pub fn zoomed(&self, tab: &str) -> bool {
        self.group_of(tab).is_some_and(|g| g.zoom)
    }

    /// The panes on screen are `leaves` (layout order) and `focus` has focus.
    /// Makes the groups agree: two or more leaves are one group (made when
    /// missing), a lone leaf is in no group, leaves leave other groups, and
    /// members that are no longer shown become plain tabs. Panes beyond the cap
    /// are returned: the view takes them off the screen.
    pub fn sync_visible(&mut self, leaves: &[String], focus: &str) -> Vec<String> {
        if leaves.len() < 2 {
            if let Some(t) = leaves.first() {
                self.remove(t);
            }
            return Vec::new();
        }
        let keep = leaves.len().min(MAX_PANES);
        let (shown, extra) = leaves.split_at(keep);
        let gid = shown
            .iter()
            .find_map(|t| self.group_of(t).map(|g| g.id.clone()));
        let gid = gid.unwrap_or_else(|| {
            let id = new_gid();
            self.0.push(Group {
                id: id.clone(),
                ..Default::default()
            });
            id
        });
        for t in shown {
            if self.group_of(t).is_some_and(|g| g.id != gid) {
                self.remove(t);
            }
        }
        // `remove` can dissolve other groups only, never `gid` (it keeps >= 2 panes or is new).
        let g = self.get_mut(&gid).unwrap();
        if g.panes != shown {
            g.layout = None;
        }
        g.panes = shown.to_vec();
        g.focus = if shown.iter().any(|p| p == focus) {
            focus.to_string()
        } else {
            shown[0].clone()
        };
        extra.to_vec()
    }

    /// Forgets tabs that do not exist (any more) and dissolves what is left too small.
    pub fn prune(&mut self, exists: impl Fn(&str) -> bool) {
        for g in &mut self.0 {
            let n = g.panes.len();
            g.panes.retain(|p| exists(p));
            if g.panes.len() != n {
                g.layout = None;
            }
            if !g.panes.contains(&g.focus) {
                g.focus = g.panes.first().cloned().unwrap_or_default();
            }
            g.panes.truncate(MAX_PANES);
        }
        self.0.retain(|g| g.panes.len() >= 2);
        // A tab in two groups stays in the first.
        let mut seen: Vec<String> = Vec::new();
        for g in &mut self.0 {
            let n = g.panes.len();
            g.panes.retain(|p| !seen.contains(p));
            if g.panes.len() != n {
                g.layout = None;
            }
            seen.extend(g.panes.iter().cloned());
        }
        self.0.retain(|g| g.panes.len() >= 2);
    }

    /// A file written before groups existed has one split layout: its leaves become the group.
    pub fn from_legacy(&mut self, layout: Option<&LayoutData>) {
        if !self.0.is_empty() {
            return;
        }
        let Some(l) = layout else { return };
        let mut leaves = Vec::new();
        collect_leaves(l, &mut leaves);
        leaves.dedup();
        if leaves.len() >= 2 {
            let panes: Vec<String> = leaves.into_iter().take(MAX_PANES).collect();
            self.0.push(Group {
                id: new_gid(),
                focus: panes[0].clone(),
                panes,
                ..Default::default()
            });
        }
    }
}

pub fn collect_leaves(l: &LayoutData, out: &mut Vec<String>) {
    match l {
        LayoutData::Tab { tab } => out.push(tab.clone()),
        LayoutData::Split { children, .. } => children.iter().for_each(|c| collect_leaves(c, out)),
    }
}

/// A layout for `panes` when none was saved: 2 side by side (or stacked, per
/// `axis`), 3 = one left + two stacked right, 4 = a 2×2 grid.
pub fn default_layout(panes: &[String], axis: SplitAxis) -> LayoutData {
    let tab = |i: usize| LayoutData::Tab {
        tab: panes[i].clone(),
    };
    let split = |axis, children| LayoutData::Split {
        axis,
        children,
        sizes: Vec::new(),
    };
    match panes.len() {
        0 => LayoutData::Tab { tab: String::new() },
        1 => tab(0),
        2 => split(axis, vec![tab(0), tab(1)]),
        3 => split(
            SplitAxis::Row,
            vec![tab(0), split(SplitAxis::Column, vec![tab(1), tab(2)])],
        ),
        _ => split(
            SplitAxis::Column,
            vec![
                split(SplitAxis::Row, vec![tab(0), tab(1)]),
                split(SplitAxis::Row, vec![tab(2), tab(3)]),
            ],
        ),
    }
}

fn new_gid() -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "g{}-{}",
        crate::store::new_id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spaces::SpacesFile;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn group4() -> Groups {
        let mut g = Groups::default();
        g.add("a", "b", false).unwrap();
        g.add("b", "c", false).unwrap();
        g.add("c", "d", false).unwrap();
        g
    }

    #[test]
    fn add_builds_a_group_in_order() {
        let mut g = Groups::default();
        g.add("a", "b", false).unwrap();
        g.add("a", "c", true).unwrap();
        assert_eq!(g.0.len(), 1);
        assert_eq!(g.0[0].panes, s(&["c", "a", "b"]));
        assert_eq!(g.0[0].focus, "c");
        assert_eq!(g.add("a", "a", false), Err(AddError::Nothing));
        assert_eq!(g.add("a", "b", false), Err(AddError::Nothing));
    }

    #[test]
    fn cap_of_four() {
        let mut g = group4();
        assert_eq!(g.0[0].panes.len(), MAX_PANES);
        assert!(g.is_full("a"));
        assert_eq!(g.add("a", "e", false), Err(AddError::Full));
        assert_eq!(g.0[0].panes.len(), 4);
        // sync_visible never keeps more than the cap either
        let extra = g.sync_visible(&s(&["a", "b", "c", "d", "e"]), "a");
        assert_eq!(extra, s(&["e"]));
        assert_eq!(g.0[0].panes.len(), 4);
    }

    #[test]
    fn remove_moves_focus_to_a_remaining_pane() {
        let mut g = group4();
        g.0[0].focus = "b".into();
        let r = g.remove("b").unwrap();
        assert!(!r.dissolved);
        assert_eq!(g.0[0].panes, s(&["a", "c", "d"]));
        assert_eq!(r.focus, "c");
        assert_eq!(g.0[0].focus, "c");
        // the last pane removed: focus goes to the one before
        g.0[0].focus = "d".into();
        assert_eq!(g.remove("d").unwrap().focus, "c");
        assert!(g.remove("zzz").is_none());
    }

    #[test]
    fn dissolves_when_one_pane_remains() {
        let mut g = Groups::default();
        g.add("a", "b", false).unwrap();
        let r = g.remove("a").unwrap();
        assert!(r.dissolved);
        assert_eq!(r.focus, "b");
        assert!(g.0.is_empty());
        assert!(g.group_of("b").is_none());
    }

    #[test]
    fn replace_swaps_a_pane_and_leaves_the_old_tab_alone() {
        let mut g = group4();
        g.0[0].focus = "b".into();
        g.replace("b", "e").unwrap();
        assert_eq!(g.0[0].panes, s(&["a", "e", "c", "d"]));
        assert_eq!(g.0[0].focus, "e");
        assert!(g.group_of("b").is_none());
        assert_eq!(g.replace("a", "c"), Err(AddError::Nothing));
        // a tab from another group leaves that group (which dissolves)
        let mut h = Groups::default();
        h.add("a", "b", false).unwrap();
        h.add("x", "y", false).unwrap();
        h.replace("a", "x").unwrap();
        assert_eq!(h.0.len(), 1);
        assert_eq!(h.0[0].panes, s(&["x", "b"]));
        assert!(h.group_of("y").is_none());
    }

    #[test]
    fn dissolve_keeps_tabs() {
        let mut g = group4();
        let id = g.0[0].id.clone();
        assert_eq!(g.dissolve(&id), s(&["a", "b", "c", "d"]));
        assert!(g.0.is_empty());
        assert!(g.dissolve(&id).is_empty());
    }

    #[test]
    fn tab_moves_between_groups() {
        let mut g = Groups::default();
        g.add("a", "b", false).unwrap();
        g.add("c", "d", false).unwrap();
        g.add("c", "b", false).unwrap(); // b leaves {a,b}: that one dissolves
        assert_eq!(g.0.len(), 1);
        assert_eq!(g.0[0].panes, s(&["c", "b", "d"]));
    }

    #[test]
    fn sync_visible_follows_the_screen() {
        let mut g = Groups::default();
        g.add("a", "b", false).unwrap();
        g.add("a", "c", false).unwrap();
        // c was dropped from the screen
        g.sync_visible(&s(&["a", "b"]), "b");
        assert_eq!(g.0[0].panes, s(&["a", "b"]));
        assert_eq!(g.0[0].focus, "b");
        assert!(g.group_of("c").is_none());
        // a lone leaf is no group
        g.sync_visible(&s(&["a"]), "a");
        assert!(g.0.is_empty());
        // two leaves with no group make one
        g.sync_visible(&s(&["x", "y"]), "y");
        assert_eq!(g.0[0].panes, s(&["x", "y"]));
    }

    #[test]
    fn zoom_toggles_and_is_not_serialised() {
        let mut g = Groups::default();
        g.add("a", "b", false).unwrap();
        assert_eq!(g.toggle_zoom("a"), Some(true));
        assert!(g.zoomed("b"));
        assert_eq!(g.toggle_zoom("zzz"), None);
        let json = serde_json::to_string(&g).unwrap();
        assert!(!json.contains("zoom"));
        let back: Groups = serde_json::from_str(&json).unwrap();
        assert!(!back.zoomed("a"));
        g.clear_zoom();
        assert!(!g.zoomed("a"));
    }

    #[test]
    fn prune_drops_missing_tabs() {
        let mut g = group4();
        g.prune(|t| t != "b" && t != "c");
        assert_eq!(g.0[0].panes, s(&["a", "d"]));
        g.prune(|t| t == "a");
        assert!(g.0.is_empty());
        let mut g = group4();
        g.prune(|t| t != "b");
        assert_eq!(g.0[0].panes, s(&["a", "c", "d"]));
    }

    #[test]
    fn default_layouts_have_every_pane_once() {
        for n in 1..=4 {
            let p: Vec<String> = (0..n).map(|i| format!("t{i}")).collect();
            let mut out = Vec::new();
            collect_leaves(&default_layout(&p, SplitAxis::Row), &mut out);
            assert_eq!(out, p);
        }
    }

    #[test]
    fn old_spaces_json_split_layout_becomes_a_group() {
        let old = r#"{"spaces":[{"id":"s","name":"P","tabs":[{"id":"t1"},{"id":"t2"},{"id":"t3"}],
            "active_tab":"t1","focused_pane":0,
            "layout":{"kind":"split","axis":"row","sizes":[300.0,300.0],
                      "children":[{"kind":"tab","tab":"t1"},{"kind":"tab","tab":"t2"}]}}]}"#;
        let f: SpacesFile = serde_json::from_str(old).unwrap();
        let sp = &f.spaces[0];
        assert!(sp.groups.0.is_empty());
        let mut g = sp.groups.clone();
        g.from_legacy(sp.layout.as_ref());
        assert_eq!(g.0.len(), 1);
        assert_eq!(g.0[0].panes, s(&["t1", "t2"]));
        // a single tab layout makes none
        let mut g = Groups::default();
        g.from_legacy(Some(&LayoutData::Tab { tab: "t1".into() }));
        assert!(g.0.is_empty());
        // more than four leaves keep four
        let l = LayoutData::Split {
            axis: SplitAxis::Row,
            sizes: vec![],
            children: (0..6)
                .map(|i| LayoutData::Tab {
                    tab: format!("t{i}"),
                })
                .collect(),
        };
        let mut g = Groups::default();
        g.from_legacy(Some(&l));
        assert_eq!(g.0[0].panes.len(), 4);
    }

    #[test]
    fn groups_round_trip_in_spaces_file() {
        let mut g = Groups::default();
        g.add("a", "b", false).unwrap();
        g.0[0].open = true;
        g.0[0].layout = Some(default_layout(&s(&["a", "b"]), SplitAxis::Column));
        let json = serde_json::to_string(&g).unwrap();
        let back: Groups = serde_json::from_str(&json).unwrap();
        assert_eq!(back, g);
        // a file without the field loads, a file with it keeps it
        let f: SpacesFile = serde_json::from_str(r#"{"spaces":[{"id":"s","name":"P"}]}"#).unwrap();
        assert!(f.spaces[0].groups.0.is_empty());
        let mut f = f;
        f.spaces[0].groups = g.clone();
        let again: SpacesFile = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(again.spaces[0].groups, g);
    }
}
