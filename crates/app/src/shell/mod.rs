//! The main window: title bar, the Space sidebar (connections, vertical tab
//! list, Space switcher) and the work area (one or more split panes, each a
//! query tab with its own editor, results and status bar).

pub(crate) mod dnd;
pub(crate) mod files;
pub(crate) mod groups;
pub(crate) mod header;
pub(crate) mod icons;
pub(crate) mod keymap;
pub(crate) mod layout;
pub(crate) mod menu;
pub(crate) mod motion;
pub(crate) mod pages;
pub(crate) mod persist;
pub(crate) mod rail;
pub(crate) mod scale;
pub(crate) mod seg;
pub(crate) mod shortcuts;
pub(crate) mod sidebar;
pub(crate) mod space_ops;
pub(crate) mod space_popover;
pub(crate) mod statusbar;
pub(crate) mod style;
pub(crate) mod tab_list;
pub(crate) mod tabs;
pub(crate) mod theme;
pub(crate) mod titlebar;
pub(crate) mod toast;
pub(crate) mod welcome;

// The shell's imports are re-exported: feature modules (`crate::query`,
// `crate::palette`, ...) extend `Locus` with `use crate::shell::*;`.

pub(crate) use crate::connections::form::{ConnForm, ConnFormEvent};
pub(crate) use crate::query::tab::{QueryTab, RunMode, TabEvent};
pub(crate) use db_core::{DbError, ErrorKind};
pub(crate) use gpui_kit::component::button::{Button, ButtonVariants as _};
pub(crate) use gpui_kit::component::input::{Input, InputEvent, InputState};
pub(crate) use gpui_kit::component::menu::{
    ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem,
};
pub(crate) use gpui_kit::component::resizable::{
    ResizableState, h_resizable, resizable_panel, v_resizable,
};
pub(crate) use gpui_kit::component::scroll::ScrollableElement as _;
pub(crate) use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, TitleBar, WindowExt as _,
    h_flex, v_flex,
};
pub(crate) use gpui_kit::prelude::FluentBuilder as _;
pub(crate) use gpui_kit::*;
pub(crate) use locus_core::pool::Pool;
pub(crate) use locus_core::settings::Settings;
pub(crate) use locus_core::spaces::{
    LayoutData, RightPanel, SPACE_HUES, SidebarMode, SidebarView, SpaceData, SpacesFile, SplitAxis,
    TabData, default_limit,
};
pub(crate) use locus_core::store::{SavedConnection, Store, Tls};
pub(crate) use rust_i18n::t;
pub(crate) use std::sync::Arc;
pub(crate) use std::time::Duration;

pub(crate) use dnd::DropZone;
pub(crate) use layout::Layout;
pub(crate) use locus_core::groups::Groups;
pub(crate) use shortcuts::Shortcut;
pub(crate) use space_popover::SpacePopover;
pub(crate) use style::PopDialog;
pub(crate) use style::apply_theme;
pub(crate) use style::*;
pub(crate) use tab_list::TabDrag;

pub struct Space {
    pub id: String,
    pub name: String,
    pub color: usize,
    pub icon: Option<String>,
    /// Colours, intensity, light / dark (old hue-only Spaces get a one-colour theme on load).
    pub theme: locus_core::space_theme::SpaceTheme,
    pub connections: Vec<String>,
    pub tabs: Vec<Entity<QueryTab>>,
    pub layout: Layout,
    /// Split groups of this Space; the visible layout is one of them (or a single tab).
    pub groups: Groups,
    pub focused: usize,
    pub view: SidebarView,
    /// Bound folder of .sql files (new files are saved there by default).
    pub folder: Option<String>,
}

impl Space {
    pub(crate) fn panes(&self) -> Vec<String> {
        let mut v = Vec::new();
        self.layout.leaves(&mut v);
        v
    }

    pub(crate) fn focused_tab_id(&self) -> String {
        let p = self.panes();
        p.get(self.focused)
            .or(p.first())
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn tab(&self, id: &str, cx: &App) -> Option<Entity<QueryTab>> {
        self.tabs.iter().find(|t| t.read(cx).id == id).cloned()
    }

    /// Tabs in list order: pinned first, then the rest, each in user order.
    pub(crate) fn ordered(&self, cx: &App) -> Vec<Entity<QueryTab>> {
        let mut v: Vec<_> = self
            .tabs
            .iter()
            .filter(|t| t.read(cx).pinned)
            .cloned()
            .collect();
        v.extend(self.tabs.iter().filter(|t| !t.read(cx).pinned).cloned());
        v
    }

    /// What the tint helpers draw this Space with.
    pub(crate) fn look(&self) -> crate::shell::style::Look {
        crate::shell::style::Look::of(&self.theme)
    }

    /// The Space colour as an accent (dots, icons, active marks): its first colour.
    pub(crate) fn accent(&self, cx: &App) -> Hsla {
        crate::shell::style::tint_color(self.theme.primary(), cx)
    }
}

#[derive(Clone, PartialEq)]
pub(crate) enum Rename {
    Tab(String),
}

pub struct Locus {
    pub(crate) store: Store,
    pub conns: Vec<SavedConnection>,
    pub spaces: Vec<Space>,
    pub active_space: usize,
    pub(crate) sidebar: SidebarMode,
    pub(crate) sidebar_width: f32,
    pub(crate) right_width: f32,
    pub(crate) resizing_right: bool,
    /// The root element's focus: key bindings in `Workspace` need a focused
    /// element inside it, so focus falls back here when the focused one goes away.
    pub(crate) focus: FocusHandle,
    /// Pane and region a dragged tab would land in.
    pub(crate) drop_hint: Option<(String, DropZone)>,
    pub(crate) space_pop: Option<SpacePopover>,
    pub(crate) welcome_url: Entity<InputState>,
    /// Sidebar animation: sequence (restarts it), slide direction, width it starts from.
    pub(crate) anim_seq: u64,
    pub(crate) slide_dir: f32,
    pub(crate) anim_from_w: f32,
    pub(crate) reduce_motion: bool,
    pub(crate) resizing_sidebar: bool,
    pub pool: Pool,
    pub(crate) close_requested: bool,
    pub(crate) form: Option<Entity<ConnForm>>,
    pub(crate) renaming: Option<(Rename, Entity<InputState>, Subscription)>,
    pub(crate) objects: Entity<crate::connections::objects::ObjectTree>,
    pub(crate) history: Entity<crate::history::History>,
    pub(crate) saved: Entity<crate::saved::SavedLib>,
    /// Tabs most recently shown, newest first (palette "recent").
    pub(crate) recent: Vec<String>,
    /// Catalog cache of autocomplete, shared by all tabs.
    pub(crate) completions: crate::query::completion::Cache,
    pub(crate) save_task: Option<Task<()>>,
    pub(crate) tab_subs: Vec<(String, Subscription)>,
    /// Runs once, right after the first frame (bench mode prints `ready` there).
    pub first_frame: Option<Box<dyn FnOnce(&mut Window, &mut App)>>,
    /// Keeps bench-mode state alive; unused otherwise.
    #[cfg(feature = "dev")]
    pub bench: Option<AnyEntity>,
    // ---- v3 shell state
    /// Sidebar "Connections" section expanded.
    pub(crate) conns_open: bool,
    /// Connection rows expanded to their schema list.
    pub(crate) toasts: Entity<toast::ToastHost>,
    /// Tab filter field (shown while the filter toggle is on) and its text.
    pub(crate) tab_filter: Option<(Entity<InputState>, Subscription)>,
    pub(crate) tab_filter_text: String,
    /// Right-hand panel next to the panes.
    pub(crate) right_panel: Option<RightPanel>,
    pub(crate) schema_search: Entity<InputState>,
    /// Server version and round-trip time per open connection.
    pub(crate) conn_meta: std::collections::HashMap<String, crate::connections::meta::ConnMeta>,
    /// Production lock state (memory only).
    pub(crate) prod: crate::prod::ProdState,
    pub(crate) _subs: Vec<Subscription>,
}

pub(crate) fn conn_hue(id: &str) -> f32 {
    let h = id
        .bytes()
        .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
    (h % 360) as f32 / 360.0
}

/// `LOCUS_KEYLOG=<file>` (dev builds only): appends each keystroke, passwords
/// typed into forms included. Never compiled into a normal build.
#[cfg(feature = "dev")]
fn keylog(k: &gpui::Keystroke) {
    use std::io::Write as _;
    if let Some(f) = std::env::var_os("LOCUS_KEYLOG")
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(f)
    {
        let _ = writeln!(f, "{:?} key={:?} ch={:?}", k.modifiers, k.key, k.key_char);
    }
}

impl Locus {
    pub fn new(
        store: Store,
        rt: tokio::runtime::Handle,
        initial_sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let conns = store.load();
        let file = if store.persists() {
            SpacesFile::load()
        } else {
            None
        };
        let file = file
            .filter(|f| !f.spaces.is_empty())
            .unwrap_or_else(|| first_run_file(&conns, initial_sql));

        let mut subs = Vec::new();
        subs.push(cx.observe_window_appearance(window, |_, window, cx| apply_theme(window, cx)));
        // Shortcuts are actions (`keymap`), handled on the root element. They
        // need focus inside it: when the focused element disappears, focus the root.
        subs.push(cx.on_focus_lost(window, |this, window, cx| {
            this.focus.focus(window, cx);
        }));
        // Dev only: an observer (it sees every keystroke, changes nothing).
        #[cfg(feature = "dev")]
        subs.push(cx.observe_keystrokes(|_, ev, _, _| keylog(&ev.keystroke)));
        let me = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            me.update(cx, |this, cx| this.should_close(window, cx))
                .unwrap_or(true)
        });

        // The automatic update check only for the real data folder, not the demo / UI-test
        // scenes (those still read update.json from their LOCUS_DATA_DIR).
        let updates = crate::update::init(store.persists(), matches!(store, Store::Disk), cx);
        subs.push(cx.observe(&updates, |_, _, cx| cx.notify()));

        let objects = cx.new(|_| crate::connections::objects::ObjectTree::new());
        subs.push(cx.subscribe_in(
            &objects,
            window,
            |this, _, ev: &crate::connections::objects::ObjectsEvent, window, cx| {
                this.on_objects_event(ev, window, cx)
            },
        ));
        let history = cx.new(|_| crate::history::History::new(store.persists()));
        let saved = cx.new(|_| crate::saved::SavedLib::new(store.persists()));
        let welcome_url = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("conn.url_placeholder").to_string())
        });
        subs.push(cx.subscribe_in(
            &welcome_url,
            window,
            |this, input, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } | InputEvent::Change = ev {
                    let text = input.read(cx).value().to_string();
                    if locus_core::store::parse_url(&text).is_some() {
                        input.update(cx, |s, cx| s.set_value("", window, cx));
                        this.open_form_with(Some(text), None, window, cx);
                    }
                }
            },
        ));
        let schema_search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("shell.schema_search").to_string())
        });
        subs.push(cx.subscribe_in(
            &schema_search,
            window,
            |this, input, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    let q = input.read(cx).value().to_string();
                    this.objects.update(cx, |o, cx| o.set_filter(&q, cx));
                }
            },
        ));
        crate::redis::set_list_width(file.redis_list_width);
        let mut this = Self {
            store,
            conns,
            spaces: Vec::new(),
            active_space: 0,
            sidebar: file.sidebar,
            sidebar_width: file.sidebar_width.clamp(180.0, 520.0),
            right_width: file.right_width.clamp(200.0, 640.0),
            resizing_right: false,
            resizing_sidebar: false,
            focus: cx.focus_handle(),
            drop_hint: None,
            space_pop: None,
            welcome_url,
            anim_seq: 0,
            slide_dir: 0.0,
            anim_from_w: -1.0,
            reduce_motion: crate::settings::get(cx).reduce_motion,
            pool: Pool::new(rt),
            close_requested: false,
            form: None,
            renaming: None,
            objects,
            history,
            saved,
            recent: Vec::new(),
            completions: Default::default(),
            save_task: None,
            tab_subs: Vec::new(),
            first_frame: None,
            #[cfg(feature = "dev")]
            bench: None,
            conns_open: true,
            toasts: toast::init(cx),
            tab_filter: None,
            tab_filter_text: String::new(),
            right_panel: file
                .right_panel
                .filter(|p| crate::panels::is_registered(*p)),
            schema_search,
            conn_meta: Default::default(),
            prod: Default::default(),
            _subs: subs,
        };
        this.init_prod_locks();
        for sd in &file.spaces {
            let space = this.build_space(sd, window, cx);
            this.spaces.push(space);
        }
        this.active_space = file.active_space.min(this.spaces.len() - 1);
        for i in 0..this.spaces.len() {
            this.sync_space_conns(i, cx);
        }
        this
    }

    /// One restored Space: its tabs, split groups and layout.
    fn build_space(
        &mut self,
        sd: &SpaceData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Space {
        let this = self;
        let mut tabs = Vec::new();
        for td in &sd.tabs {
            tabs.push(this.make_tab(td, window, cx));
        }
        if tabs.is_empty() {
            tabs.push(this.make_tab(
                &this.blank_tab(sd.connections.first().cloned(), cx),
                window,
                cx,
            ));
        }
        let ids: Vec<String> = tabs.iter().map(|t| t.read(cx).id.clone()).collect();
        let mut groups = sd.groups.clone();
        groups.from_legacy(sd.layout.as_ref());
        groups.prune(|t| ids.iter().any(|i| i == t));
        let mut layout = sd.layout.as_ref().map(|l| Layout::from_data(l, cx));
        // Drop a layout that names tabs which no longer exist.
        if let Some(l) = &layout {
            let mut leaves = Vec::new();
            l.leaves(&mut leaves);
            if leaves.iter().any(|x| !ids.contains(x)) {
                layout = None;
            }
        }
        let layout = layout.unwrap_or_else(|| {
            let first = sd
                .active_tab
                .clone()
                .filter(|a| ids.contains(a))
                .unwrap_or_else(|| ids[0].clone());
            Layout::Tab(first)
        });
        let mut space = Space {
            id: sd.id.clone(),
            name: sd.name.clone(),
            color: sd.color,
            icon: sd.icon.clone(),
            theme: locus_core::space_theme::SpaceTheme::of(
                sd.color,
                sd.custom_hue,
                sd.theme.as_ref(),
            ),
            connections: sd
                .connections
                .iter()
                .filter(|c| this.conns.iter().any(|x| &x.id == *c))
                .cloned()
                .collect(),
            tabs,
            layout,
            groups,
            focused: sd.focused_pane,
            view: sd.view,
            folder: sd.folder.clone(),
        };
        if space.panes().len() < 2 {
            // A lone tab that belongs to a group opens the whole group.
            let f = space.focused_tab_id();
            space.show_group_of(&f, cx);
        } else {
            space.sync_groups();
        }
        space
    }

    /// What the object tree asks for.
    fn on_objects_event(
        &mut self,
        ev: &crate::connections::objects::ObjectsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = self;
        match ev {
            crate::connections::objects::ObjectsEvent::OpenTable { conn_id, sql } => {
                this.open_query(conn_id.clone(), sql.clone(), window, cx)
            }
            crate::connections::objects::ObjectsEvent::Connect(id) => {
                this.connect_for_tree(id.clone(), cx)
            }
            crate::connections::objects::ObjectsEvent::OpenSource {
                conn_id,
                schema,
                kind,
                name,
            } => this.open_source(
                conn_id.clone(),
                schema.clone(),
                *kind,
                name.clone(),
                window,
                cx,
            ),
            crate::connections::objects::ObjectsEvent::ImportInto {
                conn_id,
                schema,
                table,
            } => this.open_import(
                conn_id.clone(),
                schema.clone(),
                table.clone(),
                None,
                window,
                cx,
            ),
            crate::connections::objects::ObjectsEvent::ShowStructure {
                conn_id,
                schema,
                table,
            } => this.open_structure(conn_id.clone(), schema.clone(), table.clone(), window, cx),
            crate::connections::objects::ObjectsEvent::ShowDdl {
                conn_id,
                schema,
                kind,
                name,
            } => this.show_ddl(
                conn_id.clone(),
                schema.clone(),
                *kind,
                name.clone(),
                window,
                cx,
            ),
            crate::connections::objects::ObjectsEvent::ExportTable {
                conn_id,
                schema,
                table,
            } => this.export_table(conn_id.clone(), schema.clone(), table.clone(), window, cx),
            crate::connections::objects::ObjectsEvent::ShowRelations {
                conn_id,
                schema,
                table,
            } => this.open_relations(conn_id.clone(), schema.clone(), table.clone(), window, cx),
            crate::connections::objects::ObjectsEvent::OpenConnDiagram(id) => {
                this.open_conn_diagram(id.clone(), window, cx)
            }
            crate::connections::objects::ObjectsEvent::OpenDiagram { conn_id, schema } => {
                this.open_diagram(conn_id.clone(), schema.clone(), window, cx)
            }
        }
    }

    pub(crate) fn blank_tab(&self, conn: Option<String>, cx: &App) -> TabData {
        TabData {
            id: locus_core::store::new_id() + "t",
            title: None,
            conn_id: conn,
            sql: String::new(),
            pinned: false,
            limit: if self.store.persists() {
                crate::settings::get(cx).default_limit()
            } else {
                default_limit()
            },
            path: None,
            modified: false,
            diagram: None,
            diagram_tables: None,
            diagram_state: None,
            page: None,
            obj: None,
        }
    }

    pub(crate) fn make_tab(
        &mut self,
        data: &TabData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<QueryTab> {
        let cache = self.completions.clone();
        let tab = cx.new(|cx| QueryTab::new(data, cache, window, cx));
        self.point_completion(&tab, cx);
        // Production tabs start in Manual transaction mode (setting).
        let mode = self.default_tx_mode(data.conn_id.as_deref(), locus_core::txn::TxMode::Auto, cx);
        tab.update(cx, |t, _| t.tx.mode = mode);
        self.attach_page(&tab, data, window, cx);
        self.attach_object(&tab, data, window, cx);
        if data.diagram.is_some() {
            self.start_diagram(tab.clone(), cx);
        }
        let sub = cx.subscribe_in(
            &tab,
            window,
            |this, tab, ev: &TabEvent, window, cx| match ev {
                TabEvent::Rerun => {
                    let sql = tab.read(cx).last_sql.clone();
                    this.run_tab(tab.clone(), RunMode::Script, Some(sql), window, cx)
                }
                TabEvent::RerunView => {
                    if let Some(sql) = tab.update(cx, |t, _| t.view_sql()) {
                        this.run_tab(tab.clone(), RunMode::Script, Some(sql), window, cx)
                    }
                }
                TabEvent::Changed => this.schedule_save(cx),
                TabEvent::ApplyRequest => this.request_apply(tab.clone(), window, cx),
                TabEvent::CompileSource => this.compile_source(tab.clone(), window, cx),
                TabEvent::ReloadSource => this.reload_source(tab.clone(), window, cx),
                TabEvent::ApplyDone => {
                    if let Some(id) = tab.read(cx).conn_id.clone() {
                        this.after_run(&id, cx);
                    }
                }
                TabEvent::OpenTable { conn_id, sql } => {
                    this.open_query(conn_id.clone(), sql.clone(), window, cx)
                }
                TabEvent::ShowQueryRelations { conn_id, tables } => {
                    this.open_query_relations(conn_id.clone(), tables.clone(), window, cx)
                }
                TabEvent::DiagramCentre {
                    conn_id,
                    schema,
                    table,
                } => this.open_relations(
                    conn_id.clone(),
                    schema.clone(),
                    crate::diagram::tab::bare(table).to_string(),
                    window,
                    cx,
                ),
                TabEvent::ShowStructure {
                    conn_id,
                    schema,
                    table,
                } => {
                    this.open_structure(conn_id.clone(), schema.clone(), table.clone(), window, cx)
                }
                TabEvent::OpenTrigger {
                    conn_id,
                    schema,
                    name,
                } => this.open_source(
                    conn_id.clone(),
                    schema.clone(),
                    db_core::DbObjectKind::Trigger,
                    name.clone(),
                    window,
                    cx,
                ),
                TabEvent::DiagramJoin { sql } => this.insert_join(sql.clone(), window, cx),
                TabEvent::DiagramSql { conn_id, sql } => {
                    this.insert_diagram_sql(sql.clone(), Some(conn_id.clone()), window, cx)
                }
                TabEvent::DiagramShowInSchema { table } => {
                    this.show_in_schema(table.clone(), window, cx)
                }
                TabEvent::DiagramChanged => cx.notify(),
                TabEvent::Finished(e) => {
                    let e = e.clone();
                    this.after_run(&e.conn_id, cx);
                    this.auto_plan(&e, cx);
                    this.history.update(cx, |h, cx| h.add(e, cx));
                }
            },
        );
        self.tab_subs.push((data.id.clone(), sub));
        tab
    }

    /// Gives each tab of a Space the list of that Space's connections (its picker).
    pub(crate) fn sync_space_conns(&mut self, si: usize, cx: &mut Context<Self>) {
        let list: Vec<SavedConnection> = self.spaces[si]
            .connections
            .iter()
            .filter_map(|id| self.conns.iter().find(|c| &c.id == id).cloned())
            .collect();
        for t in &self.spaces[si].tabs {
            t.update(cx, |t, cx| {
                t.space_conns = list.clone();
                cx.notify();
            });
        }
    }

    pub fn space(&self) -> &Space {
        &self.spaces[self.active_space]
    }

    pub(crate) fn space_mut(&mut self) -> &mut Space {
        &mut self.spaces[self.active_space]
    }

    /// The tab in the focused pane of the active Space.
    pub fn active_tab(&self, cx: &App) -> Entity<QueryTab> {
        let s = self.space();
        s.tab(&s.focused_tab_id(), cx)
            .unwrap_or_else(|| s.tabs[0].clone())
    }

    pub fn active_conn(&self, cx: &App) -> Option<&SavedConnection> {
        let id = self.active_tab(cx).read(cx).conn_id.clone()?;
        self.conns.iter().find(|c| c.id == id)
    }

    // ---- bench accessors ---------------------------------------------------

    #[cfg(feature = "dev")]
    pub fn tab_status<'a>(&'a self, cx: &'a App) -> &'a Entity<crate::query::status::RunStatus> {
        let s = self.space();
        let id = s.focused_tab_id();
        let t = s
            .tabs
            .iter()
            .find(|t| t.read(cx).id == id)
            .unwrap_or(&s.tabs[0]);
        &t.read(cx).status
    }

    #[cfg(feature = "dev")]
    pub fn tab_table<'a>(
        &'a self,
        cx: &'a App,
    ) -> Option<&'a Entity<gpui_kit::component::table::TableState<crate::results::grid::ResultGrid>>>
    {
        let s = self.space();
        let id = s.focused_tab_id();
        let t = s
            .tabs
            .iter()
            .find(|t| t.read(cx).id == id)
            .unwrap_or(&s.tabs[0]);
        t.read(cx).current_table()
    }

    #[cfg(feature = "dev")]
    pub fn tab_editor<'a>(&'a self, cx: &'a App) -> &'a Entity<crate::query::editor::SqlEditor> {
        let s = self.space();
        let id = s.focused_tab_id();
        let t = s
            .tabs
            .iter()
            .find(|t| t.read(cx).id == id)
            .unwrap_or(&s.tabs[0]);
        &t.read(cx).editor
    }

    #[cfg(feature = "dev")]
    pub fn tab_driver(&self, cx: &App) -> Option<Arc<locus_core::driver::AnyDriver>> {
        self.active_tab(cx).read(cx).driver.clone()
    }

    /// Bench mode: a connection that is shown but never written to disk.
    #[cfg(feature = "dev")]
    pub fn add_ephemeral(&mut self, c: SavedConnection, password: String, cx: &mut Context<Self>) {
        if let Err(e) = self.store.set_password(&c, Some(&password)) {
            log::warn!("bench connection password not stored: {e}");
        }
        self.conns.retain(|x| x.id != c.id);
        self.conns.insert(0, c.clone());
        self.init_prod_locks();
        let s = self.space_mut();
        s.connections.retain(|x| x != &c.id);
        s.connections.insert(0, c.id.clone());
        let tab = self.active_tab(cx);
        tab.update(cx, |t, _| {
            if t.conn_id.is_none() {
                t.conn_id = Some(c.id.clone());
            }
        });
        self.point_completion(&tab, cx);
        self.sync_space_conns(self.active_space, cx);
        cx.notify();
    }
}

impl Render for Locus {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(f) = self.first_frame.take() {
            window.on_next_frame(f);
        }
        let theme = cx.theme().clone();
        let t = crate::shell::theme::tokens(cx);
        let look = self.space().look();
        crate::shell::style::set_space_look(look, cx);
        self.sync_space_appearance(window, cx);
        let mut bg = window_bg(look, cx);
        // Three colours: two gradient halves (GPUI gradients have two stops), drawn under
        // everything as the first child.
        let bg_layers = (bg.len() > 1).then(|| {
            bg.drain(..).fold(v_flex().absolute().inset_0(), |v, b| {
                v.child(div().flex_1().w_full().bg(b))
            })
        });
        let root_bg = bg.pop();
        self.sync_objects(cx);
        self.probe_conns(cx);
        self.sync_pages(cx);
        let mut counter = 0;
        let work = if self.space().zoomed() {
            // Maximised: only the focused pane, with the same header (restore button).
            self.render_pane(self.space().focused, &self.space().focused_tab_id(), cx)
        } else {
            self.render_layout(
                &self.space().layout,
                &mut counter,
                &self.space().id.to_string(),
                cx,
            )
        };
        // No connection yet: the welcome card, except over a page tab (Settings, History),
        // which works without one; the card itself links to Settings.
        let page_in_front = self.active_tab(cx).read(cx).pageslot.kind.is_some();
        let work = if self.space().connections.is_empty() && !page_in_front {
            self.render_welcome(cx)
        } else {
            work
        };
        let sidebar = (self.sidebar != SidebarMode::Hidden).then(|| self.render_sidebar(cx));
        // The main card: header, panes + right panel + rail, status bar.
        let card = self.render_main_card(work, cx);
        if window.focused(cx).is_none() {
            self.focus.focus(window, cx);
        }
        let root = v_flex().key_context("Workspace").track_focus(&self.focus);
        keymap::register(root, cx)
            .size_full()
            .relative()
            .when_some(root_bg, |d, b| d.bg(b))
            .children(bg_layers)
            .text_color(t.tx)
            .child(self.render_title_bar(cx))
            .children(self.render_space_popover(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .pr(px(scale::S4))
                    .pb(px(scale::S4))
                    .when(self.sidebar == SidebarMode::Hidden, |h| h.pl(px(scale::S4)))
                    .children(sidebar)
                    // Drag the sidebar edge: width is remembered; narrower than
                    // 140 px snaps to icons, wider again returns to full.
                    .when(self.sidebar != SidebarMode::Hidden, |h| {
                        h.child(
                            div()
                                .id("sidebar-edge")
                                .flex_none()
                                .w(px(6.))
                                .h_full()
                                .cursor(CursorStyle::ResizeLeftRight)
                                .hover(|s| s.bg(theme.border.opacity(0.6)))
                                .when(self.resizing_sidebar, |d| d.bg(theme.border))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.resizing_sidebar = true;
                                        cx.notify();
                                    }),
                                ),
                        )
                    })
                    .child(card),
            )
            // Last child: painted over the card (toasts are absolutely positioned).
            .child(self.toasts.clone())
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| {
                this.on_window_mouse_move(ev, window, cx)
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.on_window_mouse_up(cx)),
            )
    }
}

impl Locus {
    /// The main card: header, panes with the right panel and rail, status bar.
    fn render_main_card(&self, work: AnyElement, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = crate::shell::theme::tokens(cx);
        let look = self.space().look();
        v_flex()
            .id("main-card")
            .flex_1()
            .min_w_0()
            .h_full()
            .rounded(px(scale::R_LG))
            .bg(crate::shell::style::card_bg(look, cx))
            .border_1()
            .border_color(t.ring)
            .shadow(vec![BoxShadow {
                color: t.shadow,
                offset: point(px(0.), px(6.)),
                blur_radius: px(24.),
                spread_radius: px(0.),
                inset: false,
            }])
            .overflow_hidden()
            // A .csv / .xlsx file dropped on the window: import wizard on the active tab's connection.
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                let Some(file) = paths
                    .paths()
                    .iter()
                    .find(|p| locus_core::import::source::is_importable(p))
                    .cloned()
                else {
                    return;
                };
                if let Some(conn) = this.active_tab(cx).read(cx).conn_id.clone() {
                    this.open_import(conn, String::new(), None, Some(file), window, cx);
                }
            }))
            .child(self.render_card_header(cx))
            .children(self.render_prodbar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(div().flex_1().min_w_0().h_full().child(work))
                    .children(self.render_right_panel(cx))
                    .child(self.render_rail(cx)),
            )
            .child(self.render_status_bar(cx))
    }

    /// Dragging a panel edge: the right panel width, or the sidebar width / mode.
    fn on_window_mouse_move(
        &mut self,
        ev: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.resizing_right {
            if ev.pressed_button != Some(MouseButton::Left) {
                self.resizing_right = false;
                self.schedule_save(cx);
            } else {
                // Card right edge is 8 px inside the window, then the 44 px rail.
                let w =
                    f32::from(window.bounds().size.width) - 8.0 - 44.0 - f32::from(ev.position.x);
                self.right_width = w.clamp(200.0, 640.0);
            }
            cx.notify();
            return;
        }
        if !self.resizing_sidebar {
            return;
        }
        if ev.pressed_button != Some(MouseButton::Left) {
            self.resizing_sidebar = false;
            self.schedule_save(cx);
            cx.notify();
            return;
        }
        let x = f32::from(ev.position.x) - 8.0;
        if x < 140.0 {
            self.sidebar = SidebarMode::Icons;
        } else {
            self.sidebar = SidebarMode::Full;
            self.sidebar_width = x.clamp(180.0, 520.0);
        }
        cx.notify();
    }

    /// Releasing the mouse ends a panel resize.
    fn on_window_mouse_up(&mut self, cx: &mut Context<Self>) {
        if self.resizing_right {
            self.resizing_right = false;
            self.schedule_save(cx);
            cx.notify();
        }
        if self.resizing_sidebar {
            self.resizing_sidebar = false;
            self.schedule_save(cx);
            cx.notify();
        }
    }
}

/// First start (or an older install with only connections.json): everything goes into one default Space.
fn first_run_file(conns: &[SavedConnection], initial_sql: &str) -> SpacesFile {
    SpacesFile {
        spaces: vec![SpaceData {
            id: locus_core::store::new_id(),
            name: t!("space.default").into_owned(),
            color: 0,
            icon: None,
            custom_hue: None,
            theme: None,
            connections: conns.iter().map(|c| c.id.clone()).collect(),
            tabs: vec![TabData {
                id: locus_core::store::new_id() + "t",
                title: None,
                conn_id: conns.first().map(|c| c.id.clone()),
                sql: initial_sql.to_string(),
                pinned: false,
                limit: default_limit(),
                path: None,
                modified: false,
                diagram: None,
                diagram_tables: None,
                diagram_state: None,
                page: None,
                obj: None,
            }],
            active_tab: None,
            layout: None,
            focused_pane: 0,
            view: SidebarView::Tabs,
            folder: None,
            groups: Default::default(),
        }],
        active_space: 0,
        sidebar: SidebarMode::Full,
        sidebar_width: locus_core::spaces::default_sidebar_width(),
        right_width: locus_core::spaces::default_right_width(),
        redis_list_width: locus_core::spaces::default_redis_list_width(),
        right_panel: None,
    }
}
