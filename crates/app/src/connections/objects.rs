//! "Database objects" sidebar view: per connected connection of the Space, a
//! lazy tree schema -> tables / views -> columns through `Driver::catalog()`.
//! A level is loaded when it is first expanded. The tree is flattened into
//! rows and drawn with `uniform_list`, so only visible rows are laid out
//! (a schema with thousands of tables scrolls without extra work per frame).

use db_core::{
    ColumnInfo, DbObjectKind, Driver as _, InfoItem, ObjectEntry, ObjectKind, Principal,
    SchemaObject,
};

type LKey = (String, String, DbObjectKind);

/// Paging state of one folder: rows asked for, matches on the server (when counted).
#[derive(Clone, Copy)]
struct Page {
    cap: u32,
    total: Option<u64>,
    busy: bool,
}
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use locus_core::driver::AnyDriver;
use rust_i18n::t;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

enum Load<T> {
    Loading,
    Loaded(T),
    Failed(String),
}

/// What a double-click on a table asks the shell for.
pub enum ObjectsEvent {
    OpenTable {
        conn_id: String,
        sql: String,
    },
    /// Expanding a connection that is not open yet: the shell connects.
    Connect(String),
    /// Whole-schema diagram of the schema a connection is on (connection row).
    OpenConnDiagram(String),
    /// "Diagram" on a schema.
    OpenDiagram {
        conn_id: String,
        schema: String,
    },
    /// "Export table…" on a table or view.
    ExportTable {
        conn_id: String,
        schema: String,
        table: String,
    },
    /// "Show relations" on a table.
    ShowRelations {
        conn_id: String,
        schema: String,
        table: String,
    },
    /// "View DDL": open the object's CREATE text in a read-only tab.
    ShowDdl {
        conn_id: String,
        schema: String,
        kind: DbObjectKind,
        name: String,
    },
    /// Double-click / "Edit source" on a package, procedure, function, trigger,
    /// type or event: the Source tab (`crate::routines`).
    OpenSource {
        conn_id: String,
        schema: String,
        kind: DbObjectKind,
        name: String,
    },
    /// "Structure" on a table or view (context menu, hover button).
    ShowStructure {
        conn_id: String,
        schema: String,
        table: String,
    },
    /// "Import data…" on a table, or "Import into new table…" on a schema (`table: None`).
    ImportInto {
        conn_id: String,
        schema: String,
        table: Option<String>,
    },
}

#[derive(Clone)]
enum Row {
    Conn {
        id: String,
        name: String,
        connected: bool,
        open: bool,
    },
    Schema {
        conn: String,
        schema: String,
        open: bool,
    },
    Object {
        conn: String,
        schema: String,
        obj: SchemaObject,
        open: bool,
    },
    /// A category of a schema (Tables, Views, Indexes, ...), with its count once loaded.
    Folder {
        conn: String,
        schema: String,
        kind: DbObjectKind,
        open: bool,
        count: Option<usize>,
        of: Option<u64>,
    },
    /// "First N of M" at the end of a truncated folder, with the load-more button.
    More {
        conn: String,
        schema: String,
        kind: DbObjectKind,
        shown: usize,
        total: Option<u64>,
        busy: bool,
    },
    /// An object that is not a table or view (index, routine, trigger, ...).
    Entry {
        conn: String,
        schema: String,
        kind: DbObjectKind,
        entry: ObjectEntry,
    },
    /// Connection-level folders: users / roles, and server info.
    ConnFolder {
        conn: String,
        info: bool,
        open: bool,
    },
    Principal(Principal),
    Info(InfoItem),
    Column(ColumnInfo),
    Loading,
    Error(String),
    Note(String),
}

pub struct ObjectTree {
    /// (id, name, open driver) of the Space's connections, set by the shell.
    conns: Vec<(String, String, Option<Arc<AnyDriver>>)>,
    expanded: HashSet<String>,
    schemas: HashMap<String, Load<Vec<String>>>,
    objects: HashMap<(String, String), Load<Vec<SchemaObject>>>,
    columns: HashMap<(String, String, String), Load<Vec<ColumnInfo>>>,
    lists: HashMap<LKey, Load<Vec<ObjectEntry>>>,
    pages: HashMap<LKey, Page>,
    /// Server-side name filter results per folder: (filter text, rows).
    found: HashMap<LKey, (String, Load<Vec<ObjectEntry>>)>,
    principals: HashMap<String, Load<Vec<Principal>>>,
    infos: HashMap<String, Load<Vec<InfoItem>>>,
    rows: Vec<(usize, Row)>,
    scroll: UniformListScrollHandle,
    /// Search box of the Schema panel: only loaded rows that match (and their parents) are listed.
    filter: String,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<ObjectsEvent> for ObjectTree {}

fn key(parts: &[&str]) -> String {
    parts.join("\u{1f}")
}

impl ObjectTree {
    pub fn new() -> Self {
        Self {
            conns: Vec::new(),
            expanded: HashSet::new(),
            schemas: HashMap::new(),
            objects: HashMap::new(),
            columns: HashMap::new(),
            lists: HashMap::new(),
            pages: HashMap::new(),
            found: HashMap::new(),
            principals: HashMap::new(),
            infos: HashMap::new(),
            rows: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            filter: String::new(),
            _tasks: Vec::new(),
        }
    }

    /// Called by the shell when the Space or a connection's state changed.
    pub fn set_conns(
        &mut self,
        conns: Vec<(String, String, Option<Arc<AnyDriver>>)>,
        cx: &mut Context<Self>,
    ) {
        let same = conns.len() == self.conns.len()
            && conns
                .iter()
                .zip(&self.conns)
                .all(|(a, b)| a.0 == b.0 && a.1 == b.1 && a.2.is_some() == b.2.is_some());
        if !same {
            self.conns = conns;
            // A connection that was expanded while connecting: load it now.
            let ready: Vec<String> = self
                .conns
                .iter()
                .filter(|c| {
                    c.2.is_some()
                        && self.expanded.contains(&key(&[&c.0]))
                        && !matches!(self.schemas.get(&c.0), Some(Load::Loaded(_)))
                })
                .map(|c| c.0.clone())
                .collect();
            for c in ready {
                self.load_schemas(&c, cx);
            }
            self.rebuild();
            cx.notify();
        }
    }

    fn driver(&self, conn: &str) -> Option<Arc<AnyDriver>> {
        self.conns
            .iter()
            .find(|c| c.0 == conn)
            .and_then(|c| c.2.clone())
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.schemas.clear();
        self.objects.clear();
        self.columns.clear();
        self.lists.clear();
        self.pages.clear();
        self.found.clear();
        self.principals.clear();
        self.infos.clear();
        // Re-load what is open.
        let open: Vec<String> = self.expanded.iter().cloned().collect();
        for k in open {
            let p: Vec<&str> = k.split('\u{1f}').collect();
            match p.len() {
                1 => self.load_schemas(p[0], cx),
                2 => self.load_objects(p[0], p[1], cx),
                // A category folder ("#Package"): its list, or the table list.
                3 if p[2].starts_with('#') => {
                    if let Some(kind) = self
                        .kinds(p[0])
                        .into_iter()
                        .find(|kind| folder_key(p[0], p[1], *kind) == k)
                        && !self.is_table_list(p[0], kind)
                    {
                        self.load_list(p[0], p[1], kind, cx);
                    }
                }
                3 => self.load_columns(p[0], p[1], p[2], cx),
                _ => {}
            }
        }
        self.rebuild();
        cx.notify();
    }

    fn load_schemas(&mut self, conn: &str, cx: &mut Context<Self>) {
        let Some(d) = self.driver(conn) else { return };
        let Some(fut) = d.catalog().map(|c| c.schemas()) else {
            return;
        };
        self.schemas.insert(conn.to_string(), Load::Loading);
        let c = conn.to_string();
        self._tasks.push(cx.spawn(async move |this, cx| {
            let r = fut.await;
            let _ = this.update(cx, |t, cx| {
                t.schemas.insert(
                    c,
                    r.map(Load::Loaded)
                        .unwrap_or_else(|e| Load::Failed(e.message)),
                );
                t.rebuild();
                cx.notify();
            });
        }));
    }

    fn load_objects(&mut self, conn: &str, schema: &str, cx: &mut Context<Self>) {
        let Some(d) = self.driver(conn) else { return };
        let Some(fut) = d.catalog().map(|c| c.objects(schema)) else {
            return;
        };
        let k = (conn.to_string(), schema.to_string());
        self.objects.insert(k.clone(), Load::Loading);
        self._tasks.push(cx.spawn(async move |this, cx| {
            let r = fut.await;
            let _ = this.update(cx, |t, cx| {
                t.objects.insert(
                    k,
                    r.map(Load::Loaded)
                        .unwrap_or_else(|e| Load::Failed(e.message)),
                );
                t.rebuild();
                cx.notify();
            });
        }));
    }

    fn load_columns(&mut self, conn: &str, schema: &str, obj: &str, cx: &mut Context<Self>) {
        let Some(d) = self.driver(conn) else { return };
        let Some(fut) = d.catalog().map(|c| c.columns(schema, obj)) else {
            return;
        };
        let k = (conn.to_string(), schema.to_string(), obj.to_string());
        self.columns.insert(k.clone(), Load::Loading);
        self._tasks.push(cx.spawn(async move |this, cx| {
            let r = fut.await;
            let _ = this.update(cx, |t, cx| {
                t.columns.insert(
                    k,
                    r.map(Load::Loaded)
                        .unwrap_or_else(|e| Load::Failed(e.message)),
                );
                t.rebuild();
                cx.notify();
            });
        }));
    }

    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some((_, row)) = self.rows.get(ix).cloned() else {
            return;
        };
        let (k, load): (String, Box<dyn FnOnce(&mut Self, &mut Context<Self>)>) = match row {
            Row::Conn {
                id,
                connected: true,
                ..
            } => {
                let c = id.clone();
                (
                    key(&[&id]),
                    Box::new(move |t, cx| {
                        if !t.schemas.contains_key(&c) {
                            t.load_schemas(&c, cx)
                        }
                    }),
                )
            }
            Row::Schema { conn, schema, .. } => {
                let (c, s) = (conn.clone(), schema.clone());
                (
                    key(&[&conn, &schema]),
                    Box::new(move |t, cx| {
                        if !t.paged(&c) && !t.objects.contains_key(&(c.clone(), s.clone())) {
                            t.load_objects(&c, &s, cx)
                        }
                    }),
                )
            }
            Row::Object {
                conn, schema, obj, ..
            } => {
                let (c, s, o) = (conn.clone(), schema.clone(), obj.name.clone());
                (
                    key(&[&conn, &schema, &obj.name]),
                    Box::new(move |t, cx| {
                        if !t.columns.contains_key(&(c.clone(), s.clone(), o.clone())) {
                            t.load_columns(&c, &s, &o, cx)
                        }
                    }),
                )
            }
            Row::Conn {
                id,
                connected: false,
                ..
            } => {
                let k = key(&[&id]);
                if !self.expanded.remove(&k) {
                    self.expanded.insert(k);
                    self.schemas.insert(id.clone(), Load::Loading);
                    cx.emit(ObjectsEvent::Connect(id));
                }
                self.rebuild();
                cx.notify();
                return;
            }
            _ => return,
        };
        if !self.expanded.remove(&k) {
            self.expanded.insert(k);
            load(self, cx);
        }
        self.rebuild();
        cx.notify();
    }

    fn open_table(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some((
            _,
            Row::Object {
                conn, schema, obj, ..
            },
        )) = self.rows.get(ix).cloned()
            && let Some(d) = self.driver(&conn)
        {
            let sql = format!(
                "SELECT * FROM {}.{}",
                d.quote_identifier(&schema),
                d.quote_identifier(&obj.name)
            );
            cx.emit(ObjectsEvent::OpenTable { conn_id: conn, sql });
        }
    }

    fn status_rows<T>(
        load: Option<&Load<T>>,
        depth: usize,
        rows: &mut Vec<(usize, Row)>,
    ) -> Option<()> {
        match load {
            None | Some(Load::Loading) => rows.push((depth, Row::Loading)),
            Some(Load::Failed(e)) => rows.push((depth, Row::Error(e.clone()))),
            Some(Load::Loaded(_)) => return Some(()),
        }
        None
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        if self.conns.is_empty() {
            rows.push((0, Row::Note(t!("objects.no_connections").into_owned())));
        }
        for (id, name, d) in &self.conns {
            let open = self.expanded.contains(&key(&[id]));
            let connected = d.is_some();
            rows.push((
                0,
                Row::Conn {
                    id: id.clone(),
                    name: name.clone(),
                    connected,
                    open,
                },
            ));
            if !connected {
                if open {
                    let _ = Self::status_rows(self.schemas.get(id), 1, &mut rows);
                }
                continue;
            }
            if d.as_ref().is_some_and(|d| d.catalog().is_none()) {
                rows.push((1, Row::Note(t!("objects.not_available").into_owned())));
                continue;
            }
            if !open {
                continue;
            }
            let sl = self.schemas.get(id);
            if Self::status_rows(sl, 1, &mut rows).is_none() {
                continue;
            }
            let Some(Load::Loaded(schemas)) = sl else {
                continue;
            };
            for s in schemas {
                self.push_schema_rows(id, s, &mut rows);
            }
            self.push_conn_folder_rows(id, d, &mut rows);
        }
        if !self.filter.is_empty() {
            rows = filter_rows(rows, &self.filter);
        }
        self.rows = rows;
    }

    /// Rows of one schema: its row and, when open, its category folders.
    fn push_schema_rows(&self, id: &str, s: &str, rows: &mut Vec<(usize, Row)>) {
        let (id, s) = (&id.to_string(), &s.to_string());
        let sopen = self.expanded.contains(&key(&[id, s]));
        rows.push((
            1,
            Row::Schema {
                conn: id.clone(),
                schema: s.clone(),
                open: sopen,
            },
        ));
        if !self.expanded.contains(&key(&[id, s])) {
            return;
        }
        // Category folders, as the driver supports them (DBeaver-like).
        for kind in self.kinds(id) {
            self.push_folder_rows(id, s, kind, rows);
        }
    }

    /// Rows of one category folder of a schema: its row and, when open, the objects below.
    fn push_folder_rows(
        &self,
        id: &str,
        s: &str,
        kind: DbObjectKind,
        rows: &mut Vec<(usize, Row)>,
    ) {
        let (id, s) = (&id.to_string(), &s.to_string());
        let ol = self.objects.get(&(id.clone(), s.clone()));
        let fopen = self.expanded.contains(&folder_key(id, s, kind));
        let lk: LKey = (id.clone(), s.clone(), kind);
        let table_like = self.is_table_list(id, kind);
        let of = self.pages.get(&lk).and_then(|p| p.total);
        let count = if table_like {
            match ol {
                Some(Load::Loaded(objs)) => {
                    Some(objs.iter().filter(|o| same_kind(o, kind)).count())
                }
                _ => None,
            }
        } else {
            match self.lists.get(&(id.clone(), s.clone(), kind)) {
                Some(Load::Loaded(v)) => Some(v.len()),
                _ => None,
            }
        };
        rows.push((
            2,
            Row::Folder {
                conn: id.clone(),
                schema: s.clone(),
                kind,
                open: fopen,
                count,
                of,
            },
        ));
        if !fopen {
            return;
        }
        if table_like {
            if Self::status_rows(ol, 3, rows).is_none() {
                return;
            }
            let Some(Load::Loaded(objs)) = ol else {
                return;
            };
            for o in objs.iter().filter(|o| same_kind(o, kind)) {
                let oopen = self.expanded.contains(&key(&[id, s, &o.name]));
                rows.push((
                    3,
                    Row::Object {
                        conn: id.clone(),
                        schema: s.clone(),
                        obj: o.clone(),
                        open: oopen,
                    },
                ));
                if !oopen {
                    continue;
                }
                let cl = self.columns.get(&(id.clone(), s.clone(), o.name.clone()));
                if Self::status_rows(cl, 4, rows).is_none() {
                    continue;
                }
                if let Some(Load::Loaded(cols)) = cl {
                    rows.extend(cols.iter().map(|c| (4, Row::Column(c.clone()))));
                }
            }
        } else {
            let l = self.lists.get(&(id.clone(), s.clone(), kind));
            if Self::status_rows(l, 3, rows).is_none() {
                return;
            }
            if let Some(Load::Loaded(v)) = l {
                // A name filter on a truncated folder: what the server found.
                let v = match self.found.get(&lk) {
                    Some((q, Load::Loaded(f))) if *q == self.filter => f,
                    _ => v,
                };
                if v.is_empty() {
                    rows.push((3, Row::Note(t!("objects.none").into_owned())));
                }
                let table_kind = matches!(kind, DbObjectKind::Table | DbObjectKind::View);
                for e in v {
                    if table_kind {
                        let obj = SchemaObject {
                            name: e.name.clone(),
                            kind: if kind == DbObjectKind::View {
                                ObjectKind::View
                            } else {
                                ObjectKind::Table
                            },
                        };
                        let oopen = self.expanded.contains(&key(&[id, s, &obj.name]));
                        rows.push((
                            3,
                            Row::Object {
                                conn: id.clone(),
                                schema: s.clone(),
                                obj: obj.clone(),
                                open: oopen,
                            },
                        ));
                        if oopen {
                            let cl = self.columns.get(&(id.clone(), s.clone(), obj.name.clone()));
                            if Self::status_rows(cl, 4, rows).is_some()
                                && let Some(Load::Loaded(cols)) = cl
                            {
                                rows.extend(cols.iter().map(|c| (4, Row::Column(c.clone()))));
                            }
                        }
                    } else {
                        rows.push((
                            3,
                            Row::Entry {
                                conn: id.clone(),
                                schema: s.clone(),
                                kind,
                                entry: e.clone(),
                            },
                        ));
                    }
                }
                if self.filter.is_empty() && self.truncated(&lk) {
                    let p = self.pages.get(&lk);
                    rows.push((
                        3,
                        Row::More {
                            conn: id.clone(),
                            schema: s.clone(),
                            kind,
                            shown: v.len(),
                            total: p.and_then(|p| p.total),
                            busy: p.is_some_and(|p| p.busy),
                        },
                    ));
                }
            }
        }
    }

    /// Connection level folders: users / roles and server info.
    fn push_conn_folder_rows(
        &self,
        id: &String,
        d: &Option<Arc<AnyDriver>>,
        rows: &mut Vec<(usize, Row)>,
    ) {
        let sqlite = d
            .as_ref()
            .is_some_and(|d| matches!(**d, AnyDriver::Sqlite(_)));
        for info in [false, true] {
            if sqlite && !info {
                continue;
            }
            let k = key(&[id, if info { "#info" } else { "#principals" }]);
            let fopen = self.expanded.contains(&k);
            rows.push((
                1,
                Row::ConnFolder {
                    conn: id.clone(),
                    info,
                    open: fopen,
                },
            ));
            if !fopen {
                continue;
            }
            if info {
                let l = self.infos.get(id);
                if Self::status_rows(l, 2, rows).is_none() {
                    continue;
                }
                if let Some(Load::Loaded(v)) = l {
                    rows.extend(v.iter().map(|i| (2, Row::Info(i.clone()))));
                }
            } else {
                let l = self.principals.get(id);
                if Self::status_rows(l, 2, rows).is_none() {
                    continue;
                }
                if let Some(Load::Loaded(v)) = l {
                    rows.extend(v.iter().map(|p| (2, Row::Principal(p.clone()))));
                }
            }
        }
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let (depth, row) = &self.rows[ix];
        let base = h_flex()
            .id(("obj", ix))
            .w_full()
            .h(px(24.))
            .pl(px(4. + *depth as f32 * 14.))
            .pr(px(14.))
            .gap_1()
            .items_center()
            .text_sm()
            .rounded_md();
        if matches!(
            row,
            Row::Folder { .. }
                | Row::More { .. }
                | Row::Entry { .. }
                | Row::ConnFolder { .. }
                | Row::Principal(_)
                | Row::Info(_)
        ) {
            return self
                .render_extra(ix, base, cx)
                .unwrap_or_else(|| div().into_any_element());
        }
        match row {
            Row::Conn { .. } => self.render_conn_row(ix, base, cx),
            Row::Schema { .. } => self.render_schema_row(ix, base, cx),
            Row::Object { .. } => self.render_object_row(ix, base, cx),
            Row::Column(c) => base
                .child(
                    div()
                        .w(px(12.))
                        .flex_none()
                        .text_xs()
                        .text_color(theme.warning)
                        .child(if c.primary_key {
                            crate::shell::icons::icon("key-round")
                                .size(px(11.))
                                .into_any_element()
                        } else {
                            div().into_any_element()
                        }),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .max_w(px(120.))
                        .truncate()
                        .child(c.name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{}{}",
                            c.type_name,
                            if c.nullable { " ?" } else { "" }
                        )),
                )
                .into_any_element(),
            Row::Loading => base
                .text_color(theme.muted_foreground)
                .child(crate::shell::style::spinner(theme.muted_foreground))
                .child(t!("objects.loading").to_string())
                .into_any_element(),
            Row::Error(e) => base
                .text_color(theme.danger)
                .child(
                    div()
                        .truncate()
                        .child(t!("objects.error", msg = e.clone()).to_string()),
                )
                .into_any_element(),
            Row::Note(n) => base
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().truncate().child(n.clone()))
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }

    /// Connection row: chevron, name and the hover-only diagram button.
    fn render_conn_row(
        &self,
        ix: usize,
        base: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let chevron = |open: bool| {
            Icon::new(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .xsmall()
            .text_color(theme.muted_foreground)
        };
        let Row::Conn {
            id,
            name,
            open,
            connected,
        } = &self.rows[ix].1
        else {
            return div().into_any_element();
        };
        base.group(SharedString::from(format!("obj-row-{ix}")))
            .cursor_pointer()
            .hover(|s| s.bg(theme.background.opacity(0.5)))
            .when(!*connected, |r| r.opacity(0.55))
            .child(chevron(*open))
            .child(Icon::new(IconName::HardDrive).xsmall())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(name.clone()),
            )
            // Hover only: visibility changes, never display (gpui-fast panics on that).
            .when(*connected, |r| {
                let conn = id.clone();
                r.child(
                    div()
                        .flex_none()
                        .invisible()
                        .group_hover(SharedString::from(format!("obj-row-{ix}")), |s| s.visible())
                        .child(
                            Button::new(("conn-diagram", ix))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Network)
                                .tooltip(t!("erv3.conn_open").to_string())
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(ObjectsEvent::OpenConnDiagram(conn.clone()));
                                })),
                        ),
                )
            })
            .on_click(cx.listener(move |t, _, _, cx| t.toggle(ix, cx)))
            .into_any_element()
    }

    /// Schema row: chevron, name, diagram button and import menu.
    fn render_schema_row(
        &self,
        ix: usize,
        base: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let chevron = |open: bool| {
            Icon::new(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .xsmall()
            .text_color(theme.muted_foreground)
        };
        let Row::Schema {
            conn: conn_of_schema,
            schema,
            open,
        } = &self.rows[ix].1
        else {
            return div().into_any_element();
        };
        base.cursor_pointer()
            .hover(|s| s.bg(theme.background.opacity(0.5)))
            .child(chevron(*open))
            .child(
                Icon::new(IconName::Folder)
                    .xsmall()
                    .text_color(theme.muted_foreground),
            )
            .child(div().flex_1().min_w_0().truncate().child(schema.clone()))
            .child(
                Button::new(("diagram", ix))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Network)
                    .tooltip(t!("diagram.open").to_string())
                    .on_click(cx.listener(move |t, _, _, cx| {
                        cx.stop_propagation();
                        if let Some((_, Row::Schema { conn, schema, .. })) = t.rows.get(ix).cloned()
                        {
                            cx.emit(ObjectsEvent::OpenDiagram {
                                conn_id: conn,
                                schema,
                            });
                        }
                    })),
            )
            .on_click(cx.listener(move |t, _, _, cx| t.toggle(ix, cx)))
            .context_menu({
                let me = cx.entity().downgrade();
                let (c, s) = (conn_of_schema.clone(), schema.clone());
                move |menu: PopupMenu, _, _| {
                    let (me, c, s) = (me.clone(), c.clone(), s.clone());
                    menu.item(crate::shell::menu::item(
                        t!("imp.menu_new").to_string(),
                        Some("table"),
                        move |_, cx| {
                            let _ = me.update(cx, |_, cx| {
                                cx.emit(ObjectsEvent::ImportInto {
                                    conn_id: c.clone(),
                                    schema: s.clone(),
                                    table: None,
                                })
                            });
                        },
                    ))
                }
            })
            .into_any_element()
    }

    /// Table or view row with its hover buttons and context menu.
    fn render_object_row(
        &self,
        ix: usize,
        base: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let chevron = |open: bool| {
            Icon::new(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .xsmall()
            .text_color(theme.muted_foreground)
        };
        let Row::Object {
            conn,
            schema,
            obj,
            open,
        } = &self.rows[ix].1
        else {
            return div().into_any_element();
        };
        base.group(SharedString::from(format!("obj-row-{ix}")))
            .cursor_pointer()
            .hover(|s| s.bg(theme.background.opacity(0.5)))
            .child(
                div()
                    .id(("obj-chevron", ix))
                    .child(chevron(*open))
                    .on_click(cx.listener(move |t, _, _, cx| {
                        cx.stop_propagation();
                        t.toggle(ix, cx)
                    })),
            )
            .child(
                Icon::new(if obj.kind == ObjectKind::View {
                    IconName::Eye
                } else {
                    IconName::LayoutDashboard
                })
                .xsmall()
                .text_color(theme.muted_foreground),
            )
            .child(div().flex_1().min_w_0().truncate().child(obj.name.clone()))
            .child(
                div()
                    .flex_none()
                    .invisible()
                    .group_hover(SharedString::from(format!("obj-row-{ix}")), |s| s.visible())
                    .child(
                        Button::new(("structure", ix))
                            .ghost()
                            .xsmall()
                            .icon(IconName::LayoutDashboard)
                            .tooltip(t!("structure.open").to_string())
                            .on_click(cx.listener(move |t, _, _, cx| {
                                cx.stop_propagation();
                                if let Some((
                                    _,
                                    Row::Object {
                                        conn, schema, obj, ..
                                    },
                                )) = t.rows.get(ix).cloned()
                                {
                                    cx.emit(ObjectsEvent::ShowStructure {
                                        conn_id: conn,
                                        schema,
                                        table: obj.name,
                                    });
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .invisible()
                    .group_hover(SharedString::from(format!("obj-row-{ix}")), |s| s.visible())
                    .child(
                        Button::new(("relations", ix))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Network)
                            .tooltip(t!("diagram.relations").to_string())
                            .on_click(cx.listener(move |t, _, _, cx| {
                                cx.stop_propagation();
                                if let Some((
                                    _,
                                    Row::Object {
                                        conn, schema, obj, ..
                                    },
                                )) = t.rows.get(ix).cloned()
                                {
                                    cx.emit(ObjectsEvent::ShowRelations {
                                        conn_id: conn,
                                        schema,
                                        table: obj.name,
                                    });
                                }
                            })),
                    ),
            )
            .child(Self::ddl_button(
                ("obj-ddl", ix),
                conn.clone(),
                schema.clone(),
                if obj.kind == ObjectKind::View {
                    DbObjectKind::View
                } else {
                    DbObjectKind::Table
                },
                obj.name.clone(),
                cx,
            ))
            .tooltip({
                let tip = t!("objects.open_hint").into_owned();
                move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)
            })
            .on_click(cx.listener(move |t, e: &ClickEvent, _, cx| {
                // Single click selects nothing; the chevron expands. Double-click opens only.
                if e.click_count() >= 2 {
                    t.open_table(ix, cx)
                }
            }))
            .context_menu(Self::table_menu(
                cx.entity().downgrade(),
                conn.clone(),
                schema.clone(),
                obj.name.clone(),
            ))
            .into_any_element()
    }

    /// Right-click menu of a table or view row.
    fn table_menu(
        me: WeakEntity<Self>,
        c: String,
        s: String,
        o: String,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        move |menu: PopupMenu, _, _| {
            let (me1, me2, me3, me4, me5) =
                (me.clone(), me.clone(), me.clone(), me.clone(), me.clone());
            let (c5, s5, o5) = (c.clone(), s.clone(), o.clone());
            let (c4, s4, o4) = (c.clone(), s.clone(), o.clone());
            let (c1, s1, o1) = (c.clone(), s.clone(), o.clone());
            let (c2, s2, o2) = (c.clone(), s.clone(), o.clone());
            let (c3, s3, o3) = (c.clone(), s.clone(), o.clone());
            menu.item(crate::shell::menu::item(
                t!("structure.open").to_string(),
                Some("table"),
                move |_, cx| {
                    let _ = me1.update(cx, |_, cx| {
                        cx.emit(ObjectsEvent::ShowStructure {
                            conn_id: c1.clone(),
                            schema: s1.clone(),
                            table: o1.clone(),
                        })
                    });
                },
            ))
            .item(crate::shell::menu::item(
                t!("diagram.relations").to_string(),
                Some("schema"),
                move |_, cx| {
                    let _ = me2.update(cx, |_, cx| {
                        cx.emit(ObjectsEvent::ShowRelations {
                            conn_id: c2.clone(),
                            schema: s2.clone(),
                            table: o2.clone(),
                        })
                    });
                },
            ))
            .item(crate::shell::menu::item(
                t!("objects.view_ddl").to_string(),
                Some("script"),
                move |_, cx| {
                    let _ = me3.update(cx, |_, cx| {
                        cx.emit(ObjectsEvent::ShowDdl {
                            conn_id: c3.clone(),
                            schema: s3.clone(),
                            kind: DbObjectKind::Table,
                            name: o3.clone(),
                        })
                    });
                },
            ))
            .item(crate::shell::menu::item(
                t!("imp.menu_into").to_string(),
                Some("table"),
                move |_, cx| {
                    let _ = me4.update(cx, |_, cx| {
                        cx.emit(ObjectsEvent::ImportInto {
                            conn_id: c4.clone(),
                            schema: s4.clone(),
                            table: Some(o4.clone()),
                        })
                    });
                },
            ))
            .item(crate::shell::menu::item(
                t!("exp2.export_table").to_string(),
                Some("download"),
                move |_, cx| {
                    let _ = me5.update(cx, |_, cx| {
                        cx.emit(ObjectsEvent::ExportTable {
                            conn_id: c5.clone(),
                            schema: s5.clone(),
                            table: o5.clone(),
                        })
                    });
                },
            ))
        }
    }
}

impl Render for ObjectTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let n = self.rows.len();
        // The widest row (by text length + indent) sets the content width, so long names scroll
        // sideways instead of being cut off.
        let widest = self
            .rows
            .iter()
            .enumerate()
            .max_by_key(|(_, (depth, row))| {
                let chars = match row {
                    Row::Conn { name, .. } => name.chars().count(),
                    Row::Schema { schema, .. } => schema.chars().count(),
                    Row::Object { obj, .. } => obj.name.chars().count(),
                    Row::Entry { entry, .. } => {
                        entry.name.chars().count()
                            + entry.detail.as_ref().map_or(0, |d| d.chars().count())
                    }
                    Row::Column(c) => c.name.chars().count() + c.type_name.chars().count() + 3,
                    _ => 0,
                };
                chars * 7 + depth * 14
            })
            .map(|(i, _)| i);
        v_flex().size_full().child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    uniform_list(
                        "object-tree",
                        n,
                        cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                            range.map(|i| this.render_row(i, cx)).collect::<Vec<_>>()
                        }),
                    )
                    .with_horizontal_sizing_behavior(
                        gpui_kit::ListHorizontalSizingBehavior::Unconstrained,
                    )
                    .with_width_from_item(widest)
                    .track_scroll(&self.scroll)
                    .size_full(),
                )
                .vertical_scrollbar(&self.scroll)
                .horizontal_scrollbar(&self.scroll),
        )
    }
}

impl ObjectTree {
    /// The shell could not open `conn`: show it as the connection's error row.
    pub fn connect_failed(&mut self, conn: &str, msg: String, cx: &mut Context<Self>) {
        self.schemas.insert(conn.to_string(), Load::Failed(msg));
        self.rebuild();
        cx.notify();
    }
}

// ---- category folders (extended catalog API) ------------------------------------

fn folder_key(conn: &str, schema: &str, kind: DbObjectKind) -> String {
    key(&[conn, schema, &format!("#{kind:?}")])
}

fn same_kind(o: &SchemaObject, kind: DbObjectKind) -> bool {
    match kind {
        DbObjectKind::Table => o.kind == ObjectKind::Table,
        DbObjectKind::View => o.kind == ObjectKind::View,
        _ => false,
    }
}

/// Translated folder name of a kind (`DbObjectKind::label` is English only).
pub fn kind_label(kind: DbObjectKind) -> String {
    let k = match kind {
        DbObjectKind::Table => "tables",
        DbObjectKind::View => "views",
        DbObjectKind::MaterializedView => "materialized_views",
        DbObjectKind::Index => "indexes",
        DbObjectKind::Sequence => "sequences",
        DbObjectKind::Procedure => "procedures",
        DbObjectKind::Function => "functions",
        DbObjectKind::Package => "packages",
        DbObjectKind::Trigger => "triggers",
        DbObjectKind::Event => "events",
        DbObjectKind::Synonym => "synonyms",
        DbObjectKind::Type => return t!("routines.kind_types").into_owned(),
        _ => return kind.label().to_string(),
    };
    let key = format!("objects.kinds.{k}");
    t!(&key).into_owned()
}

fn kind_icon(kind: DbObjectKind) -> IconName {
    match kind {
        DbObjectKind::Table => IconName::LayoutDashboard,
        DbObjectKind::View | DbObjectKind::MaterializedView => IconName::Eye,
        DbObjectKind::Index => IconName::Search,
        DbObjectKind::Sequence => IconName::SortAscending,
        DbObjectKind::Procedure
        | DbObjectKind::Function
        | DbObjectKind::Package
        | DbObjectKind::Type => IconName::SquareTerminal,
        DbObjectKind::Trigger | DbObjectKind::Event => IconName::Bell,
        _ => IconName::File,
    }
}

const TREE_CAP: u32 = 1000;
/// Rows a folder can grow to with "load more"; beyond that the filter finds the rest.
const MAX_CAP: u32 = 50_000;
/// Rows a server-side name search returns.
const FILTER_CAP: u32 = 200;

/// Kinds that open in a Source tab (the shell falls back to View DDL
/// when the engine cannot save that kind).
fn is_routine(kind: DbObjectKind) -> bool {
    matches!(
        kind,
        DbObjectKind::Package
            | DbObjectKind::Procedure
            | DbObjectKind::Function
            | DbObjectKind::Trigger
            | DbObjectKind::Type
            | DbObjectKind::Event
    )
}

impl ObjectTree {
    /// Folders of a schema: what the driver supports, else tables and views.
    fn kinds(&self, conn: &str) -> Vec<DbObjectKind> {
        let k = self
            .driver(conn)
            .and_then(|d| d.catalog().map(|c| c.supported_kinds()))
            .unwrap_or_default();
        if k.is_empty() {
            vec![DbObjectKind::Table, DbObjectKind::View]
        } else {
            k
        }
    }

    /// Engines whose catalog lists every kind (tables too) through `list_page`;
    /// SQLite has no such catalog and uses `objects()` for tables and views.
    fn paged(&self, conn: &str) -> bool {
        self.driver(conn)
            .and_then(|d| d.catalog().map(|c| !c.supported_kinds().is_empty()))
            .unwrap_or(false)
    }

    /// Tables / views served by `objects()` (no paging) rather than `list_page`.
    fn is_table_list(&self, conn: &str, kind: DbObjectKind) -> bool {
        matches!(kind, DbObjectKind::Table | DbObjectKind::View) && !self.paged(conn)
    }

    fn load_list(&mut self, conn: &str, schema: &str, kind: DbObjectKind, cx: &mut Context<Self>) {
        self.load_page(conn, schema, kind, None, TREE_CAP, cx);
    }

    /// "Load more": the folder is read again with `TREE_CAP` more rows.
    fn load_more(&mut self, conn: &str, schema: &str, kind: DbObjectKind, cx: &mut Context<Self>) {
        let k = (conn.to_string(), schema.to_string(), kind);
        let cap = self
            .pages
            .get(&k)
            .map_or(TREE_CAP, |p| p.cap)
            .saturating_add(TREE_CAP)
            .min(MAX_CAP);
        self.load_page(conn, schema, kind, None, cap, cx);
    }

    /// Reads a folder (first `cap` rows, optionally only names containing
    /// `filter`) through the catalog's server-side paging.
    fn load_page(
        &mut self,
        conn: &str,
        schema: &str,
        kind: DbObjectKind,
        filter: Option<String>,
        cap: u32,
        cx: &mut Context<Self>,
    ) {
        let Some(d) = self.driver(conn) else { return };
        let Some(fut) = d
            .catalog()
            .map(|c| c.list_page(schema, kind, filter.as_deref(), cap))
        else {
            return;
        };
        let k: LKey = (conn.to_string(), schema.to_string(), kind);
        match &filter {
            Some(q) => {
                self.found.insert(k.clone(), (q.clone(), Load::Loading));
            }
            None => {
                let more = matches!(self.lists.get(&k), Some(Load::Loaded(_)));
                if more {
                    if let Some(p) = self.pages.get_mut(&k) {
                        p.busy = true;
                    }
                } else {
                    self.lists.insert(k.clone(), Load::Loading);
                }
                self.pages
                    .entry(k.clone())
                    .and_modify(|p| p.cap = cap)
                    .or_insert(Page {
                        cap,
                        total: None,
                        busy: more,
                    });
            }
        }
        self._tasks.push(cx.spawn(async move |this, cx| {
            let r = fut.await;
            let _ = this.update(cx, |t, cx| {
                match (filter, r) {
                    (Some(q), r) => {
                        t.found.insert(
                            k,
                            (
                                q,
                                r.map(|p| Load::Loaded(p.entries))
                                    .unwrap_or_else(|e| Load::Failed(e.message)),
                            ),
                        );
                    }
                    (None, Ok(p)) => {
                        t.pages.insert(
                            k.clone(),
                            Page {
                                cap,
                                total: p.total,
                                busy: false,
                            },
                        );
                        t.lists.insert(k, Load::Loaded(p.entries));
                    }
                    (None, Err(e)) => {
                        if let Some(pg) = t.pages.get_mut(&k) {
                            pg.busy = false;
                        }
                        if !matches!(t.lists.get(&k), Some(Load::Loaded(_))) {
                            t.lists.insert(k, Load::Failed(e.message));
                        }
                    }
                }
                t.rebuild();
                cx.notify();
            });
        }));
    }

    /// A folder shows fewer rows than the server has (or may have).
    fn truncated(&self, k: &LKey) -> bool {
        let Some(Load::Loaded(v)) = self.lists.get(k) else {
            return false;
        };
        match self.pages.get(k) {
            Some(Page { total: Some(t), .. }) => *t > v.len() as u64,
            Some(Page { cap, .. }) => v.len() as u64 >= u64::from(*cap),
            None => false,
        }
    }

    /// The tree filter changed: every open, truncated folder asks the server
    /// for the names that contain it (bounded), so rows beyond the cap are found.
    fn search_server(&mut self, cx: &mut Context<Self>) {
        if self.filter.is_empty() {
            return;
        }
        let ks: Vec<LKey> = self
            .lists
            .keys()
            .filter(|k| self.expanded.contains(&folder_key(&k.0, &k.1, k.2)) && self.truncated(k))
            .cloned()
            .collect();
        for k in ks {
            if self.found.get(&k).is_some_and(|(q, _)| *q == self.filter) {
                continue;
            }
            let q = self.filter.clone();
            self.load_page(&k.0, &k.1, k.2, Some(q), FILTER_CAP, cx);
        }
    }

    fn load_conn_folder(&mut self, conn: &str, info: bool, cx: &mut Context<Self>) {
        let Some(d) = self.driver(conn) else { return };
        let Some(cat) = d.catalog() else { return };
        let c = conn.to_string();
        if info {
            let fut = cat.server_info(TREE_CAP);
            self.infos.insert(c.clone(), Load::Loading);
            self._tasks.push(cx.spawn(async move |this, cx| {
                let r = fut.await;
                let _ = this.update(cx, |t, cx| {
                    t.infos.insert(
                        c,
                        r.map(Load::Loaded)
                            .unwrap_or_else(|e| Load::Failed(e.message)),
                    );
                    t.rebuild();
                    cx.notify();
                });
            }));
        } else {
            let fut = cat.principals(TREE_CAP);
            self.principals.insert(c.clone(), Load::Loading);
            self._tasks.push(cx.spawn(async move |this, cx| {
                let r = fut.await;
                let _ = this.update(cx, |t, cx| {
                    t.principals.insert(
                        c,
                        r.map(Load::Loaded)
                            .unwrap_or_else(|e| Load::Failed(e.message)),
                    );
                    t.rebuild();
                    cx.notify();
                });
            }));
        }
    }

    /// Folders and connection folders; returns true when the row was one of them.
    fn toggle_extra(&mut self, ix: usize, cx: &mut Context<Self>) -> bool {
        let Some((_, row)) = self.rows.get(ix).cloned() else {
            return false;
        };
        match row {
            Row::Folder {
                conn, schema, kind, ..
            } => {
                let k = folder_key(&conn, &schema, kind);
                if !self.expanded.remove(&k) {
                    self.expanded.insert(k);
                    if self.is_table_list(&conn, kind) {
                        if !self.objects.contains_key(&(conn.clone(), schema.clone())) {
                            self.load_objects(&conn, &schema, cx);
                        }
                    } else if !self
                        .lists
                        .contains_key(&(conn.clone(), schema.clone(), kind))
                    {
                        self.load_list(&conn, &schema, kind, cx);
                    }
                }
            }
            Row::ConnFolder { conn, info, .. } => {
                let k = key(&[&conn, if info { "#info" } else { "#principals" }]);
                if !self.expanded.remove(&k) {
                    self.expanded.insert(k);
                    let loaded = if info {
                        self.infos.contains_key(&conn)
                    } else {
                        self.principals.contains_key(&conn)
                    };
                    if !loaded {
                        self.load_conn_folder(&conn, info, cx);
                    }
                }
            }
            _ => return false,
        }
        self.rebuild();
        cx.notify();
        true
    }

    fn ddl_button(
        id: (&'static str, usize),
        conn: String,
        schema: String,
        kind: DbObjectKind,
        name: String,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .ghost()
            .xsmall()
            .icon(IconName::FileText)
            .tooltip(t!("objects.view_ddl").to_string())
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                cx.emit(ObjectsEvent::ShowDdl {
                    conn_id: conn.clone(),
                    schema: schema.clone(),
                    kind,
                    name: name.clone(),
                });
            }))
    }

    /// Rows added by the category folders.
    fn render_extra(
        &self,
        ix: usize,
        base: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let chevron = |open: bool| {
            Icon::new(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .xsmall()
            .text_color(theme.muted_foreground)
        };
        Some(match &self.rows[ix].1 {
            Row::Folder {
                kind,
                open,
                count,
                of,
                ..
            } => base
                .cursor_pointer()
                .hover(|s| s.bg(theme.background.opacity(0.5)))
                .child(chevron(*open))
                .child(
                    Icon::new(IconName::Folder)
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
                .child(div().flex_1().min_w_0().truncate().child(kind_label(*kind)))
                .when_some(*count, |r, n| {
                    let text = match of {
                        Some(t) if *t > n as u64 => format!(
                            "{} / {}",
                            locus_core::fmt::count(n as u64),
                            locus_core::fmt::count(*t)
                        ),
                        _ => locus_core::fmt::count(n as u64),
                    };
                    r.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(text),
                    )
                })
                .on_click(cx.listener(move |t, _, _, cx| {
                    t.toggle_extra(ix, cx);
                }))
                .into_any_element(),
            Row::More { .. } => self.render_more_row(ix, base, cx),
            Row::Entry { .. } => self.render_entry_row(ix, base, cx),
            Row::ConnFolder { info, open, .. } => base
                .cursor_pointer()
                .hover(|s| s.bg(theme.background.opacity(0.5)))
                .child(chevron(*open))
                .child(
                    Icon::new(if *info {
                        IconName::Info
                    } else {
                        IconName::User
                    })
                    .xsmall()
                    .text_color(theme.muted_foreground),
                )
                .child(
                    div().flex_1().min_w_0().truncate().child(
                        if *info {
                            t!("objects.server_info")
                        } else {
                            t!("objects.principals")
                        }
                        .to_string(),
                    ),
                )
                .on_click(cx.listener(move |t, _, _, cx| {
                    t.toggle_extra(ix, cx);
                }))
                .into_any_element(),
            Row::Principal(p) => base
                .child(div().w(px(12.)).flex_none())
                .child(
                    Icon::new(IconName::User)
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .max_w(px(160.))
                        .truncate()
                        .child(p.name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(p.kind.clone()),
                )
                .into_any_element(),
            Row::Info(i) => base
                .child(div().w(px(12.)).flex_none())
                .child(
                    div()
                        .flex_shrink_0()
                        .max_w(px(140.))
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{} · {}", i.group, i.name)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .child(i.value.clone()),
                )
                .into_any_element(),
            _ => return None,
        })
    }

    /// "First N of M" row with the load-more button.
    fn render_more_row(
        &self,
        ix: usize,
        base: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let Row::More {
            conn,
            schema,
            kind,
            shown,
            total,
            busy,
        } = &self.rows[ix].1
        else {
            return div().into_any_element();
        };
        let (c, s, k, busy) = (conn.clone(), schema.clone(), *kind, *busy);
        let msg = match total {
            Some(t) => t!(
                "wavea.first_of",
                n = locus_core::fmt::count(*shown as u64),
                total = locus_core::fmt::count(*t)
            )
            .into_owned(),
            None => t!("wavea.first_n", n = locus_core::fmt::count(*shown as u64)).into_owned(),
        };
        base.w_full()
            .overflow_hidden()
            .child(div().w(px(12.)).flex_none())
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(msg),
            )
            .child(
                Button::new(("more", ix))
                    .ghost()
                    .xsmall()
                    .loading(busy)
                    .label(t!("wavea.load_more").to_string())
                    .on_click(cx.listener(move |t, _, _, cx| {
                        cx.stop_propagation();
                        if !busy {
                            t.load_more(&c, &s, k, cx);
                        }
                    })),
            )
            .into_any_element()
    }

    /// Index, routine or trigger row.
    fn render_entry_row(
        &self,
        ix: usize,
        base: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let Row::Entry {
            conn,
            schema,
            kind,
            entry,
        } = &self.rows[ix].1
        else {
            return div().into_any_element();
        };
        let label = match &entry.parent {
            Some(p) => format!("{} ({p})", entry.name),
            None => entry.name.clone(),
        };
        let (c, s, k, n) = (conn.clone(), schema.clone(), *kind, entry.name.clone());
        let routine = is_routine(k);
        let invalid = entry.detail.as_deref() == Some("INVALID");
        let (c2, s2, n2) = (c.clone(), s.clone(), n.clone());
        base.w_full()
            .overflow_hidden()
            .cursor_pointer()
            .hover(|st| st.bg(theme.background.opacity(0.5)))
            .child(div().w(px(12.)).flex_none())
            .child(
                Icon::new(kind_icon(*kind))
                    .xsmall()
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .max_w(px(160.))
                    .truncate()
                    .child(label),
            )
            .when(invalid, |r| {
                r.child(
                    Icon::new(IconName::CircleX)
                        .xsmall()
                        .flex_none()
                        .text_color(theme.danger),
                )
            })
            .when_some(entry.detail.clone(), |r, d| {
                r.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(if invalid {
                            theme.danger
                        } else {
                            theme.muted_foreground
                        })
                        .child(d),
                )
            })
            .when(routine, |r| {
                r.child(
                    Button::new(("entry-src", ix))
                        .ghost()
                        .xsmall()
                        .icon(IconName::SquareTerminal)
                        .tooltip(t!("routines.edit_source").to_string())
                        .flex_none()
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(ObjectsEvent::OpenSource {
                                conn_id: c2.clone(),
                                schema: s2.clone(),
                                kind: k,
                                name: n2.clone(),
                            });
                        })),
                )
            })
            .when(k != DbObjectKind::Index, |r| {
                r.child(
                    Self::ddl_button(("entry-ddl", ix), c.clone(), s.clone(), k, n.clone(), cx)
                        .flex_none(),
                )
            })
            .on_click(cx.listener(move |_, e: &ClickEvent, _, cx| {
                if e.click_count() >= 2 {
                    let (conn_id, schema, name) = (c.clone(), s.clone(), n.clone());
                    cx.emit(if routine {
                        ObjectsEvent::OpenSource {
                            conn_id,
                            schema,
                            kind: k,
                            name,
                        }
                    } else {
                        ObjectsEvent::ShowDdl {
                            conn_id,
                            schema,
                            kind: k,
                            name,
                        }
                    });
                }
            }))
            .into_any_element()
    }
}

// ---- Schema panel support (v3 shell) ------------------------------------------------

/// Schemas of one connection, as the sidebar's expanded connection row shows them.
pub enum SchemaList {
    NotLoaded,
    Loading,
    Loaded,
    Failed,
}

impl ObjectTree {
    pub fn set_filter(&mut self, q: &str, cx: &mut Context<Self>) {
        let q = q.trim().to_lowercase();
        if q != self.filter {
            self.filter = q;
            self.search_server(cx);
            self.rebuild();
            cx.notify();
        }
    }

    pub fn schema_list(&self, conn: &str) -> SchemaList {
        match self.schemas.get(conn) {
            None => SchemaList::NotLoaded,
            Some(Load::Loading) => SchemaList::Loading,
            Some(Load::Loaded(_)) => SchemaList::Loaded,
            Some(Load::Failed(_)) => SchemaList::Failed,
        }
    }
}

fn row_label(r: &Row) -> Option<String> {
    Some(match r {
        Row::Conn { name, .. } => name.clone(),
        Row::Schema { schema, .. } => schema.clone(),
        Row::Object { obj, .. } => obj.name.clone(),
        Row::Entry { entry, .. } => entry.name.clone(),
        Row::Column(c) => c.name.clone(),
        Row::Principal(p) => p.name.clone(),
        Row::Info(i) => format!("{} {}", i.name, i.value),
        _ => return None,
    })
}

/// Keeps rows whose label contains `q`, with every ancestor of a kept row.
fn filter_rows(rows: Vec<(usize, Row)>, q: &str) -> Vec<(usize, Row)> {
    let mut out = Vec::new();
    // Ancestors not emitted yet: (depth, row).
    let mut stack: Vec<(usize, Row, bool)> = Vec::new();
    for (d, r) in rows {
        while stack.last().is_some_and(|(sd, _, _)| *sd >= d) {
            stack.pop();
        }
        let hit = row_label(&r).is_some_and(|l| l.to_lowercase().contains(q));
        if hit {
            for (sd, sr, done) in stack.iter_mut() {
                if !*done {
                    out.push((*sd, sr.clone()));
                    *done = true;
                }
            }
            out.push((d, r.clone()));
        }
        stack.push((d, r, hit));
    }
    if out.is_empty() {
        out.push((0, Row::Note(t!("shell.no_match").into_owned())));
    }
    out
}
