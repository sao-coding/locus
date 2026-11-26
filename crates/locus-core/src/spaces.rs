//! What is restored on restart: Spaces, their connections, their tabs (title,
//! connection, SQL text, pinned), the active tab and the split layout.
//! `%APPDATA%\Locus\spaces.json`. Never result data, never passwords.

use crate::settings;
use serde::{Deserialize, Serialize};

/// The hue a Space draws with: its picked hue, else the preset `color` (index into `SPACE_HUES`).
pub fn space_hue(color: usize, custom: Option<f32>) -> f32 {
    custom
        .map(|h| h.rem_euclid(1.0))
        .unwrap_or(SPACE_HUES[color % SPACE_HUES.len()])
}

/// Space colours, as hues (0..1). The UI derives light / dark shades itself.
pub const SPACE_HUES: &[f32] = &[0.60, 0.33, 0.08, 0.92, 0.75, 0.50, 0.14, 0.0];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TabData {
    pub id: String,
    /// `None` = the default title ("Query N").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conn_id: Option<String>,
    #[serde(default)]
    pub sql: String,
    #[serde(default)]
    pub pinned: bool,
    /// Row limit; `None` = unlimited. Missing in older files = the default.
    #[serde(default = "default_limit")]
    pub limit: Option<u64>,
    /// The .sql file this tab was opened from or saved to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Changed since it was opened, saved or created.
    #[serde(default)]
    pub modified: bool,
    /// A pane showing the relationship diagram of `(connection, schema)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagram: Option<(String, String)>,
    /// Table-centred diagram: the tables whose neighbours are shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagram_tables: Option<Vec<String>>,
    /// Scope and hidden tables of a diagram tab (Strata v3), kept as JSON here:
    /// the type (`DiagramState` in the app's `diagram/tab.rs`) uses `er_diagram`,
    /// which needs GPUI. Positions and camera live in the diagram layout files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagram_state: Option<serde_json::Value>,
    /// A page instead of an editor: Query History (one per Space) or Settings
    /// (one per window). Such a tab never joins a split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<TabPage>,
    /// A Source or Structure tab: which database object it shows (the connection is `conn_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obj: Option<ObjRef>,
}

/// The object behind a Source / Structure tab, so the tab returns as such after a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjRef {
    pub tab: ObjTab,
    pub schema: String,
    /// `DbObjectKind` by its debug name (`Package`, `Procedure`, ...); `Table` for Structure.
    pub kind: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjTab {
    Source,
    Structure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabPage {
    History,
    Settings,
    /// A Redis view (key browser, key, console, server info); its spec is in the tab's `sql`.
    Redis,
    /// Server activity: sessions, variables, status of one connection.
    Activity,
    /// Saved queries and snippets (one per window).
    Saved,
    /// A read-only diff (one per file and side); its spec is JSON in the tab's `sql`.
    Diff,
}

pub fn default_limit() -> Option<u64> {
    Some(DEFAULT_ROW_LIMIT)
}

pub const DEFAULT_ROW_LIMIT: u64 = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitAxis {
    /// Panes side by side.
    Row,
    /// Panes stacked.
    Column,
}

/// Split layout of the work area: a tree whose leaves show one tab each.
/// Leaves name what they host by kind, so a pane can later show something
/// other than a query (the relationship diagram).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LayoutData {
    Tab {
        tab: String,
    },
    Split {
        axis: SplitAxis,
        children: Vec<LayoutData>,
        sizes: Vec<f32>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpaceData {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: usize,
    /// A hue (0..1) picked with the colour picker; overrides `color` when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_hue: Option<f32>,
    /// Colours, intensity and light / dark choice. Missing in files from before themes: the
    /// Space then draws with its hue (`SpaceTheme::of`). `custom_hue` keeps the first
    /// colour's hue so an older Locus still shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<crate::space_theme::SpaceTheme>,
    /// Icon name from `SPACE_ICONS`; `None` = the colour dot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default)]
    pub connections: Vec<String>,
    #[serde(default)]
    pub tabs: Vec<TabData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_tab: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<LayoutData>,
    /// Index of the focused pane (leaf order).
    #[serde(default)]
    pub focused_pane: usize,
    /// Sidebar view shown for this Space.
    #[serde(default)]
    pub view: SidebarView,
    /// Folder of .sql files bound to this Space (new files are saved there by default; never holds connection settings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Split groups (Strata v3). Files from before groups have none: their
    /// split layout becomes one group on load (`Groups::from_legacy`).
    #[serde(default, skip_serializing_if = "crate::groups::Groups::is_empty")]
    pub groups: crate::groups::Groups,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpacesFile {
    #[serde(default)]
    pub spaces: Vec<SpaceData>,
    #[serde(default)]
    pub active_space: usize,
    #[serde(default)]
    pub sidebar: SidebarMode,
    /// Width of the full sidebar in pixels (dragged by its edge).
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    /// Right-hand panel shown next to the panes (`None` = closed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_panel: Option<RightPanel>,
    /// Width of the right-hand panel in pixels (dragged by its edge).
    #[serde(default = "default_right_width")]
    pub right_width: f32,
}

/// Views of the sidebar, picked by the icon tabs at its top. More views
/// (history, relationship diagram) are added here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarView {
    #[default]
    Tabs,
    Objects,
    History,
}

impl SidebarView {
    pub const ALL: &[SidebarView] = &[
        SidebarView::Tabs,
        SidebarView::Objects,
        SidebarView::History,
    ];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarMode {
    #[default]
    Full,
    Icons,
    Hidden,
}

impl SpacesFile {
    pub fn load() -> Option<Self> {
        crate::datafile::load_named("spaces.json")
    }

    pub fn save(&self) -> std::io::Result<()> {
        let dir = settings::app_dir().ok_or_else(|| std::io::Error::other("APPDATA is not set"))?;
        std::fs::create_dir_all(&dir)?;
        settings::write_atomic(&dir.join("spaces.json"), &serde_json::to_vec_pretty(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_hue_overrides_the_preset() {
        assert_eq!(space_hue(1, None), SPACE_HUES[1]);
        assert_eq!(space_hue(1, Some(0.25)), 0.25);
        assert!((space_hue(9, Some(1.25)) - 0.25).abs() < 1e-6);
        assert_eq!(space_hue(SPACE_HUES.len() + 2, None), SPACE_HUES[2]);
    }

    #[test]
    fn round_trip_has_no_results_or_passwords() {
        let f = SpacesFile {
            spaces: vec![SpaceData {
                id: "s".into(),
                name: "Proj".into(),
                color: 2,
                custom_hue: None,
                theme: None,
                icon: None,
                connections: vec!["c1".into()],
                tabs: vec![TabData {
                    id: "t".into(),
                    title: None,
                    conn_id: Some("c1".into()),
                    sql: "SELECT 1".into(),
                    pinned: true,
                    limit: None,
                    path: None,
                    modified: false,
                    diagram: None,
                    diagram_tables: None,
                    diagram_state: None,
                    page: None,
                    obj: Some(ObjRef {
                        tab: ObjTab::Source,
                        schema: "APPS".into(),
                        kind: "Package".into(),
                        name: "P".into(),
                    }),
                }],
                active_tab: Some("t".into()),
                layout: Some(LayoutData::Split {
                    axis: SplitAxis::Row,
                    children: vec![
                        LayoutData::Tab { tab: "t".into() },
                        LayoutData::Tab { tab: "t".into() },
                    ],
                    sizes: vec![300., 300.],
                }),
                focused_pane: 1,
                view: SidebarView::Objects,
                folder: None,
                groups: Default::default(),
            }],
            active_space: 0,
            sidebar: SidebarMode::Icons,
            sidebar_width: 300.0,
            right_panel: Some(RightPanel::Schema),
            right_width: 264.0,
        };
        let s = serde_json::to_string(&f).unwrap();
        assert!(!s.contains("password"));
        let back: SpacesFile = serde_json::from_str(&s).unwrap();
        assert_eq!(back.spaces[0].tabs[0].limit, None);
        let old: TabData = serde_json::from_str(r#"{"id":"x"}"#).unwrap();
        assert_eq!(old.limit, Some(DEFAULT_ROW_LIMIT));
        // A Source tab keeps its object through the file; older files without it still load.
        assert_eq!(
            back.spaces[0].tabs[0].obj,
            Some(ObjRef {
                tab: ObjTab::Source,
                schema: "APPS".into(),
                kind: "Package".into(),
                name: "P".into()
            })
        );
        assert!(s.contains(r#""obj":{"tab":"source""#));
        assert_eq!(old.obj, None);
        // A panel this build lacks still loads.
        let old: SpacesFile = serde_json::from_str(r#"{"right_panel":"future-panel"}"#).unwrap();
        assert_eq!(old.right_panel, Some(RightPanel::Unknown));
    }
}

pub fn default_right_width() -> f32 {
    264.0
}

pub fn default_sidebar_width() -> f32 {
    280.0
}

/// What the right-hand panel (opened from the right rail) shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RightPanel {
    /// The database-object tree.
    Schema,
    /// Query history (a later task turns it into a tab).
    History,
    /// Git: changes, commit box, history.
    Git,
    /// A panel this build does not have: opens nothing.
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod theme_file_tests {
    use super::*;
    use crate::space_theme::{Appearance, LEGACY_SAT, SpaceTheme};

    #[test]
    fn a_space_from_before_themes_loads_with_its_hue() {
        let old = r#"{"spaces":[{"id":"s","name":"Old","color":2,"custom_hue":0.3}]}"#;
        let f: SpacesFile = serde_json::from_str(old).unwrap();
        let sd = &f.spaces[0];
        assert!(sd.theme.is_none());
        let t = SpaceTheme::of(sd.color, sd.custom_hue, sd.theme.as_ref());
        assert_eq!(t.colors.len(), 1);
        assert!((t.colors[0].h - 0.3).abs() < 1e-6);
        assert!((t.colors[0].s - LEGACY_SAT).abs() < 1e-6);
    }

    #[test]
    fn a_theme_round_trips_through_the_file() {
        let mut t = SpaceTheme::of(0, None, None);
        t.add_color();
        t.appearance = Appearance::Dark;
        t.intensity = 0.8;
        let sd = SpaceData {
            id: "s".into(),
            name: "N".into(),
            color: 0,
            custom_hue: Some(t.primary().h),
            theme: Some(t.clone()),
            icon: None,
            connections: vec![],
            tabs: vec![],
            active_tab: None,
            layout: None,
            focused_pane: 0,
            view: Default::default(),
            folder: None,
            groups: Default::default(),
        };
        let s = serde_json::to_string(&sd).unwrap();
        assert!(s.contains(r#""appearance":"dark""#), "{s}");
        let back: SpaceData = serde_json::from_str(&s).unwrap();
        assert_eq!(back.theme, Some(t));
    }
}
