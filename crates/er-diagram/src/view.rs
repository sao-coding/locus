use crate::layout::{auto_layout, level_layout};
use crate::model::{
    CARD_PAD_BOTTOM, Density, DiagramLayout, DiagramModel, End, HEADER_H, JoinPath, ROW_H,
    Relation, Scope, TableDetail, TableNode, card_size, cardinality, color_groups,
    fk_without_index, join_path_text, join_paths, join_sql, scoped_tables, to_mermaid,
};
use crate::route::{self, EdgePath, EdgeStyle, Rect};
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::time::Instant;

mod paint;

const BASE_FONT: f32 = 12.0;
/// Range of the user zoom (buttons, Ctrl+wheel): 40 % to 160 %.
pub const MIN_ZOOM: f32 = 0.4;
pub const MAX_ZOOM: f32 = 1.6;
/// "Fit" may go lower so that a big schema can still be overviewed.
const FIT_MIN_ZOOM: f32 = 0.08;
/// Zoom floor of the fit that runs when a diagram opens.
const OPEN_MIN_ZOOM: f32 = 0.75;
const ZOOM_STEP: f32 = 1.1;
const ASYNC_LAYOUT_FROM: usize = 80;
const DRAG_THRESHOLD: f32 = 3.0;
const DOT_GAP: f32 = 20.0;
/// Hit area of a line: this many screen pixels beyond its stroke, at any zoom.
const HIT_TOL: f32 = 4.0;
/// Camera flight to a search hit, seconds.
const FLY_S: f32 = 0.36;
/// The border pulse on a search hit, seconds.
const PULSE_S: f32 = 0.9;
/// Width of the detail panel (screen pixels).
const PANEL_W: f32 = 300.0;
/// Width of the chevron area at the right of a card header (world units).
const CHEVRON_W: f32 = 24.0;

/// Texts the host supplies (the crate itself shows no hard-coded text).
#[derive(Clone, Debug, Default)]
pub struct Labels {
    /// Shown in the middle when the model has no tables.
    pub empty: SharedString,
    /// Toolbar buttons; each text is also the button's tooltip.
    pub fit: SharedString,
    pub reset_zoom: SharedString,
    pub relayout: SharedString,
    /// Placeholder of the Ctrl+F search field.
    pub search: SharedString,
    /// Tooltip line of a dashed (inferred) relation, e.g. "inferred, no FK".
    pub inferred: SharedString,
    /// Tooltip of the warning on a foreign-key column without an index.
    pub fk_no_index: SharedString,
    /// Detail panel: section titles and buttons.
    pub columns: SharedString,
    pub indexes: SharedString,
    pub rows: SharedString,
    pub open_data: SharedString,
    pub structure: SharedString,
    pub select_star: SharedString,
    pub copy_name: SharedString,
    /// Title of the chooser between JOIN paths of the same length.
    pub choose_path: SharedString,
}

/// What [`ErDiagram::join_path`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinPathResult {
    /// Fewer than two tables are selected.
    NeedTables,
    /// No chain of foreign keys connects them.
    NoPath,
    /// One shortest path: highlighted, `InsertSql` sent.
    Inserted,
    /// Several of the same length: the chooser is open.
    Choose,
}

/// Current selection; relation index refers to `DiagramModel::relations`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    pub tables: Vec<String>,
    pub relation: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ErDiagramEvent {
    /// Double click on a table (its key, `schema.name`).
    OpenTable(String),
    SelectionChanged(Selection),
    /// A table moved, or the camera changed; save `ErDiagram::layout()`.
    LayoutChanged,
    /// The zoom changed (also after "fit"); the value is the new factor, 1.0 = 100 %.
    ZoomChanged(f32),
    /// A relation line was clicked. Feed it to [`crate::join_clause`] to get a JOIN.
    /// Child = `from_table` / `from_columns`, parent = `to_table` / `to_columns`.
    RelationClicked(Relation),
    /// Right click on a table; `position` is in window coordinates. The host
    /// shows its menu and may call `hide_table` / `set_scope`.
    TableContextMenu {
        table: String,
        position: Point<Pixels>,
    },
    /// SQL for the host to put into a query editor (a JOIN path).
    InsertSql(String),
    /// Detail panel "SELECT *": the host writes it for its dialect.
    InsertSelect(String),
    /// Detail panel "structure" of a table (its key).
    OpenStructure(String),
    /// Density, a card's collapse chevron, the minimap or the edge style
    /// changed; save them (`density`, `collapsed_cards`, `minimap`, `edge_style`).
    ViewOptionsChanged,
}

struct TextCache {
    px: u32,
    sig: (Hsla, Hsla),
    header: ShapedLine,
    count: Option<ShapedLine>,
    names: Vec<ShapedLine>,
    types: Vec<ShapedLine>,
}

/// Small texts shared by all cards (tags).
struct SharedText {
    px: u32,
    sig: (Hsla, Hsla, Hsla, SharedString),
    pk: ShapedLine,
    fk: ShapedLine,
    centre: ShapedLine,
}

struct Node {
    key: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    placed: bool,
    /// Column takes part in a foreign key (as referencing column).
    fk: Vec<bool>,
    /// Row of each column on the card; `None` = not shown at this density.
    slot: Vec<Option<u16>>,
    /// Shown columns, top to bottom.
    shown: Vec<usize>,
    text: Option<TextCache>,
}

/// What an edge's geometry was computed from: both card boxes and row centres.
type GeomKey = [f32; 11];

struct Rel {
    from: usize,
    to: usize,
    /// Key columns (indices) on the referencing / referenced card.
    from_cols: Vec<usize>,
    to_cols: Vec<usize>,
    /// Index into `DiagramModel::relations`.
    model_ix: usize,
    /// (referencing end, referenced end).
    ends: (End, End),
    /// Where the cards and rows were when the channel offsets were last assigned.
    pos: Option<[f32; 10]>,
    /// Own vertical in the gap it shares with other lines into the same card.
    offset: f32,
    geom: Option<(GeomKey, EdgePath)>,
}

/// Screen box of the minimap and its world-to-screen mapping (local coordinates).
#[derive(Clone, Copy, Debug, PartialEq)]
struct MiniMap {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    ox: f32,
    oy: f32,
    wx: f32,
    wy: f32,
    s: f32,
}

impl MiniMap {
    fn contains(&self, p: (f32, f32)) -> bool {
        p.0 >= self.x && p.0 <= self.x + self.w && p.1 >= self.y && p.1 <= self.y + self.h
    }
    fn map_to_world(self, p: (f32, f32)) -> (f32, f32) {
        (
            self.wx + (p.0 - self.ox) / self.s,
            self.wy + (p.1 - self.oy) / self.s,
        )
    }
}

#[derive(Clone, Copy)]
struct Anim {
    start: Instant,
    from_c: (f32, f32),
    to_c: (f32, f32),
    from_z: f32,
    to_z: f32,
}

struct Search {
    input: Entity<InputState>,
    query: String,
    matches: Vec<usize>,
    /// The hit Enter went to last.
    cur: Option<usize>,
    _sub: Subscription,
}

#[derive(Default)]
enum Drag {
    #[default]
    None,
    Pan {
        start: (f32, f32),
        pan0: (f32, f32),
        moved: bool,
    },
    Move {
        start: (f32, f32),
        items: Vec<(usize, f32, f32)>,
        moved: bool,
        clicked: usize,
        shift: bool,
    },
    Rubber {
        start: (f32, f32),
        cur: (f32, f32),
        base: BTreeSet<usize>,
    },
    /// Panning by the minimap; its mapping stays fixed while the button is down.
    Minimap(MiniMap),
}

pub struct ErDiagram {
    model: DiagramModel,
    labels: Labels,
    nodes: Vec<Node>,
    rels: Vec<Rel>,
    index: HashMap<String, usize>,
    order: Vec<usize>,
    zoom: f32,
    pan: (f32, f32),
    pending_fit: bool,
    /// The next fit comes from the "fit" button: may go below the open-time floor.
    fit_exact: bool,
    /// Draw the floating Fit / 100 % / Arrange chips (hosts with their own toolbar turn this off).
    floating_toolbar: bool,
    layouting: bool,
    layout_gen: u64,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    focus: FocusHandle,
    sel: BTreeSet<usize>,
    sel_rel: Option<usize>,
    hover_rel: Option<usize>,
    hover_table: Option<usize>,
    /// Pointer position (local) while hovering, for the line tooltip.
    hover_pos: (f32, f32),
    drag: Drag,
    // scope / visibility
    scope: Scope,
    hidden: BTreeSet<usize>,
    vis: Vec<bool>,
    rel_vis: Vec<bool>,
    /// Hop distance per table in a centred scope.
    levels: Vec<Option<u32>>,
    centre: Option<usize>,
    /// Positions of the whole schema while a narrower scope is shown.
    stash: Option<Vec<(f32, f32, bool)>>,
    counts: HashMap<String, u64>,
    centre_label: SharedString,
    shared: Option<SharedText>,
    // view options
    density: Density,
    /// Cards whose chevron was clicked: header only (or, at `Density::Header`, all columns).
    flipped: BTreeSet<usize>,
    edge_style: EdgeStyle,
    minimap: bool,
    mm: Cell<Option<MiniMap>>,
    search: Option<Search>,
    anim: Option<Anim>,
    pulse: Option<(usize, Instant)>,
    /// Highlight of this frame: tables / relations in focus (hover, selection, search).
    focus_t: Vec<bool>,
    focus_r: Vec<bool>,
    focus_active: bool,
    /// Reused buffers of `refresh_geoms`.
    chan: Vec<(usize, route::Side, f32)>,
    chan_ix: Vec<usize>,
    chan_scratch: Vec<usize>,
    chan_off: Vec<f32>,
    // insights
    /// Draw the dashed, inferred relations.
    show_inferred: bool,
    /// Tint card headers by schema / name prefix.
    color_groups: bool,
    /// Group names and each table's group.
    groups: (Vec<String>, Vec<Option<usize>>),
    /// Per table, per column: a foreign key without an index.
    fk_warn: Vec<Vec<bool>>,
    unindexed: usize,
    /// The row under the pointer when it has a warning: (table, column).
    hover_warn: Option<(usize, usize)>,
    /// The detail panel was closed for the current selection.
    panel_closed: bool,
    /// Highlighted JOIN path: tables and relations (indexes into `rels`).
    path_hl: Option<(Vec<usize>, Vec<usize>)>,
    /// JOIN paths to choose from: (text, SQL, tables, relations).
    chooser: Option<Vec<(String, String, Vec<usize>, Vec<usize>)>>,
}

impl EventEmitter<ErDiagramEvent> for ErDiagram {}

/// 950 -> "950", 12_300 -> "12.3k", 1_200_000 -> "1.2M".
fn fmt_count(n: u64) -> String {
    let f = |v: f64, s: &str| {
        if v >= 100.0 {
            format!("{:.0}{s}", v)
        } else {
            format!("{:.1}{s}", v).replace(".0", "")
        }
    };
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => f(n as f64 / 1e3, "k"),
        1_000_000..=999_999_999 => f(n as f64 / 1e6, "M"),
        _ => f(n as f64 / 1e9, "B"),
    }
}

fn compute_layout(
    sizes: &[(f32, f32)],
    edges: &[(usize, usize)],
    levels: &Option<Vec<u32>>,
) -> Vec<(f32, f32)> {
    match levels {
        Some(l) => level_layout(sizes, l),
        None => auto_layout(sizes, edges),
    }
}

/// `orders.customer_id → customers.id`; composite keys as `t.(a, b)`.
pub(crate) fn relation_text(r: &Relation) -> String {
    let bare = |k: &str| k.split_once('.').map_or(k, |(_, n)| n).to_string();
    let side = |t: &str, cols: &[String]| match cols {
        [one] => format!("{}.{one}", bare(t)),
        many => format!("{}.({})", bare(t), many.join(", ")),
    };
    format!(
        "{} → {}",
        side(&r.from_table, &r.from_columns),
        side(&r.to_table, &r.to_columns)
    )
}

/// Search hits among `names` (index, name) for `q`: names containing `q`
/// (case-insensitive), prefix hits first, then shorter names, then by index.
pub(crate) fn search_hits<'a>(
    names: impl Iterator<Item = (usize, &'a str)>,
    q: &str,
) -> Vec<usize> {
    let q = q.trim().to_lowercase();
    if q.is_empty() {
        return vec![];
    }
    let mut hits: Vec<(bool, usize, usize)> = names
        .filter_map(|(i, n)| {
            let l = n.to_lowercase();
            l.find(&q).map(|at| (at != 0, n.len(), i))
        })
        .collect();
    hits.sort_unstable();
    hits.into_iter().map(|h| h.2).collect()
}

fn ease(t: f32) -> f32 {
    // cubic in-out
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

impl ErDiagram {
    pub fn new(
        model: DiagramModel,
        labels: Labels,
        layout: Option<DiagramLayout>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut s = Self {
            model: DiagramModel::default(),
            labels,
            nodes: vec![],
            rels: vec![],
            index: HashMap::new(),
            order: vec![],
            zoom: 1.0,
            pan: (0.0, 0.0),
            pending_fit: true,
            fit_exact: false,
            floating_toolbar: true,
            layouting: false,
            layout_gen: 0,
            bounds: Rc::new(Cell::new(Bounds::default())),
            focus: cx.focus_handle(),
            sel: BTreeSet::new(),
            sel_rel: None,
            hover_rel: None,
            hover_table: None,
            hover_pos: (0.0, 0.0),
            drag: Drag::None,
            scope: Scope::All,
            hidden: BTreeSet::new(),
            vis: vec![],
            rel_vis: vec![],
            levels: vec![],
            centre: None,
            stash: None,
            counts: HashMap::new(),
            centre_label: "centre".into(),
            shared: None,
            density: Density::Full,
            flipped: BTreeSet::new(),
            edge_style: EdgeStyle::Bezier,
            minimap: true,
            mm: Cell::new(None),
            search: None,
            anim: None,
            pulse: None,
            focus_t: vec![],
            focus_r: vec![],
            focus_active: false,
            chan: vec![],
            chan_ix: vec![],
            chan_scratch: vec![],
            chan_off: vec![],
            show_inferred: true,
            color_groups: false,
            groups: (vec![], vec![]),
            fk_warn: vec![],
            unindexed: 0,
            hover_warn: None,
            panel_closed: false,
            path_hl: None,
            chooser: None,
        };
        s.set_model(model, layout, cx);
        s
    }

    pub fn set_labels(&mut self, labels: Labels, cx: &mut Context<Self>) {
        self.labels = labels;
        cx.notify();
    }

    /// Replace the data. Tables found in `layout` keep their saved place; the
    /// others are placed automatically. The current scope is kept, hidden
    /// tables are shown again.
    pub fn set_model(
        &mut self,
        model: DiagramModel,
        layout: Option<DiagramLayout>,
        cx: &mut Context<Self>,
    ) {
        self.nodes = model
            .tables
            .iter()
            .map(|t| {
                let (w, h) = card_size(t);
                let mut fk = vec![false; t.columns.len()];
                for r in model
                    .relations
                    .iter()
                    .filter(|r| r.from_table == t.key() && !r.inferred)
                {
                    for c in &r.from_columns {
                        if let Some(i) = t.columns.iter().position(|x| &x.name == c) {
                            fk[i] = true;
                        }
                    }
                }
                Node {
                    key: t.key(),
                    x: 0.0,
                    y: 0.0,
                    w,
                    h,
                    placed: false,
                    fk,
                    slot: vec![],
                    shown: vec![],
                    text: None,
                }
            })
            .collect();
        self.index = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.key.clone(), i))
            .collect();
        self.order = (0..self.nodes.len()).collect();
        self.rels = model
            .relations
            .iter()
            .enumerate()
            .filter_map(|(mi, r)| {
                let (f, t) = (
                    *self.index.get(&r.from_table)?,
                    *self.index.get(&r.to_table)?,
                );
                let cols = |ti: usize, names: &[String]| -> Vec<usize> {
                    names
                        .iter()
                        .filter_map(|c| model.tables[ti].columns.iter().position(|x| &x.name == c))
                        .collect()
                };
                Some(Rel {
                    from: f,
                    to: t,
                    from_cols: cols(f, &r.from_columns),
                    to_cols: cols(t, &r.to_columns),
                    model_ix: mi,
                    ends: cardinality(r, &model.tables[f]),
                    pos: None,
                    offset: 0.0,
                    geom: None,
                })
            })
            .collect();
        self.model = model;
        self.flipped.clear();
        for i in 0..self.nodes.len() {
            self.apply_rows(i);
        }
        self.recompute_checks();
        self.path_hl = None;
        self.chooser = None;
        self.hover_warn = None;
        self.sel.clear();
        self.sel_rel = None;
        self.hover_rel = None;
        self.hover_table = None;
        self.drag = Drag::None;
        self.hidden.clear();
        self.stash = None;
        self.pending_fit = true;
        self.pulse = None;
        if let Some(l) = &layout {
            self.apply_saved(l);
        }
        self.recompute_vis();
        if !matches!(self.scope, Scope::All) {
            self.stash_whole();
            self.unplace_visible();
        }
        self.run_layout(true, cx);
        if let Some(s) = &self.search {
            let q = s.query.clone();
            self.set_query(q, cx);
        }
        cx.notify();
    }

    /// Indexes, comments and defaults per table key, from the host's
    /// structure calls. Updates the 1:1 marks and the "keys only" cards.
    pub fn set_table_details(
        &mut self,
        details: HashMap<String, TableDetail>,
        cx: &mut Context<Self>,
    ) {
        for (k, d) in &details {
            if let Some(&i) = self.index.get(k) {
                self.model.tables[i].apply_detail(d);
                self.apply_rows(i);
            }
        }
        for r in &mut self.rels {
            r.ends = cardinality(
                &self.model.relations[r.model_ix],
                &self.model.tables[r.from],
            );
        }
        self.recompute_checks();
        cx.notify();
    }

    /// The model as shown (with the details the host added).
    pub fn model(&self) -> &DiagramModel {
        &self.model
    }

    /// Which rows card `i` shows, and its height.
    fn apply_rows(&mut self, i: usize) {
        let t = &self.model.tables[i];
        let flip = self.flipped.contains(&i);
        let d = match (self.density, flip) {
            (d, false) => d,
            (Density::Header, true) => Density::Full,
            (_, true) => Density::Header,
        };
        let n = &mut self.nodes[i];
        n.slot = vec![None; t.columns.len()];
        n.shown.clear();
        for c in 0..t.columns.len() {
            if d.shows(t, c, n.fk[c]) {
                n.slot[c] = Some(n.shown.len() as u16);
                n.shown.push(c);
            }
        }
        n.h = if d == Density::Full {
            card_size(t).1
        } else if n.shown.is_empty() {
            HEADER_H
        } else {
            HEADER_H + n.shown.len() as f32 * ROW_H + CARD_PAD_BOTTOM
        };
    }

    fn apply_saved(&mut self, l: &DiagramLayout) {
        for n in &mut self.nodes {
            if let Some(p) = l.tables.get(&n.key) {
                n.x = p[0];
                n.y = p[1];
                n.placed = true;
            }
        }
        if let Some(z) = l.zoom
            && let Some(p) = l.pan
        {
            self.zoom = z.clamp(FIT_MIN_ZOOM, MAX_ZOOM);
            self.pan = (p[0], p[1]);
            self.pending_fit = false;
        }
    }

    /// Positions and camera, for the host to save. While a narrower scope is
    /// shown the positions of the whole schema are returned.
    pub fn layout(&self) -> DiagramLayout {
        let tables = match &self.stash {
            Some(s) => self
                .nodes
                .iter()
                .zip(s)
                .filter(|(_, p)| p.2)
                .map(|(n, p)| (n.key.clone(), [p.0, p.1]))
                .collect(),
            None => self
                .nodes
                .iter()
                .map(|n| (n.key.clone(), [n.x, n.y]))
                .collect(),
        };
        DiagramLayout {
            tables,
            zoom: (!self.pending_fit).then_some(self.zoom),
            pan: (!self.pending_fit).then_some([self.pan.0, self.pan.1]),
        }
    }

    /// Positions of the tables on the canvas as they are now, plus the camera
    /// (what [`Self::layout`] returns for the whole schema). Save it per tab
    /// for a narrower scope; `set_layout` puts the tables back.
    pub fn scope_layout(&self) -> DiagramLayout {
        DiagramLayout {
            tables: self
                .nodes
                .iter()
                .zip(&self.vis)
                .filter(|(_, v)| **v)
                .map(|(n, _)| (n.key.clone(), [n.x, n.y]))
                .collect(),
            zoom: (!self.pending_fit).then_some(self.zoom),
            pan: (!self.pending_fit).then_some([self.pan.0, self.pan.1]),
        }
    }

    pub fn set_layout(&mut self, l: &DiagramLayout, cx: &mut Context<Self>) {
        self.apply_saved(l);
        cx.notify();
    }

    pub fn selection(&self) -> Selection {
        Selection {
            tables: self
                .sel
                .iter()
                .map(|&i| self.nodes[i].key.clone())
                .collect(),
            relation: self.sel_rel.map(|r| self.rels[r].model_ix),
        }
    }

    /// Select these tables (keys; unknown or hidden ones are skipped).
    pub fn set_selection(&mut self, keys: &[String], cx: &mut Context<Self>) {
        self.sel = keys
            .iter()
            .filter_map(|k| self.index.get(k).copied())
            .filter(|&i| self.vis[i])
            .collect();
        self.sel_rel = None;
        self.emit_selection(cx);
        cx.notify();
    }

    /// Number of tables in the model (not only the visible ones).
    pub fn table_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    // ---- view options -------------------------------------------------------

    pub fn density(&self) -> Density {
        self.density
    }

    /// Full columns, keys only or header only, for every card (the per-card
    /// chevrons are kept and still flip their card).
    pub fn set_density(&mut self, d: Density, cx: &mut Context<Self>) {
        if d == self.density {
            return;
        }
        self.density = d;
        for i in 0..self.nodes.len() {
            self.apply_rows(i);
        }
        cx.notify();
        cx.emit(ErDiagramEvent::ViewOptionsChanged);
    }

    /// Keys of the cards whose chevron was clicked.
    pub fn collapsed_cards(&self) -> Vec<String> {
        self.flipped
            .iter()
            .map(|&i| self.nodes[i].key.clone())
            .collect()
    }

    pub fn set_collapsed_cards(&mut self, keys: &[String], cx: &mut Context<Self>) {
        let old = std::mem::take(&mut self.flipped);
        self.flipped = keys
            .iter()
            .filter_map(|k| self.index.get(k).copied())
            .collect();
        for i in old
            .union(&self.flipped.clone())
            .copied()
            .collect::<Vec<_>>()
        {
            self.apply_rows(i);
        }
        cx.notify();
    }

    /// Flip one card between its density and header only (the chevron).
    pub fn toggle_card(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(&i) = self.index.get(key) {
            self.toggle_card_ix(i, cx);
        }
    }

    fn toggle_card_ix(&mut self, i: usize, cx: &mut Context<Self>) {
        if !self.flipped.remove(&i) {
            self.flipped.insert(i);
        }
        self.apply_rows(i);
        cx.notify();
        cx.emit(ErDiagramEvent::ViewOptionsChanged);
    }

    pub fn edge_style(&self) -> EdgeStyle {
        self.edge_style
    }

    pub fn set_edge_style(&mut self, s: EdgeStyle, cx: &mut Context<Self>) {
        if s != self.edge_style {
            self.edge_style = s;
            for r in &mut self.rels {
                r.geom = None;
                r.pos = None;
            }
            cx.notify();
            cx.emit(ErDiagramEvent::ViewOptionsChanged);
        }
    }

    pub fn minimap(&self) -> bool {
        self.minimap
    }

    /// Show or hide the minimap (bottom right; click or drag on it to pan).
    pub fn set_minimap(&mut self, on: bool, cx: &mut Context<Self>) {
        if on != self.minimap {
            self.minimap = on;
            cx.notify();
            cx.emit(ErDiagramEvent::ViewOptionsChanged);
        }
    }

    // ---- insights -----------------------------------------------------------

    /// Foreign keys without an index (`fk_without_index`) and colour groups.
    fn recompute_checks(&mut self) {
        self.fk_warn = self
            .model
            .tables
            .iter()
            .map(|t| vec![false; t.columns.len()])
            .collect();
        let bad = fk_without_index(&self.model);
        self.unindexed = bad.len();
        for ri in bad {
            let r = &self.model.relations[ri];
            if let Some(&ti) = self.index.get(&r.from_table) {
                for c in &r.from_columns {
                    if let Some(ci) = self.model.tables[ti]
                        .columns
                        .iter()
                        .position(|x| &x.name == c)
                    {
                        self.fk_warn[ti][ci] = true;
                    }
                }
            }
        }
        let refs: Vec<&TableNode> = self.model.tables.iter().collect();
        self.groups = color_groups(&refs);
    }

    /// Declared foreign keys whose columns start no index (indexes known).
    pub fn unindexed_fk_count(&self) -> usize {
        self.unindexed
    }

    pub fn show_inferred(&self) -> bool {
        self.show_inferred
    }

    /// Show or hide the dashed relations guessed from names (default shown).
    pub fn set_show_inferred(&mut self, on: bool, cx: &mut Context<Self>) {
        if on != self.show_inferred {
            self.show_inferred = on;
            self.recompute_vis();
            cx.notify();
            cx.emit(ErDiagramEvent::ViewOptionsChanged);
        }
    }

    pub fn color_groups(&self) -> bool {
        self.color_groups
    }

    /// Tint card headers by schema (several shown) or name prefix, with a legend.
    pub fn set_color_groups(&mut self, on: bool, cx: &mut Context<Self>) {
        if on != self.color_groups {
            self.color_groups = on;
            cx.notify();
            cx.emit(ErDiagramEvent::ViewOptionsChanged);
        }
    }

    /// Names of the colour groups (empty when nothing groups).
    pub fn color_group_names(&self) -> &[String] {
        &self.groups.0
    }

    /// Shortest JOIN path through declared foreign keys between the selected
    /// tables (intermediate tables allowed). One path: highlighted and sent
    /// as `InsertSql`; several of the same length: a chooser opens.
    pub fn join_path(
        &mut self,
        quote: impl Fn(&str) -> String,
        cx: &mut Context<Self>,
    ) -> JoinPathResult {
        let keys: Vec<String> = self
            .sel
            .iter()
            .map(|&i| self.nodes[i].key.clone())
            .collect();
        if keys.len() < 2 {
            return JoinPathResult::NeedTables;
        }
        let paths = join_paths(&self.model, &keys, 8);
        let to_view = |p: &JoinPath| -> (Vec<usize>, Vec<usize>) {
            let t = p
                .tables
                .iter()
                .filter_map(|k| self.index.get(k).copied())
                .collect();
            let r = p
                .relations
                .iter()
                .filter_map(|&mi| self.rels.iter().position(|r| r.model_ix == mi))
                .collect();
            (t, r)
        };
        let result = match paths.len() {
            0 => JoinPathResult::NoPath,
            1 => {
                let (t, r) = to_view(&paths[0]);
                self.path_hl = Some((t, r));
                self.chooser = None;
                cx.emit(ErDiagramEvent::InsertSql(join_sql(
                    &self.model,
                    &paths[0],
                    &quote,
                )));
                JoinPathResult::Inserted
            }
            _ => {
                self.chooser = Some(
                    paths
                        .iter()
                        .map(|p| {
                            let (t, r) = to_view(p);
                            (join_path_text(p), join_sql(&self.model, p, &quote), t, r)
                        })
                        .collect(),
                );
                JoinPathResult::Choose
            }
        };
        cx.notify();
        result
    }

    fn pick_path(&mut self, k: usize, cx: &mut Context<Self>) {
        let Some(list) = self.chooser.take() else {
            return;
        };
        if let Some((_, sql, t, r)) = list.into_iter().nth(k) {
            self.path_hl = Some((t, r));
            cx.emit(ErDiagramEvent::InsertSql(sql));
        }
        cx.notify();
    }

    /// The diagram as SVG (vector paths and text) in the current theme: the
    /// selected tables when `selection` and something is selected, else all
    /// tables on the canvas; lines between the exported tables.
    pub fn export_svg(&mut self, selection: bool, cx: &mut Context<Self>) -> String {
        use crate::export::{SvgCard, SvgColors, SvgEdge, SvgRow, SvgScene};
        self.refresh_geoms();
        let only_sel = selection && !self.sel.is_empty();
        let take: Vec<bool> = (0..self.nodes.len())
            .map(|i| self.vis[i] && (!only_sel || self.sel.contains(&i)))
            .collect();
        let t = cx.theme();
        let hex = |c: Hsla| {
            let r = Rgba::from(c);
            let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            if r.a >= 0.999 {
                format!("#{:02x}{:02x}{:02x}", b(r.r), b(r.g), b(r.b))
            } else {
                format!("#{:02x}{:02x}{:02x}{:02x}", b(r.r), b(r.g), b(r.b), b(r.a))
            }
        };
        let tints = [t.blue, t.green, t.yellow, t.magenta, t.cyan, t.red];
        let colors = SvgColors {
            bg: hex(t.background),
            card: hex(t.popover),
            header: hex(t.muted),
            border: hex(t.border),
            fg: hex(t.foreground),
            muted: hex(t.muted_foreground),
            line: hex(t.muted_foreground),
            key: hex(t.warning),
            info: hex(t.info),
            warn: hex(t.warning),
        };
        let font = crate::export::css_font(&t.font_family, "sans-serif");
        let mono = crate::export::css_font(&t.mono_font_family, "monospace");
        let mut cards = vec![];
        for (i, &keep) in take.iter().enumerate() {
            if !keep {
                continue;
            }
            let n = &self.nodes[i];
            let tm = &self.model.tables[i];
            let tint = self
                .color_groups
                .then(|| self.groups.1.get(i).copied().flatten())
                .flatten()
                .map(|g| hex(tints[g % tints.len()].opacity(0.22)));
            cards.push(SvgCard {
                x: n.x,
                y: n.y,
                w: n.w,
                h: n.h,
                title: tm.name.clone(),
                count: self.counts.get(&n.key).map(|c| fmt_count(*c)),
                rows: n
                    .shown
                    .iter()
                    .map(|&c| {
                        let col = &tm.columns[c];
                        SvgRow {
                            name: col.name.clone(),
                            type_name: if col.nullable {
                                format!("{}?", col.type_name)
                            } else {
                                col.type_name.clone()
                            },
                            tag: if col.primary_key {
                                Some("PK")
                            } else if n.fk[c] {
                                Some("FK")
                            } else {
                                None
                            },
                            warn: self.fk_warn[i][c],
                        }
                    })
                    .collect(),
                tint,
            });
        }
        let edges = self
            .rels
            .iter()
            .enumerate()
            .filter(|(ri, r)| self.rel_vis[*ri] && take[r.from] && take[r.to])
            .filter_map(|(_, r)| {
                let (_, e) = r.geom.as_ref()?;
                Some(SvgEdge {
                    path: e.clone(),
                    ends: r.ends,
                    dashed: self.model.relations[r.model_ix].inferred,
                })
            })
            .collect();
        crate::export::to_svg(&SvgScene {
            cards,
            edges,
            colors,
            font,
            mono,
        })
    }

    /// The single selected table, when the detail panel shows it.
    fn panel_table(&self) -> Option<usize> {
        if self.panel_closed || self.sel.len() != 1 {
            return None;
        }
        self.sel.iter().next().copied()
    }

    fn close_panel(&mut self, cx: &mut Context<Self>) {
        self.panel_closed = true;
        cx.notify();
    }

    // ---- scope ------------------------------------------------------------

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Show the whole schema, a centre table with its neighbours, or a list of
    /// tables. Tables are re-arranged (columns by hop distance for a centre);
    /// going back to `Scope::All` restores the earlier positions. Hidden tables
    /// stay hidden.
    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if scope == self.scope {
            return;
        }
        let (was_all, to_all) = (
            matches!(self.scope, Scope::All),
            matches!(scope, Scope::All),
        );
        if was_all && !to_all {
            self.stash_whole();
        }
        self.scope = scope;
        self.recompute_vis();
        self.layout_gen += 1; // drop a pending background layout
        self.layouting = false;
        if to_all {
            if let Some(s) = self.stash.take() {
                for (n, p) in self.nodes.iter_mut().zip(s) {
                    (n.x, n.y, n.placed) = p;
                }
            }
        } else {
            self.unplace_visible();
        }
        self.pending_fit = true;
        self.run_layout(true, cx);
        cx.notify();
        cx.emit(ErDiagramEvent::LayoutChanged);
    }

    /// Keys of the tables on the canvas (scope minus hidden ones).
    pub fn visible_tables(&self) -> Vec<String> {
        (0..self.nodes.len())
            .filter(|&i| self.vis[i])
            .map(|i| self.nodes[i].key.clone())
            .collect()
    }

    pub fn visible_count(&self) -> usize {
        self.vis.iter().filter(|v| **v).count()
    }

    /// Declared foreign keys between visible tables (inferred ones not counted).
    pub fn visible_relation_count(&self) -> usize {
        self.rels
            .iter()
            .zip(&self.rel_vis)
            .filter(|(r, v)| **v && !self.model.relations[r.model_ix].inferred)
            .count()
    }

    /// Remove a table from the canvas (the model is untouched).
    pub fn hide_table(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(&i) = self.index.get(key) else {
            return;
        };
        if !self.hidden.insert(i) {
            return;
        }
        self.recompute_vis();
        if self.sel.remove(&i) {
            self.emit_selection(cx);
        }
        cx.notify();
    }

    pub fn show_hidden_tables(&mut self, cx: &mut Context<Self>) {
        if self.hidden.is_empty() {
            return;
        }
        self.hidden.clear();
        self.recompute_vis();
        self.run_layout(true, cx);
        cx.notify();
    }

    pub fn hidden_tables(&self) -> Vec<String> {
        self.hidden
            .iter()
            .map(|&i| self.nodes[i].key.clone())
            .collect()
    }

    /// Mermaid `erDiagram` text for the visible tables.
    pub fn mermaid(&self) -> String {
        to_mermaid(&self.model, &self.visible_tables())
    }

    /// Row counts by table key; shown in the card headers.
    pub fn set_row_counts(&mut self, counts: HashMap<String, u64>, cx: &mut Context<Self>) {
        self.counts = counts;
        for n in &mut self.nodes {
            n.text = None;
        }
        cx.notify();
    }

    /// Row count of a table, when the host supplied one.
    pub fn row_count(&self, key: &str) -> Option<u64> {
        self.counts.get(key).copied()
    }

    /// Text of the tag in the centre table's header (default "centre").
    pub fn set_centre_label(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.centre_label = text.into();
        cx.notify();
    }

    fn recompute_vis(&mut self) {
        let n = self.nodes.len();
        self.vis = vec![false; n];
        self.levels = vec![None; n];
        for (k, d) in scoped_tables(&self.model, &self.scope) {
            if let Some(&i) = self.index.get(&k) {
                self.vis[i] = !self.hidden.contains(&i);
                self.levels[i] = d;
            }
        }
        self.rel_vis = self
            .rels
            .iter()
            .map(|r| {
                self.vis[r.from]
                    && self.vis[r.to]
                    && (self.show_inferred || !self.model.relations[r.model_ix].inferred)
            })
            .collect();
        // the channel groups change with what is visible
        for r in &mut self.rels {
            r.pos = None;
        }
        self.centre = match &self.scope {
            Scope::Centre { table, .. } => self.index.get(table).copied(),
            _ => None,
        };
        let vis = self.vis.clone();
        self.sel.retain(|&i| vis[i]);
        if let Some(r) = self.sel_rel
            && !self.rel_vis[r]
        {
            self.sel_rel = None;
        }
        if let Some(r) = self.hover_rel
            && !self.rel_vis[r]
        {
            self.hover_rel = None;
        }
        if self.hover_table.is_some_and(|t| !vis[t]) {
            self.hover_table = None;
        }
    }

    fn stash_whole(&mut self) {
        self.stash = Some(self.nodes.iter().map(|n| (n.x, n.y, n.placed)).collect());
    }

    fn unplace_visible(&mut self) {
        for (n, v) in self.nodes.iter_mut().zip(&self.vis) {
            if *v {
                n.placed = false;
            }
        }
    }

    /// Run the automatic layout again for every visible table (user action:
    /// "auto arrange"). Manual positions of the visible tables are dropped.
    pub fn relayout(&mut self, cx: &mut Context<Self>) {
        self.unplace_visible();
        self.pending_fit = true;
        self.run_layout(false, cx);
        cx.notify();
        cx.emit(ErDiagramEvent::LayoutChanged);
    }

    /// Same as [`Self::relayout`].
    pub fn auto_arrange(&mut self, cx: &mut Context<Self>) {
        self.relayout(cx);
    }

    fn run_layout(&mut self, only_unplaced: bool, cx: &mut Context<Self>) {
        let ids: Vec<usize> = (0..self.nodes.len()).filter(|&i| self.vis[i]).collect();
        if ids.iter().all(|&i| self.nodes[i].placed) {
            return;
        }
        let local: HashMap<usize, usize> = ids.iter().enumerate().map(|(l, &g)| (g, l)).collect();
        let sizes: Vec<(f32, f32)> = ids
            .iter()
            .map(|&i| (self.nodes[i].w, self.nodes[i].h))
            .collect();
        let edges: Vec<(usize, usize)> = self
            .rels
            .iter()
            .enumerate()
            .filter(|(ri, _)| self.rel_vis[*ri])
            .map(|(_, r)| (local[&r.from], local[&r.to]))
            .collect();
        let levels: Option<Vec<u32>> = self
            .centre
            .map(|_| ids.iter().map(|&i| self.levels[i].unwrap_or(0)).collect());
        self.layout_gen += 1;
        let generation = self.layout_gen;
        if ids.len() < ASYNC_LAYOUT_FROM {
            let pos = compute_layout(&sizes, &edges, &levels);
            self.apply_layout(&ids, pos, only_unplaced);
        } else {
            self.layouting = true;
            cx.spawn(async move |this, cx| {
                let pos = cx
                    .background_executor()
                    .spawn(async move { compute_layout(&sizes, &edges, &levels) })
                    .await;
                this.update(cx, |this, cx| {
                    if this.layout_gen == generation {
                        this.apply_layout(&ids, pos, only_unplaced);
                        cx.notify();
                        cx.emit(ErDiagramEvent::LayoutChanged);
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    fn apply_layout(&mut self, ids: &[usize], pos: Vec<(f32, f32)>, only_unplaced: bool) {
        for (&i, p) in ids.iter().zip(pos) {
            let n = &mut self.nodes[i];
            if !(only_unplaced && n.placed) {
                n.x = p.0;
                n.y = p.1;
            }
            n.placed = true;
        }
        self.layouting = false;
    }

    // ---- camera -----------------------------------------------------------

    pub fn fit_to_view(&mut self, cx: &mut Context<Self>) {
        self.anim = None;
        self.pending_fit = true;
        self.fit_exact = true;
        cx.notify();
        cx.emit(ErDiagramEvent::LayoutChanged);
    }

    pub fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        self.set_zoom(1.0, cx);
    }

    /// Zoom around the viewport centre, clamped to 40 %–160 %.
    pub fn set_zoom(&mut self, z: f32, cx: &mut Context<Self>) {
        let b = self.bounds.get();
        let c = (b.size.width.as_f32() / 2.0, b.size.height.as_f32() / 2.0);
        let target = z.clamp(MIN_ZOOM, MAX_ZOOM);
        self.zoom_at(c, target / self.zoom, cx);
    }

    pub fn zoom_in(&mut self, cx: &mut Context<Self>) {
        self.set_zoom(self.zoom * ZOOM_STEP, cx);
    }

    pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
        self.set_zoom(self.zoom / ZOOM_STEP, cx);
    }

    /// Move a table by a world-space delta (also for the bench and the host).
    pub fn nudge_table(&mut self, ix: usize, dx: f32, dy: f32, cx: &mut Context<Self>) {
        if let Some(n) = self.nodes.get_mut(ix) {
            n.x += dx;
            n.y += dy;
            cx.notify();
        }
    }

    /// Pan by a screen-space delta.
    pub fn pan_by(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) {
        self.anim = None;
        self.pan = (self.pan.0 + dx, self.pan.1 + dy);
        self.pending_fit = false;
        cx.notify();
    }

    /// Put the centre of the diagram under the viewport centre at `zoom`.
    pub fn view_centered(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let b = self.bounds.get();
        if let Some((x0, y0, x1, y1)) = self.world_bbox() {
            let old = self.zoom;
            self.zoom = zoom.clamp(FIT_MIN_ZOOM, MAX_ZOOM);
            self.pan = (
                b.size.width.as_f32() / 2.0 - (x0 + x1) / 2.0 * self.zoom,
                b.size.height.as_f32() / 2.0 - (y0 + y1) / 2.0 * self.zoom,
            );
            self.pending_fit = false;
            self.zoom_event(old, cx);
            cx.notify();
        }
    }

    /// Fly the camera to a table, select it and pulse its border once.
    pub fn focus_table(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(&i) = self.index.get(key) else {
            return;
        };
        if !self.vis[i] {
            return;
        }
        let b = self.bounds.get();
        let (vw, vh) = (b.size.width.as_f32(), b.size.height.as_f32());
        let n = &self.nodes[i];
        let to_z = if self.zoom < 0.7 { 1.0 } else { self.zoom };
        // keep a tall card's header in view
        let cy = if n.h * to_z > vh * 0.8 {
            n.y + vh * 0.4 / to_z
        } else {
            n.y + n.h / 2.0
        };
        self.anim = Some(Anim {
            start: Instant::now(),
            from_c: self.to_world((vw / 2.0, vh / 2.0)),
            to_c: (n.x + n.w / 2.0, cy),
            from_z: self.zoom,
            to_z,
        });
        self.pending_fit = false;
        self.pulse = Some((i, Instant::now()));
        self.order.retain(|&x| x != i);
        self.order.push(i);
        self.sel = BTreeSet::from([i]);
        self.sel_rel = None;
        self.emit_selection(cx);
        cx.notify();
    }

    /// Advance the camera flight; true while it runs.
    fn step_anim(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(a) = self.anim else {
            return false;
        };
        let b = self.bounds.get();
        let (vw, vh) = (b.size.width.as_f32(), b.size.height.as_f32());
        let t = (a.start.elapsed().as_secs_f32() / FLY_S).min(1.0);
        let e = ease(t);
        let old = self.zoom;
        self.zoom = a.from_z * (a.to_z / a.from_z).powf(e);
        let c = (
            a.from_c.0 + (a.to_c.0 - a.from_c.0) * e,
            a.from_c.1 + (a.to_c.1 - a.from_c.1) * e,
        );
        self.pan = (vw / 2.0 - c.0 * self.zoom, vh / 2.0 - c.1 * self.zoom);
        if t >= 1.0 {
            self.anim = None;
            self.zoom_event(a.from_z.min(old), cx);
            cx.emit(ErDiagramEvent::ZoomChanged(self.zoom));
            cx.emit(ErDiagramEvent::LayoutChanged);
            return false;
        }
        true
    }

    fn world_bbox(&self) -> Option<(f32, f32, f32, f32)> {
        let mut r = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        let mut any = false;
        for (n, v) in self.nodes.iter().zip(&self.vis) {
            if *v {
                any = true;
                r = (
                    r.0.min(n.x),
                    r.1.min(n.y),
                    r.2.max(n.x + n.w),
                    r.3.max(n.y + n.h),
                );
            }
        }
        any.then_some(r)
    }

    /// A host that has its own toolbar (zoom, fit, arrange) hides the floating chips.
    pub fn set_floating_toolbar(&mut self, on: bool) {
        self.floating_toolbar = on;
    }

    fn do_fit(&mut self, vw: f32, vh: f32) {
        if let Some((x0, y0, x1, y1)) = self.world_bbox() {
            let m = 40.0;
            let z = ((vw - 2.0 * m) / (x1 - x0).max(1.0)).min((vh - 2.0 * m) / (y1 - y0).max(1.0));
            // On open (and after a re-layout) never go below a readable size: the
            // canvas scrolls instead. Only the "fit" button may shrink to the overview.
            let floor = if self.fit_exact {
                FIT_MIN_ZOOM
            } else {
                OPEN_MIN_ZOOM
            };
            self.fit_exact = false;
            self.zoom = z.clamp(floor, 1.2);
            // Centre what fits; start at the top-left margin what does not.
            let axis = |view: f32, lo: f32, hi: f32, zoom: f32| {
                if (hi - lo) * zoom + 2.0 * m <= view {
                    (view - (hi - lo) * zoom) / 2.0 - lo * zoom
                } else {
                    m - lo * zoom
                }
            };
            self.pan = (axis(vw, x0, x1, self.zoom), axis(vh, y0, y1, self.zoom));
        }
    }

    fn zoom_event(&self, old: f32, cx: &mut Context<Self>) {
        if (self.zoom - old).abs() > 1e-4 {
            cx.emit(ErDiagramEvent::ZoomChanged(self.zoom));
        }
    }

    fn zoom_at(&mut self, p: (f32, f32), factor: f32, cx: &mut Context<Self>) {
        self.anim = None;
        let old = self.zoom;
        // never jump when zooming out from a "fit" below the user range
        let nz = if old < MIN_ZOOM && factor > 1.0 {
            MIN_ZOOM
        } else {
            (old * factor).clamp(MIN_ZOOM.min(old), MAX_ZOOM)
        };
        let f = nz / old;
        self.pan = (p.0 - (p.0 - self.pan.0) * f, p.1 - (p.1 - self.pan.1) * f);
        self.zoom = nz;
        self.pending_fit = false;
        cx.notify();
        self.zoom_event(old, cx);
        cx.emit(ErDiagramEvent::LayoutChanged);
    }

    // ---- geometry -------------------------------------------------------

    fn local(&self, p: Point<Pixels>) -> (f32, f32) {
        let o = self.bounds.get().origin;
        (p.x.as_f32() - o.x.as_f32(), p.y.as_f32() - o.y.as_f32())
    }

    fn to_world(&self, p: (f32, f32)) -> (f32, f32) {
        (
            (p.0 - self.pan.0) / self.zoom,
            (p.1 - self.pan.1) / self.zoom,
        )
    }

    fn rect(&self, i: usize) -> Rect {
        let n = &self.nodes[i];
        (n.x, n.y, n.w, n.h)
    }

    /// Centre of column `col`'s row on card `ti`; the header centre when the
    /// column is not shown.
    fn row_y(&self, ti: usize, col: Option<usize>) -> f32 {
        let n = &self.nodes[ti];
        match col.and_then(|c| n.slot.get(c).copied().flatten()) {
            Some(s) => n.y + HEADER_H + s as f32 * ROW_H + ROW_H / 2.0,
            None => n.y + HEADER_H / 2.0,
        }
    }

    /// Recompute the geometry of visible edges whose cards moved or resized
    /// (cached per edge; untouched edges cost a comparison).
    fn refresh_geoms(&mut self) {
        // 1. where each visible line's cards and rows are; did anything move?
        let mut moved = false;
        for ri in 0..self.rels.len() {
            if !self.rel_vis[ri] {
                continue;
            }
            let pos = self.line_pos(ri);
            let r = &mut self.rels[ri];
            if r.pos != Some(pos) || r.geom.is_none() {
                r.pos = Some(pos);
                moved = true;
            }
        }
        if !moved {
            return;
        }
        // 2. channel offsets: lines into the same card from the same side
        self.chan.clear();
        self.chan_ix.clear();
        for (ri, r) in self.rels.iter().enumerate() {
            if let (true, Some(p)) = (self.rel_vis[ri], r.pos) {
                let (a, b) = ((p[0], p[1], p[2], p[3]), (p[5], p[6], p[7], p[8]));
                self.chan
                    .push((r.to, route::side(a, b), (p[4] - p[9]).abs()));
                self.chan_ix.push(ri);
            }
        }
        route::channel_offsets(&self.chan, &mut self.chan_scratch, &mut self.chan_off);
        // 3. new geometry for the lines whose position or offset changed
        for k in 0..self.chan_ix.len() {
            let ri = self.chan_ix[k];
            let off = self.chan_off[k];
            let r = &mut self.rels[ri];
            let Some(p) = r.pos else { continue };
            r.offset = off;
            let key = [
                p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], p[8], p[9], off,
            ];
            if matches!(&r.geom, Some((g, _)) if *g == key) {
                continue;
            }
            let (a, b) = ((p[0], p[1], p[2], p[3]), (p[5], p[6], p[7], p[8]));
            let rt = route::route(a, p[4], b, p[9], off);
            r.geom = Some((key, route::edge_path(&rt, self.edge_style)));
        }
    }

    /// Card boxes and row centres of line `ri`: [a.x, a.y, a.w, a.h, ya, b.x, b.y, b.w, b.h, yb].
    fn line_pos(&self, ri: usize) -> [f32; 10] {
        let r = &self.rels[ri];
        let (a, b) = (self.rect(r.from), self.rect(r.to));
        let mut ya = self.row_y(r.from, r.from_cols.first().copied());
        let mut yb = self.row_y(r.to, r.to_cols.first().copied());
        if r.from == r.to && (ya - yb).abs() < 12.0 {
            // a self reference on a collapsed card: keep the loop open
            let m = (ya + yb) / 2.0;
            (ya, yb) = (m + 6.0, m - 6.0);
        }
        [a.0, a.1, a.2, a.3, ya, b.0, b.1, b.2, b.3, yb]
    }

    fn hit_table(&self, w: (f32, f32)) -> Option<usize> {
        self.order.iter().rev().copied().find(|&i| {
            let n = &self.nodes[i];
            self.vis[i] && w.0 >= n.x && w.0 <= n.x + n.w && w.1 >= n.y && w.1 <= n.y + n.h
        })
    }

    /// Is world point `w` on the collapse chevron of card `i`?
    fn hit_chevron(&self, i: usize, w: (f32, f32)) -> bool {
        let n = &self.nodes[i];
        self.zoom >= 0.5 && w.1 <= n.y + HEADER_H && w.0 >= n.x + n.w - CHEVRON_W
    }

    /// Nearest line within `HIT_TOL` screen pixels of its stroke (any zoom).
    /// Needs fresh geometry (`refresh_geoms`).
    fn hit_rel(&self, w: (f32, f32)) -> Option<usize> {
        let lw = (1.5 * self.zoom).clamp(1.2, 2.4);
        let tol = (HIT_TOL + lw / 2.0) / self.zoom;
        (0..self.rels.len())
            .filter(|&i| self.rel_vis[i])
            .filter_map(|i| {
                let (_, e) = self.rels[i].geom.as_ref()?;
                route::near_box(e.bbox, w, tol).then(|| (i, route::distance(e, w)))
            })
            .filter(|(_, d)| *d <= tol)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    fn emit_selection(&mut self, cx: &mut Context<Self>) {
        // a new selection: the detail panel opens again, a JOIN path is done
        self.panel_closed = false;
        self.path_hl = None;
        self.chooser = None;
        cx.emit(ErDiagramEvent::SelectionChanged(self.selection()));
    }

    /// Esc: close the search, the JOIN path chooser, the detail panel, the
    /// path highlight, else clear the selection. False when there was
    /// nothing to do (the host may use Esc for something else).
    pub fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.search.is_some() {
            self.close_search(window, cx);
            return true;
        }
        if self.chooser.take().is_some() {
            cx.notify();
            return true;
        }
        if self.panel_table().is_some() {
            self.close_panel(cx);
            return true;
        }
        if self.path_hl.take().is_some() {
            cx.notify();
            return true;
        }
        if self.sel.is_empty() && self.sel_rel.is_none() {
            return false;
        }
        self.sel.clear();
        self.sel_rel = None;
        self.emit_selection(cx);
        cx.notify();
        true
    }

    // ---- search -----------------------------------------------------------

    /// Open (or focus) the search field. Typing filters the cards, Enter
    /// flies to the next hit.
    pub fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(s) = &self.search {
            s.input.update(cx, |i, cx| i.focus(window, cx));
            return;
        }
        let ph = self.labels.search.clone();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(ph));
        let sub = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, st, ev: &InputEvent, _, cx| match ev {
                InputEvent::Change => {
                    let q = st.read(cx).value().to_string();
                    this.set_query(q, cx);
                }
                InputEvent::PressEnter { .. } => this.search_next(cx),
                _ => {}
            },
        );
        input.update(cx, |i, cx| i.focus(window, cx));
        self.search = Some(Search {
            input,
            query: String::new(),
            matches: vec![],
            cur: None,
            _sub: sub,
        });
        cx.notify();
    }

    /// Open the search with `q` typed in.
    pub fn search_for(&mut self, q: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(window, cx);
        if let Some(s) = &self.search {
            let input = s.input.clone();
            input.update(cx, |i, cx| i.set_value(q.to_string(), window, cx));
        }
        self.set_query(q.to_string(), cx);
    }

    pub fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    pub fn search_open(&self) -> bool {
        self.search.is_some()
    }

    fn set_query(&mut self, q: String, cx: &mut Context<Self>) {
        let matches = search_hits(
            self.model
                .tables
                .iter()
                .enumerate()
                .filter(|(i, _)| self.vis[*i])
                .map(|(i, t)| (i, t.name.as_str())),
            &q,
        );
        if let Some(s) = &mut self.search {
            s.query = q;
            s.matches = matches;
            s.cur = None;
        }
        cx.notify();
    }

    fn search_next(&mut self, cx: &mut Context<Self>) {
        let Some(s) = &mut self.search else {
            return;
        };
        if s.matches.is_empty() {
            return;
        }
        let c = s.cur.map_or(0, |c| (c + 1) % s.matches.len());
        s.cur = Some(c);
        let key = self.nodes[s.matches[c]].key.clone();
        self.focus_table(&key, cx);
    }

    // ---- input ----------------------------------------------------------

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        if k.key == "escape" {
            if self.escape(window, cx) {
                cx.stop_propagation();
            }
        } else if k.key == "f" && (k.modifiers.control || k.modifiers.platform) {
            self.open_search(window, cx);
            cx.stop_propagation();
        }
    }

    /// Centre the view on the world point under minimap point `p`.
    fn pan_to_minimap(&mut self, mm: MiniMap, p: (f32, f32), cx: &mut Context<Self>) {
        let b = self.bounds.get();
        let w = mm.map_to_world(p);
        self.anim = None;
        self.pan = (
            b.size.width.as_f32() / 2.0 - w.0 * self.zoom,
            b.size.height.as_f32() / 2.0 - w.1 * self.zoom,
        );
        self.pending_fit = false;
        cx.notify();
    }

    fn on_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let p = self.local(ev.position);
        if self.minimap
            && let Some(mm) = self.mm.get()
            && mm.contains(p)
        {
            self.drag = Drag::Minimap(mm);
            self.pan_to_minimap(mm, p, cx);
            return;
        }
        let w = self.to_world(p);
        let shift = ev.modifiers.shift;
        if let Some(t) = self.hit_table(w) {
            if ev.click_count == 1 && !shift && self.hit_chevron(t, w) {
                self.toggle_card_ix(t, cx);
                return;
            }
            self.order.retain(|&i| i != t);
            self.order.push(t);
            self.sel_rel = None;
            if ev.click_count >= 2 {
                self.sel = BTreeSet::from([t]);
                cx.emit(ErDiagramEvent::OpenTable(self.nodes[t].key.clone()));
                self.emit_selection(cx);
                cx.notify();
                return;
            }
            if shift {
                if !self.sel.remove(&t) {
                    self.sel.insert(t);
                }
            } else if !self.sel.contains(&t) {
                self.sel = BTreeSet::from([t]);
            }
            let items = self
                .sel
                .iter()
                .map(|&i| (i, self.nodes[i].x, self.nodes[i].y))
                .collect();
            self.drag = Drag::Move {
                start: p,
                items,
                moved: false,
                clicked: t,
                shift,
            };
            self.emit_selection(cx);
        } else if shift {
            self.drag = Drag::Rubber {
                start: p,
                cur: p,
                base: self.sel.clone(),
            };
        } else if let Some(r) = {
            self.refresh_geoms();
            self.hit_rel(w)
        } {
            self.sel.clear();
            self.sel_rel = Some(r);
            self.emit_selection(cx);
            cx.emit(ErDiagramEvent::RelationClicked(
                self.model.relations[self.rels[r].model_ix].clone(),
            ));
        } else {
            self.drag = Drag::Pan {
                start: p,
                pan0: self.pan,
                moved: false,
            };
        }
        cx.notify();
    }

    fn on_right_down(&mut self, ev: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let w = self.to_world(self.local(ev.position));
        if let Some(t) = self.hit_table(w) {
            if !self.sel.contains(&t) {
                self.sel = BTreeSet::from([t]);
                self.sel_rel = None;
                self.emit_selection(cx);
            }
            cx.emit(ErDiagramEvent::TableContextMenu {
                table: self.nodes[t].key.clone(),
                position: ev.position,
            });
            cx.notify();
        }
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let p = self.local(ev.position);
        let z = self.zoom;
        let mut d = std::mem::take(&mut self.drag);
        let dragging = ev.pressed_button == Some(MouseButton::Left);
        match &mut d {
            Drag::Move {
                start,
                items,
                moved,
                ..
            } if dragging => {
                let (dx, dy) = (p.0 - start.0, p.1 - start.1);
                if *moved || dx.hypot(dy) > DRAG_THRESHOLD {
                    *moved = true;
                    for &(i, x0, y0) in items.iter() {
                        self.nodes[i].x = x0 + dx / z;
                        self.nodes[i].y = y0 + dy / z;
                    }
                    cx.notify();
                }
            }
            Drag::Pan { start, pan0, moved } if dragging => {
                let (dx, dy) = (p.0 - start.0, p.1 - start.1);
                if *moved || dx.hypot(dy) > DRAG_THRESHOLD {
                    *moved = true;
                    self.anim = None;
                    self.pan = (pan0.0 + dx, pan0.1 + dy);
                    self.pending_fit = false;
                    cx.notify();
                }
            }
            Drag::Rubber { start, cur, base } if dragging => {
                *cur = p;
                let (a, b) = (self.to_world(*start), self.to_world(p));
                let (x0, x1, y0, y1) = (a.0.min(b.0), a.0.max(b.0), a.1.min(b.1), a.1.max(b.1));
                let mut sel = base.clone();
                for (i, n) in self.nodes.iter().enumerate() {
                    if self.vis[i] && n.x < x1 && n.x + n.w > x0 && n.y < y1 && n.y + n.h > y0 {
                        sel.insert(i);
                    }
                }
                self.sel = sel;
                cx.notify();
            }
            Drag::Minimap(mm) if dragging => {
                let mm = *mm;
                self.pan_to_minimap(mm, p, cx);
            }
            Drag::None => {
                let panel_x = self.bounds.get().size.width.as_f32() - PANEL_W;
                let on_mm = self.minimap && self.mm.get().is_some_and(|m| m.contains(p))
                    || (self.panel_table().is_some() && p.0 >= panel_x);
                let w = self.to_world(p);
                let t = if on_mm { None } else { self.hit_table(w) };
                let h = if on_mm || t.is_some() {
                    None
                } else {
                    self.refresh_geoms();
                    self.hit_rel(w)
                };
                // a row with the foreign-key warning under the pointer
                let warn = t.and_then(|ti| {
                    let n = &self.nodes[ti];
                    let slot = ((w.1 - n.y - HEADER_H) / ROW_H).floor();
                    let col = (slot >= 0.0)
                        .then(|| n.shown.get(slot as usize))
                        .flatten()?;
                    self.fk_warn[ti][*col].then_some((ti, *col))
                });
                let moved_tip = (h.is_some() || warn.is_some()) && p != self.hover_pos;
                self.hover_pos = p;
                if h != self.hover_rel
                    || t != self.hover_table
                    || warn != self.hover_warn
                    || moved_tip
                {
                    self.hover_rel = h;
                    self.hover_table = t;
                    self.hover_warn = warn;
                    cx.notify();
                }
            }
            _ => {}
        }
        self.drag = if dragging { d } else { Drag::None };
    }

    fn on_hover_out(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if !*hovered && (self.hover_rel.is_some() || self.hover_table.is_some()) {
            self.hover_rel = None;
            self.hover_table = None;
            cx.notify();
        }
    }

    fn on_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        match std::mem::take(&mut self.drag) {
            Drag::Move {
                moved,
                clicked,
                shift,
                ..
            } => {
                if moved {
                    cx.emit(ErDiagramEvent::LayoutChanged);
                } else if !shift && self.sel.len() > 1 {
                    self.sel = BTreeSet::from([clicked]);
                    self.emit_selection(cx);
                }
            }
            Drag::Pan { moved, .. } => {
                if moved {
                    cx.emit(ErDiagramEvent::LayoutChanged);
                } else if !self.sel.is_empty() || self.sel_rel.is_some() {
                    self.sel.clear();
                    self.sel_rel = None;
                    self.emit_selection(cx);
                }
            }
            Drag::Rubber { .. } => self.emit_selection(cx),
            Drag::Minimap(_) => cx.emit(ErDiagramEvent::LayoutChanged),
            Drag::None => {}
        }
        cx.notify();
    }

    fn on_scroll(&mut self, ev: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        if ev.modifiers.control {
            let dy = ev.delta.pixel_delta(px(53.0)).y.as_f32();
            let p = self.local(ev.position);
            self.zoom_at(p, (dy * 0.002).exp(), cx);
        } else {
            let d = ev.delta.pixel_delta(px(40.0));
            let (dx, dy) = if ev.modifiers.shift {
                (d.y.as_f32(), 0.0)
            } else {
                (d.x.as_f32(), d.y.as_f32())
            };
            self.pan_by(dx, dy, cx);
            cx.emit(ErDiagramEvent::LayoutChanged);
        }
    }

    // ---- overlays -------------------------------------------------------

    /// Tooltip of the hovered line: `orders.customer_id → customers.id` and
    /// the constraint name.
    fn relation_tip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !matches!(self.drag, Drag::None) {
            return None;
        }
        // (monospace line, muted line)
        let (main, sub): (String, SharedString) = if let Some(ri) = self.hover_rel {
            let r = &self.model.relations[self.rels[ri].model_ix];
            let sub = if r.inferred {
                self.labels.inferred.clone()
            } else {
                r.name.clone().into()
            };
            (relation_text(r), sub)
        } else {
            let (ti, ci) = self.hover_warn?;
            let t = &self.model.tables[ti];
            (
                format!("{}.{}", t.name, t.columns[ci].name),
                self.labels.fk_no_index.clone(),
            )
        };
        let t = cx.theme();
        let b = self.bounds.get();
        let (x, y) = self.hover_pos;
        let left = if x + 320.0 > b.size.width.as_f32() {
            (x - 320.0).max(4.0)
        } else {
            x + 14.0
        };
        Some(
            div()
                .absolute()
                .left(px(left))
                .top(px(y + 18.0))
                .max_w(px(320.0))
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(t.border)
                .bg(t.popover)
                .shadow_md()
                .text_xs()
                .text_color(t.popover_foreground)
                .child(div().font_family(t.mono_font_family.clone()).child(main))
                .when(!sub.is_empty(), |d| {
                    d.child(div().text_color(t.muted_foreground).child(sub))
                })
                .into_any_element(),
        )
    }

    /// Right-side panel of the single selected table: columns (type, PK /
    /// FK / NULL, default, comment), indexes, row count, comment, and the
    /// actions open data, structure, `SELECT *`, copy name.
    fn detail_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let i = self.panel_table()?;
        let tm = &self.model.tables[i];
        let key = self.nodes[i].key.clone();
        let t = cx.theme().clone();
        let l = &self.labels;
        let small = |s: SharedString| div().text_xs().text_color(t.muted_foreground).child(s);
        let tag = |text: &'static str, c: Hsla| {
            div()
                .flex_none()
                .px_1()
                .rounded_sm()
                .bg(c.opacity(0.15))
                .text_color(c)
                .text_size(px(10.0))
                .font_weight(FontWeight::SEMIBOLD)
                .child(text)
        };
        let mut cols = v_flex().gap_1();
        for (c, col) in tm.columns.iter().enumerate() {
            let mut tags = h_flex().gap_1().items_center();
            if col.primary_key {
                tags = tags.child(tag("PK", t.warning));
            }
            if self.nodes[i].fk[c] {
                tags = tags.child(tag("FK", t.info));
            }
            if col.nullable {
                tags = tags.child(tag("NULL", t.muted_foreground));
            }
            if self.fk_warn[i][c] {
                tags = tags.child(tag("!", t.warning));
            }
            cols = cols.child(
                v_flex()
                    .py_1()
                    .border_b_1()
                    .border_color(t.border.opacity(0.5))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(t.mono_font_family.clone())
                                    .text_sm()
                                    .child(col.name.clone()),
                            )
                            .child(tags),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .font_family(t.mono_font_family.clone())
                            .child(col.type_name.clone())
                            .children(col.default.clone().map(|d| format!("= {d}"))),
                    )
                    .children(col.comment.clone().map(|c| small(c.into()))),
            );
        }
        let mut idx = v_flex().gap_1();
        for ix in &tm.indexes {
            idx = idx.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .child(
                        div()
                            .flex_none()
                            .font_family(t.mono_font_family.clone())
                            .child(ix.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(t.muted_foreground)
                            .child(format!("({})", ix.columns.join(", "))),
                    )
                    .when(ix.primary || ix.unique, |d| {
                        d.child(tag(if ix.primary { "PK" } else { "UNIQUE" }, t.warning))
                    }),
            );
        }
        let btn =
            |id: &'static str, text: SharedString| Button::new(id).xsmall().outline().label(text);
        let (k1, k2, k3, name) = (key.clone(), key.clone(), key, tm.name.clone());
        let count = self.counts.get(&self.nodes[i].key).copied();
        Some(
            v_flex()
                .id("er-panel")
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .w(px(PANEL_W))
                .bg(t.popover)
                .border_l_1()
                .border_color(t.border)
                .text_color(t.popover_foreground)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_scroll(|_, _, cx| cx.stop_propagation())
                .child(
                    h_flex()
                        .flex_none()
                        .h(px(40.0))
                        .pl_3()
                        .pr(px(8.0))
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(t.border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(tm.name.clone()),
                        )
                        .children(
                            count.map(|c| small(format!("{} {}", fmt_count(c), l.rows).into())),
                        )
                        .child(
                            div()
                                .id("er-panel-close")
                                .flex_none()
                                .size(px(24.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_sm()
                                .cursor_pointer()
                                .hover(|s| s.bg(t.muted))
                                .child(glyph(Glyph::Close, t.muted_foreground, 12.0))
                                .on_click(cx.listener(|this, _, _, cx| this.close_panel(cx))),
                        ),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .flex_wrap()
                        .gap_1()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(t.border)
                        .child(btn("er-p-data", l.open_data.clone()).on_click(cx.listener(
                            move |_, _, _, cx| cx.emit(ErDiagramEvent::OpenTable(k1.clone())),
                        )))
                        .child(
                            btn("er-p-struct", l.structure.clone()).on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.emit(ErDiagramEvent::OpenStructure(k2.clone()))
                                },
                            )),
                        )
                        .child(
                            btn("er-p-select", l.select_star.clone()).on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.emit(ErDiagramEvent::InsertSelect(k3.clone()))
                                },
                            )),
                        )
                        .child(btn("er-p-copy", l.copy_name.clone()).on_click(cx.listener(
                            move |_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(name.clone()))
                            },
                        ))),
                )
                .child(
                    v_flex()
                        .id("er-panel-body")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_3()
                        .py_2()
                        .gap_3()
                        .children(tm.comment.clone().map(|c| small(c.into())))
                        .child(
                            v_flex()
                                .child(small(l.columns.clone()).font_weight(FontWeight::SEMIBOLD))
                                .child(cols),
                        )
                        .when(!tm.indexes.is_empty(), |d| {
                            d.child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        small(l.indexes.clone()).font_weight(FontWeight::SEMIBOLD),
                                    )
                                    .child(idx),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    /// Chooser between JOIN paths of the same length.
    fn path_chooser(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let list = self.chooser.as_ref()?;
        let t = cx.theme().clone();
        let mut col = v_flex()
            .id("er-paths")
            .absolute()
            .top(px(48.0))
            .left_2()
            .w(px(340.0))
            .p_1()
            .gap(px(2.0))
            .rounded_md()
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .shadow_md()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(self.labels.choose_path.clone()),
            );
        for (k, (text, ..)) in list.iter().enumerate() {
            col = col.child(
                div()
                    .id(("er-path", k))
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_sm()
                    .hover(|s| s.bg(t.muted))
                    .child(text.clone())
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_path(k, cx))),
            );
        }
        Some(col.into_any_element())
    }

    /// Legend of the colour groups (bottom left).
    fn group_legend(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.color_groups || self.groups.0.is_empty() {
            return None;
        }
        let t = cx.theme();
        let tints = [t.blue, t.green, t.yellow, t.magenta, t.cyan, t.red];
        let mut col = v_flex()
            .absolute()
            .bottom(px(12.0))
            .left(px(12.0))
            .px_2()
            .py_1()
            .gap_1()
            .rounded_md()
            .border_1()
            .border_color(t.border)
            .bg(t.popover.opacity(0.94))
            .text_xs()
            .text_color(t.popover_foreground);
        for (g, name) in self.groups.0.iter().enumerate().take(12) {
            col = col.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_none()
                            .size(px(10.0))
                            .rounded_sm()
                            .bg(t.muted.blend(tints[g % tints.len()].opacity(0.24)))
                            .border_1()
                            .border_color(tints[g % tints.len()].opacity(0.6)),
                    )
                    .child(name.clone()),
            );
        }
        Some(col.into_any_element())
    }

    /// The Ctrl+F field: input, hit count, close.
    fn search_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.search.as_ref()?;
        let t = cx.theme();
        let count = match (s.query.trim().is_empty(), s.cur) {
            (true, _) => String::new(),
            (false, Some(c)) if c < s.matches.len() => format!("{}/{}", c + 1, s.matches.len()),
            (false, _) => s.matches.len().to_string(),
        };
        let (muted, hover) = (t.muted_foreground, t.muted);
        Some(
            h_flex()
                .id("er-search")
                .absolute()
                .top_2()
                .left_2()
                .w(px(300.0))
                .h(px(32.0))
                .pl(px(10.0))
                .pr(px(4.0))
                .gap(px(6.0))
                .items_center()
                .rounded_md()
                .border_1()
                .border_color(t.border)
                .bg(t.popover)
                .shadow_md()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                    if ev.keystroke.key == "escape" {
                        this.close_search(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(glyph(Glyph::Search, muted, 14.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&s.input).small().appearance(false)),
                )
                .child(div().flex_none().text_xs().text_color(muted).child(count))
                .child(
                    div()
                        .id("er-search-close")
                        .flex_none()
                        .size(px(24.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .child(glyph(Glyph::Close, muted, 12.0))
                        .on_click(cx.listener(|this, _, window, cx| this.close_search(window, cx))),
                )
                .into_any_element(),
        )
    }
}

/// Small icons the view paints itself, so it needs no icon assets from the host.
#[derive(Clone, Copy)]
enum Glyph {
    Search,
    Close,
}

/// A `size`-pixel square with the glyph stroked in `color`.
fn glyph(kind: Glyph, color: Hsla, size: f32) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let (x, y) = (b.origin.x.as_f32(), b.origin.y.as_f32());
            let s = size;
            let p = |u: f32, v: f32| point(px(x + u * s), px(y + v * s));
            let mut pb = PathBuilder::stroke(px((s / 9.0).clamp(1.2, 1.8)));
            match kind {
                Glyph::Search => {
                    // circle (polygon) and handle
                    let (cx_, cy_, r) = (0.42, 0.42, 0.3);
                    let pts: Vec<Point<Pixels>> = (0..=24)
                        .map(|k| {
                            let a = k as f32 / 24.0 * std::f32::consts::TAU;
                            p(cx_ + r * a.cos(), cy_ + r * a.sin())
                        })
                        .collect();
                    pb.add_polygon(&pts, true);
                    pb.move_to(p(0.64, 0.64));
                    pb.line_to(p(0.92, 0.92));
                }
                Glyph::Close => {
                    pb.move_to(p(0.15, 0.15));
                    pb.line_to(p(0.85, 0.85));
                    pb.move_to(p(0.85, 0.15));
                    pb.line_to(p(0.15, 0.85));
                }
            }
            if let Ok(path) = pb.build() {
                window.paint_path(path, color);
            }
        },
    )
    .flex_none()
    .size(px(size))
}

impl Render for ErDiagram {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity();
        let cell = self.bounds.clone();
        let bg = cx.theme().background;
        let empty = !self.vis.iter().any(|v| *v);
        let mut root = div()
            .id("er-diagram")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(bg)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_down))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_up))
            .on_hover(cx.listener(Self::on_hover_out))
            .on_scroll(cx.listener(Self::on_scroll))
            .child(
                canvas(
                    move |b, _, _| cell.set(b),
                    move |b, _, window, cx| {
                        me.update(cx, |s, cx| s.paint_scene(b, window, cx));
                    },
                )
                .absolute()
                .size_full(),
            );
        if self.hover_rel.is_some() {
            root = root.cursor_pointer();
        }
        if empty {
            root = root.child(
                div()
                    .absolute()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.labels.empty.clone()),
            );
        } else if self.floating_toolbar {
            let l = self.labels.clone();
            let btn = |id: &'static str, text: SharedString| {
                Button::new(id).small().label(text.clone()).tooltip(text)
            };
            root = root.child(
                h_flex()
                    .absolute()
                    .top_2()
                    .right_2()
                    .gap_1()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        btn("er-fit", l.fit).on_click(cx.listener(|t, _, _, cx| t.fit_to_view(cx))),
                    )
                    .child(
                        btn("er-reset", l.reset_zoom)
                            .on_click(cx.listener(|t, _, _, cx| t.reset_zoom(cx))),
                    )
                    .child(
                        btn("er-relayout", l.relayout)
                            .on_click(cx.listener(|t, _, _, cx| t.relayout(cx))),
                    ),
            );
        }
        root.children(self.relation_tip(cx))
            .children(self.group_legend(cx))
            .children(self.detail_panel(cx))
            .children(self.path_chooser(cx))
            .children(self.search_bar(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::{fmt_count, relation_text, search_hits};
    use crate::Relation;

    #[test]
    fn counts_are_compact() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(950), "950");
        assert_eq!(fmt_count(1_000), "1k");
        assert_eq!(fmt_count(12_340), "12.3k");
        assert_eq!(fmt_count(123_400), "123k");
        assert_eq!(fmt_count(1_260_000), "1.3M");
        assert_eq!(fmt_count(4_000_000_000), "4B");
    }

    #[test]
    fn tooltip_text_lists_composite_keys() {
        let r = |fc: &[&str], tc: &[&str]| Relation {
            name: "fk".into(),
            from_table: "s.orders".into(),
            from_columns: fc.iter().map(|s| s.to_string()).collect(),
            to_table: "s.customers".into(),
            to_columns: tc.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        assert_eq!(
            relation_text(&r(&["customer_id"], &["id"])),
            "orders.customer_id → customers.id"
        );
        assert_eq!(
            relation_text(&r(&["cid", "tenant"], &["id", "tenant"])),
            "orders.(cid, tenant) → customers.(id, tenant)"
        );
    }

    #[test]
    fn search_ranks_prefix_hits_first() {
        let names = ["order_items", "orders", "customers", "pre_orders", "x"];
        let hits = search_hits(names.iter().copied().enumerate(), "ORDER");
        assert_eq!(hits, [1, 0, 3]);
        assert!(search_hits(names.iter().copied().enumerate(), "  ").is_empty());
        assert!(search_hits(names.iter().copied().enumerate(), "zzz").is_empty());
    }
}

/// View transform and visible area of one paint pass.
#[derive(Clone, Copy)]
struct Frame {
    z: f32,
    ox: f32,
    oy: f32,
    bx: f32,
    by: f32,
    vw: f32,
    vh: f32,
}

struct Pal {
    bg: Hsla,
    card: Hsla,
    header: Hsla,
    border: Hsla,
    fg: Hsla,
    muted: Hsla,
    primary: Hsla,
    key: Hsla,
    info: Hsla,
    line: Hsla,
    /// Colour-group tints (theme base colours, used muted) and the warning badge.
    tints: [Hsla; 6],
    warn: Hsla,
    mono: SharedString,
}

/// Everything the paint helpers share for one frame.
struct Scene<'a> {
    f: Frame,
    pal: &'a Pal,
    fpx: f32,
    lh: f32,
    /// Highlighted rows: (table, column).
    rows: &'a [(usize, usize)],
}

/// Screen box of one card and the height of its header.
#[derive(Clone, Copy)]
struct CardBox {
    nx: f32,
    ny: f32,
    nw: f32,
    nh: f32,
    hh: f32,
}
