//! Diagram tabs (Strata v3): what a tab remembers (`DiagramState`), loading
//! the schema into `er_diagram::ErDiagram`, the view's events (line click ->
//! JOIN, right-click menu, double-click -> query), and the operations the card
//! header calls (depth, whole schema, zoom, auto arrange, Mermaid / DDL copy).
//!
//! Layout files (`%APPDATA%\Locus\diagrams\`):
//! - `<connection>__<schema>.json`: positions + camera of the whole schema
//!   (the format the first diagram version wrote; still read and written).
//! - `tab_<tab id>.json`: positions + camera of a centred / listed diagram.

use crate::query::tab::{QueryTab, TabEvent, empty};
use db_core::{DbObjectKind, ErrorKind};
use er_diagram::{Density, Depth, EdgeStyle, ErDiagram, ErDiagramEvent, Scope};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::*;
use locus_core::status::error_title;
use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// What a diagram tab keeps in `spaces.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DiagramState {
    #[serde(default)]
    pub scope: Scope,
    /// Table keys removed from the canvas.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
    /// Full columns, keys only or headers only.
    #[serde(default, skip_serializing_if = "is_full")]
    pub density: Density,
    /// Cards whose chevron flipped them (table keys).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collapsed: Vec<String>,
    /// Curves (default) or rounded orthogonal lines.
    #[serde(default, skip_serializing_if = "is_curved")]
    pub edge_style: EdgeStyle,
}

fn is_full(d: &Density) -> bool {
    *d == Density::Full
}

fn is_curved(s: &EdgeStyle) -> bool {
    *s == EdgeStyle::Bezier
}

impl DiagramState {
    pub fn from_data(d: &locus_core::spaces::TabData) -> Self {
        if let Some(s) = d
            .diagram_state
            .as_ref()
            .and_then(|v| serde_json::from_value::<DiagramState>(v.clone()).ok())
        {
            return s;
        }
        // First version: `diagram_tables` held the tables whose neighbours were shown.
        let schema = d.diagram.as_ref().map(|(_, s)| s.as_str()).unwrap_or("");
        let key = |t: &String| format!("{schema}.{t}");
        let scope = match d.diagram_tables.as_deref() {
            Some([one]) => Scope::Centre {
                table: key(one),
                depth: Depth::Hops(1),
            },
            Some(list) if !list.is_empty() => Scope::Tables(list.iter().map(key).collect()),
            _ => Scope::All,
        };
        Self {
            scope,
            ..Default::default()
        }
    }
}

/// `%APPDATA%\Locus\diagrams\<connection>__<schema>.json`.
fn layout_path(conn: &str, schema: &str) -> Option<std::path::PathBuf> {
    locus_core::settings::app_dir().map(|d| {
        d.join("diagrams")
            .join(format!("{}__{}.json", clean(conn), clean(schema)))
    })
}

fn tab_layout_path(tab: &str) -> Option<std::path::PathBuf> {
    locus_core::settings::app_dir()
        .map(|d| d.join("diagrams").join(format!("tab_{}.json", clean(tab))))
}

fn clean(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn read_layout(p: Option<std::path::PathBuf>) -> Option<er_diagram::DiagramLayout> {
    p.and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
}

/// Diagram options shared by every diagram tab (`diagrams\view.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiagramPrefs {
    #[serde(default = "yes")]
    pub minimap: bool,
}

fn yes() -> bool {
    true
}

impl Default for DiagramPrefs {
    fn default() -> Self {
        Self { minimap: true }
    }
}

fn prefs_path() -> Option<std::path::PathBuf> {
    locus_core::settings::app_dir().map(|d| d.join("diagrams").join("view.json"))
}

pub fn read_prefs() -> DiagramPrefs {
    prefs_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_prefs(p: &DiagramPrefs) {
    if let (Some(path), Ok(b)) = (prefs_path(), serde_json::to_vec_pretty(p)) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = locus_core::settings::write_atomic(&path, &b) {
            log::warn!("diagram options could not be saved: {e}");
        }
    }
}

/// `TableStructure` (driver) -> `TableDetail` (diagram): indexes, column
/// defaults and comments.
pub fn table_detail(st: &db_core::structure::TableStructure) -> er_diagram::TableDetail {
    er_diagram::TableDetail {
        indexes: st
            .indexes
            .iter()
            .map(|i| er_diagram::Index {
                name: i.name.clone(),
                columns: i.columns.clone(),
                unique: i.unique,
                primary: i.primary,
            })
            .collect(),
        comment: None,
        columns: st
            .columns
            .iter()
            .map(|c| (c.name.clone(), c.default.clone(), c.comment.clone()))
            .collect(),
    }
}

pub fn diagram_labels() -> er_diagram::Labels {
    er_diagram::Labels {
        empty: t!("diagram.empty").to_string().into(),
        fit: t!("diagram.fit").to_string().into(),
        reset_zoom: t!("diagram.reset_zoom").to_string().into(),
        relayout: t!("diagram.relayout").to_string().into(),
        search: t!("erv3.search").to_string().into(),
    }
}

/// `schema.name` -> `name`.
pub fn bare(key: &str) -> &str {
    key.split_once('.').map_or(key, |(_, n)| n)
}

impl QueryTab {
    /// Scope on screen (the view's), else the saved one.
    pub fn diagram_scope(&self, cx: &App) -> Scope {
        match &self.dgm.view {
            Some(v) => v.read(cx).scope().clone(),
            None => self.dgm.state.scope.clone(),
        }
    }

    /// Scope and hidden tables as they are now (what `spaces.json` stores).
    pub fn diagram_state_now(&self, cx: &App) -> DiagramState {
        match &self.dgm.view {
            Some(v) => {
                let v = v.read(cx);
                DiagramState {
                    scope: v.scope().clone(),
                    hidden: v.hidden_tables(),
                    density: v.density(),
                    collapsed: v.collapsed_cards(),
                    edge_style: v.edge_style(),
                }
            }
            None => self.dgm.state.clone(),
        }
    }

    /// Old versions read a centred tab as "relations of one table".
    pub fn diagram_legacy_tables(&self, cx: &App) -> Option<Vec<String>> {
        match self.diagram_scope(cx) {
            Scope::Centre { table, .. } => Some(vec![bare(&table).to_string()]),
            _ => None,
        }
    }

    pub fn diagram_title(&self, cx: &App) -> String {
        match self.diagram_scope(cx) {
            Scope::Centre { table, .. } => {
                t!("diagram.relations_title", table = bare(&table).to_string()).into_owned()
            }
            Scope::Tables(_) => t!("erv3.query_title").into_owned(),
            Scope::All => t!(
                "diagram.title",
                schema = self
                    .diagram
                    .as_ref()
                    .map(|d| d.1.clone())
                    .unwrap_or_default()
            )
            .into_owned(),
        }
    }

    /// "whole schema · 12 tables" / "centred on orders · 4 tables" / "tables of this query · 3 tables".
    pub fn diagram_scope_text(&self, cx: &App) -> String {
        let n = self
            .dgm
            .view
            .as_ref()
            .map(|v| v.read(cx).visible_count())
            .unwrap_or(0);
        let what = match self.diagram_scope(cx) {
            Scope::All => t!("erv3.scope_all").into_owned(),
            Scope::Centre { table, .. } => {
                t!("erv3.scope_centre", table = bare(&table).to_string()).into_owned()
            }
            Scope::Tables(_) => t!("erv3.scope_tables").into_owned(),
        };
        t!("erv3.scope_n", scope = what, n = n.to_string()).into_owned()
    }

    /// Depth of a centred diagram.
    pub fn diagram_depth(&self, cx: &App) -> Option<Depth> {
        match self.diagram_scope(cx) {
            Scope::Centre { depth, .. } => Some(depth),
            _ => None,
        }
    }

    /// (tables on the canvas, foreign keys between them).
    pub fn diagram_counts(&self, cx: &App) -> Option<(usize, usize)> {
        let v = self.dgm.view.as_ref()?.read(cx);
        Some((v.visible_count(), v.visible_relation_count()))
    }

    pub fn diagram_zoom_pct(&self, cx: &App) -> Option<i32> {
        self.dgm
            .view
            .as_ref()
            .map(|v| (v.read(cx).zoom() * 100.0).round() as i32)
    }

    /// The driver can produce CREATE TABLE text.
    pub fn diagram_supports_ddl(&self) -> bool {
        use db_core::Driver as _;
        self.dgm
            .driver
            .as_ref()
            .and_then(|d| d.catalog())
            .is_some_and(|c| c.supported_kinds().contains(&DbObjectKind::Table))
    }

    fn quote(&self, s: &str) -> String {
        match self.dgm.driver.as_deref() {
            Some(locus_core::driver::AnyDriver::Mysql(_)) => {
                format!("`{}`", s.replace('`', "``"))
            }
            _ => format!("\"{}\"", s.replace('"', "\"\"")),
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(TabEvent::Changed);
        cx.emit(TabEvent::DiagramChanged);
        cx.notify();
    }

    // ---- header operations ----------------------------------------------------

    pub fn diagram_set_depth(&mut self, depth: Depth, cx: &mut Context<Self>) {
        let (Some(v), Scope::Centre { table, .. }) =
            (self.dgm.view.clone(), self.diagram_scope(cx))
        else {
            return;
        };
        v.update(cx, |v, cx| v.set_scope(Scope::Centre { table, depth }, cx));
        self.changed(cx);
    }

    pub fn diagram_show_whole(&mut self, cx: &mut Context<Self>) {
        let Some(v) = self.dgm.view.clone() else {
            return;
        };
        v.update(cx, |v, cx| v.set_scope(Scope::All, cx));
        self.changed(cx);
    }

    pub fn diagram_zoom(&mut self, dir: i8, cx: &mut Context<Self>) {
        let Some(v) = self.dgm.view.clone() else {
            return;
        };
        v.update(cx, |v, cx| {
            if dir > 0 {
                v.zoom_in(cx)
            } else {
                v.zoom_out(cx)
            }
        });
    }

    pub fn diagram_auto_arrange(&mut self, cx: &mut Context<Self>) {
        let Some(v) = self.dgm.view.clone() else {
            return;
        };
        v.update(cx, |v, cx| v.auto_arrange(cx));
    }

    pub fn diagram_density(&self, cx: &App) -> Option<Density> {
        self.dgm.view.as_ref().map(|v| v.read(cx).density())
    }

    pub fn diagram_set_density(&mut self, d: Density, cx: &mut Context<Self>) {
        let Some(v) = self.dgm.view.clone() else {
            return;
        };
        v.update(cx, |v, cx| v.set_density(d, cx));
    }

    pub fn diagram_edge_style(&self, cx: &App) -> Option<EdgeStyle> {
        self.dgm.view.as_ref().map(|v| v.read(cx).edge_style())
    }

    pub fn diagram_set_edge_style(&mut self, s: EdgeStyle, cx: &mut Context<Self>) {
        let Some(v) = self.dgm.view.clone() else {
            return;
        };
        v.update(cx, |v, cx| v.set_edge_style(s, cx));
    }

    pub fn diagram_minimap(&self, cx: &App) -> bool {
        self.dgm.view.as_ref().is_some_and(|v| v.read(cx).minimap())
    }

    pub fn diagram_toggle_minimap(&mut self, cx: &mut Context<Self>) {
        let Some(v) = self.dgm.view.clone() else {
            return;
        };
        v.update(cx, |v, cx| {
            let on = !v.minimap();
            v.set_minimap(on, cx)
        });
    }

    /// Ctrl+F on a diagram tab: the table search.
    pub fn diagram_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(v) = self.dgm.view.clone() {
            v.update(cx, |v, cx| v.open_search(window, cx));
        }
    }

    /// Esc on a diagram tab: close the search, else clear the selection.
    pub fn diagram_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.dgm.view.clone() {
            Some(v) => v.update(cx, |v, cx| v.escape(window, cx)),
            None => false,
        }
    }

    pub fn diagram_hide(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(v) = self.dgm.view.clone() else {
            return;
        };
        v.update(cx, |v, cx| v.hide_table(key, cx));
        self.changed(cx);
    }

    pub fn diagram_has_hidden(&self, cx: &App) -> bool {
        self.dgm
            .view
            .as_ref()
            .is_some_and(|v| !v.read(cx).hidden_tables().is_empty())
    }

    pub fn diagram_show_hidden(&mut self, cx: &mut Context<Self>) {
        let Some(v) = self.dgm.view.clone() else {
            return;
        };
        v.update(cx, |v, cx| v.show_hidden_tables(cx));
        self.changed(cx);
    }

    pub fn copy_mermaid(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(v) = &self.dgm.view else { return };
        let text = v.read(cx).mermaid();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        crate::shell::toast::show(t!("erv3.copied_mermaid").to_string(), cx);
    }

    /// CREATE TABLE text of the tables on the canvas, one catalog call each.
    pub fn copy_ddl(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(v), Some(driver), Some((_, schema))) = (
            self.dgm.view.clone(),
            self.dgm.driver.clone(),
            self.diagram.clone(),
        ) else {
            return;
        };
        let keys = v.read(cx).visible_tables();
        let n = keys.len();
        cx.spawn_in(window, async move |this, cx| {
            use db_core::Driver as _;
            let mut out = Vec::new();
            let mut err = None;
            if let Some(cat) = driver.catalog() {
                for k in &keys {
                    match cat.ddl(&schema, DbObjectKind::Table, bare(k)).await {
                        Ok(s) => out.push(s.trim_end().trim_end_matches(';').to_string() + ";"),
                        Err(e) => {
                            err = Some(e.message);
                            break;
                        }
                    }
                }
            }
            let _ = this.update_in(cx, |_, _window, cx| match err {
                Some(msg) => {
                    crate::shell::toast::show(t!("objects.ddl_error", msg = msg).to_string(), cx)
                }
                None => {
                    cx.write_to_clipboard(ClipboardItem::new_string(out.join("\n\n")));
                    crate::shell::toast::show(
                        t!("erv3.copied_ddl", n = n.to_string()).to_string(),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    // ---- loading ------------------------------------------------------------------

    /// Reads tables, columns and foreign keys of the schema, then shows them
    /// in the saved scope.
    pub fn load_diagram(
        &mut self,
        driver: Arc<locus_core::driver::AnyDriver>,
        persist: bool,
        cx: &mut Context<Self>,
    ) {
        use db_core::Driver as _;
        let Some((conn, schema)) = self.diagram.clone() else {
            return;
        };
        let state = self.dgm.state.clone();
        let tab_id = self.id.clone();
        self.dgm.driver = Some(driver.clone());
        self.dgm.error = None;
        self.dgm.loading = true;
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let res: Result<er_diagram::DiagramModel, db_core::DbError> = async {
                let Some(cat) = driver.catalog() else {
                    return Err(db_core::DbError::new(
                        ErrorKind::Internal,
                        t!("objects.not_available").to_string(),
                    ));
                };
                let fks = cat.foreign_keys(&schema).await?;
                let objs = cat.objects(&schema).await?;
                let mut tables = Vec::new();
                for o in objs
                    .into_iter()
                    .filter(|o| o.kind == db_core::ObjectKind::Table)
                {
                    let cols = cat.columns(&schema, &o.name).await?;
                    tables.push((o.name, cols));
                }
                Ok(er_diagram::DiagramModel::from_catalog(
                    &schema, &tables, &fks,
                ))
            }
            .await;
            let (whole, own) = if persist {
                let own = (state.scope != Scope::All)
                    .then(|| read_layout(tab_layout_path(&tab_id)))
                    .flatten();
                (read_layout(layout_path(&conn, &schema)), own)
            } else {
                (None, None)
            };
            let _ = this.update(cx, |t, cx| {
                t.dgm.loading = false;
                match res {
                    Err(e) => t.dgm.error = Some(format!("{}: {}", error_title(e.kind), e.message)),
                    Ok(model) => {
                        let view = cx.new(|cx| {
                            let mut v = ErDiagram::new(model, diagram_labels(), whole, cx);
                            v.set_centre_label(t!("erv3.centre_tag").to_string(), cx);
                            v.set_floating_toolbar(false);
                            v.set_scope(state.scope.clone(), cx);
                            if let Some(own) = &own {
                                v.set_layout(own, cx);
                            }
                            for h in &state.hidden {
                                v.hide_table(h, cx);
                            }
                            v.set_density(state.density, cx);
                            v.set_collapsed_cards(&state.collapsed, cx);
                            v.set_edge_style(state.edge_style, cx);
                            v.set_minimap(read_prefs().minimap, cx);
                            v
                        });
                        t.load_diagram_details(view.clone(), cx);
                        t.dgm.sub = Some(cx.subscribe(
                            &view,
                            move |t: &mut QueryTab, view, ev: &ErDiagramEvent, cx| {
                                t.on_diagram_event(view, ev, persist, cx)
                            },
                        ));
                        t.dgm.view = Some(view);
                        cx.emit(TabEvent::DiagramChanged);
                    }
                }
                cx.notify();
            });
        });
        self.dgm.task = Some(task);
    }

    /// Indexes, defaults and comments of every table (one structure call
    /// each, in the background, a batch at a time): 1:1 marks, "keys only"
    /// cards. Tables whose structure fails just keep their plain columns.
    fn load_diagram_details(&mut self, view: Entity<ErDiagram>, cx: &mut Context<Self>) {
        use db_core::Driver as _;
        let (Some(driver), Some((_, schema))) = (self.dgm.driver.clone(), self.diagram.clone())
        else {
            return;
        };
        if driver.catalog().is_none() {
            return;
        }
        let keys: Vec<String> = view
            .read(cx)
            .model()
            .tables
            .iter()
            .map(|t| t.key())
            .collect();
        let view = view.downgrade();
        self.dgm.details = Some(cx.spawn(async move |_, cx| {
            for chunk in keys.chunks(16) {
                let mut out = std::collections::HashMap::new();
                if let Some(cat) = driver.catalog() {
                    for k in chunk {
                        if let Ok(st) = cat.structure(&schema, bare(k), 500).await {
                            out.insert(k.clone(), table_detail(&st));
                        }
                    }
                }
                if view
                    .update(cx, |v, cx| v.set_table_details(out, cx))
                    .is_err()
                {
                    return;
                }
            }
        }));
    }

    fn on_diagram_event(
        &mut self,
        view: Entity<ErDiagram>,
        ev: &ErDiagramEvent,
        persist: bool,
        cx: &mut Context<Self>,
    ) {
        match ev {
            ErDiagramEvent::OpenTable(key) => {
                if let Some((conn, _)) = self.diagram.clone() {
                    let (s, n) = key.split_once('.').unwrap_or(("", key.as_str()));
                    cx.emit(TabEvent::OpenTable {
                        conn_id: conn,
                        sql: format!("SELECT * FROM {}.{}", self.quote(s), self.quote(n)),
                    });
                }
            }
            ErDiagramEvent::RelationClicked(rel) => {
                let sql = er_diagram::join_clause_with(rel, |s| self.quote(s));
                cx.emit(TabEvent::DiagramJoin { sql });
            }
            ErDiagramEvent::TableContextMenu { table, position } => {
                self.dgm.menu_req = Some((table.clone(), *position));
                cx.notify();
            }
            ErDiagramEvent::ZoomChanged(_) => cx.emit(TabEvent::DiagramChanged),
            ErDiagramEvent::ViewOptionsChanged => {
                let minimap = view.read(cx).minimap();
                if read_prefs().minimap != minimap {
                    write_prefs(&DiagramPrefs { minimap });
                }
                self.changed(cx);
            }
            ErDiagramEvent::LayoutChanged if persist => {
                let Some((conn, schema)) = self.diagram.clone() else {
                    return;
                };
                let all = matches!(view.read(cx).scope(), Scope::All);
                let (layout, path) = if all {
                    (view.read(cx).layout(), layout_path(&conn, &schema))
                } else {
                    (view.read(cx).scope_layout(), tab_layout_path(&self.id))
                };
                // Saved off the UI thread, at most every half second.
                self.dgm.save = Some(cx.spawn(async move |_, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(500))
                        .await;
                    let _ = cx
                        .background_executor()
                        .spawn(async move {
                            if let (Some(p), Ok(b)) = (path, serde_json::to_vec_pretty(&layout)) {
                                if let Some(dir) = p.parent() {
                                    let _ = std::fs::create_dir_all(dir);
                                }
                                if let Err(e) = locus_core::settings::write_atomic(&p, &b) {
                                    log::warn!("diagram layout could not be saved: {e}");
                                }
                            }
                        })
                        .await;
                }));
                cx.emit(TabEvent::Changed);
            }
            _ => {}
        }
    }

    // ---- drawing --------------------------------------------------------------------

    /// The canvas, with the table menu while it is open.
    pub(crate) fn render_diagram(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some((key, pos)) = self.dgm.menu_req.take() {
            self.open_table_menu(key, pos, window, cx);
        }
        if let Some(v) = &self.dgm.view {
            return div()
                .size_full()
                .relative()
                .child(v.clone())
                .children(self.dgm.menu.as_ref().map(|(m, pos, _)| {
                    deferred(anchored().position(*pos).snap_to_window().child(m.clone()))
                        .with_priority(1)
                }))
                .into_any_element();
        }
        let text = match (&self.dgm.error, self.dgm.loading) {
            (Some(e), _) => e.clone(),
            (None, true) => t!("diagram.loading").into_owned(),
            (None, false) => t!("diagram.connecting").into_owned(),
        };
        empty(text, cx)
    }

    /// Right click on a card: centre, open a query, show in the Schema panel, hide.
    fn open_table_menu(
        &mut self,
        key: String,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.entity().downgrade();
        let item =
            |label: String, key: String, f: fn(&mut QueryTab, &str, &mut Context<QueryTab>)| {
                let weak = weak.clone();
                PopupMenuItem::new(label).on_click(move |_, _, cx| {
                    let _ = weak.update(cx, |t, cx| f(t, &key, cx));
                })
            };
        let menu = PopupMenu::build(window, cx, {
            let key = key.clone();
            move |m, _, _| {
                m.item(item(
                    t!("erv3.m_centre").into_owned(),
                    key.clone(),
                    |t, k, cx| {
                        if let Some((conn_id, schema)) = t.diagram.clone() {
                            cx.emit(TabEvent::DiagramCentre {
                                conn_id,
                                schema,
                                table: k.to_string(),
                            });
                        }
                    },
                ))
                .item(item(
                    t!("erv3.m_query").into_owned(),
                    key.clone(),
                    |t, k, cx| {
                        if let Some((conn, _)) = t.diagram.clone() {
                            let (s, n) = k.split_once('.').unwrap_or(("", k));
                            cx.emit(TabEvent::OpenTable {
                                conn_id: conn,
                                sql: format!("SELECT * FROM {}.{}", t.quote(s), t.quote(n)),
                            });
                        }
                    },
                ))
                .item(item(
                    t!("erv3.m_schema").into_owned(),
                    key.clone(),
                    |_, k, cx| {
                        cx.emit(TabEvent::DiagramShowInSchema {
                            table: bare(k).to_string(),
                        })
                    },
                ))
                .separator()
                .item(item(
                    t!("erv3.m_hide").into_owned(),
                    key.clone(),
                    |t, k, cx| t.diagram_hide(k, cx),
                ))
            }
        });
        let sub = cx.subscribe(&menu, |this: &mut QueryTab, _, _: &DismissEvent, cx| {
            this.dgm.menu = None;
            cx.notify();
        });
        menu.read(cx).focus_handle(cx).focus(window, cx);
        self.dgm.menu = Some((menu, pos, sub));
        cx.notify();
    }
}

impl QueryTab {
    /// Header "Fit": fit the whole diagram into the view (may go below the open-time floor).
    pub fn diagram_fit(&mut self, cx: &mut Context<Self>) {
        let Some(v) = self.dgm.view.clone() else {
            return;
        };
        v.update(cx, |v, cx| v.fit_to_view(cx));
    }
}

#[cfg(test)]
mod tests {
    use super::{DiagramState, bare};
    use er_diagram::{Density, Depth, EdgeStyle, Scope};

    fn data(tables: Option<Vec<&str>>, state: Option<DiagramState>) -> locus_core::spaces::TabData {
        locus_core::spaces::TabData {
            id: "t".into(),
            title: None,
            conn_id: Some("c".into()),
            sql: String::new(),
            pinned: false,
            limit: None,
            path: None,
            modified: false,
            diagram: Some(("c".into(), "db".into())),
            diagram_tables: tables.map(|v| v.into_iter().map(String::from).collect()),
            diagram_state: state.map(|s| serde_json::to_value(s).unwrap()),
            page: None,
            obj: None,
        }
    }

    #[test]
    fn old_tab_files_become_scopes() {
        assert_eq!(DiagramState::from_data(&data(None, None)).scope, Scope::All);
        assert_eq!(
            DiagramState::from_data(&data(Some(vec!["orders"]), None)).scope,
            Scope::Centre {
                table: "db.orders".into(),
                depth: Depth::Hops(1)
            }
        );
        assert_eq!(
            DiagramState::from_data(&data(Some(vec!["a", "b"]), None)).scope,
            Scope::Tables(vec!["db.a".into(), "db.b".into()])
        );
    }

    #[test]
    fn new_state_wins_and_round_trips() {
        let st = DiagramState {
            scope: Scope::Centre {
                table: "db.x".into(),
                depth: Depth::All,
            },
            hidden: vec!["db.y".into()],
            density: Density::Keys,
            collapsed: vec!["db.x".into()],
            edge_style: EdgeStyle::Smooth,
        };
        let json = serde_json::to_string(&st).unwrap();
        assert_eq!(serde_json::from_str::<DiagramState>(&json).unwrap(), st);
        assert_eq!(
            DiagramState::from_data(&data(Some(vec!["old"]), Some(st.clone()))),
            st
        );
        // A file written by the first version has neither field.
        assert_eq!(
            serde_json::from_str::<DiagramState>("{}").unwrap(),
            DiagramState::default()
        );
        // ... and so draws curves
        assert_eq!(
            serde_json::from_str::<DiagramState>("{}")
                .unwrap()
                .edge_style,
            EdgeStyle::Bezier
        );
    }

    #[test]
    fn bare_name() {
        assert_eq!(bare("db.orders"), "orders");
        assert_eq!(bare("orders"), "orders");
    }
}
