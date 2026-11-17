use crate::layout::{auto_layout, level_layout};
use crate::model::{
    DiagramLayout, DiagramModel, HEADER_H, ROW_H, Relation, Scope, card_size, scoped_tables,
    to_mermaid,
};
use crate::route::{self, Rect, Route};
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex};
use gpui_kit::*;
use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

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

/// Texts the host supplies (the crate itself shows no hard-coded text).
#[derive(Clone, Debug, Default)]
pub struct Labels {
    /// Shown in the middle when the model has no tables.
    pub empty: SharedString,
    /// Toolbar buttons; each text is also the button's tooltip.
    pub fit: SharedString,
    pub reset_zoom: SharedString,
    pub relayout: SharedString,
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
    text: Option<TextCache>,
}

struct Rel {
    from: usize,
    to: usize,
    from_row: Option<usize>,
    to_row: Option<usize>,
    /// Index into `DiagramModel::relations`.
    model_ix: usize,
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
    sel: BTreeSet<usize>,
    sel_rel: Option<usize>,
    hover_rel: Option<usize>,
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
}

impl EventEmitter<ErDiagramEvent> for ErDiagram {}

struct Pal {
    card: Hsla,
    header: Hsla,
    border: Hsla,
    fg: Hsla,
    muted: Hsla,
    primary: Hsla,
    key: Hsla,
    info: Hsla,
    line: Hsla,
    mono: SharedString,
}

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
            sel: BTreeSet::new(),
            sel_rel: None,
            hover_rel: None,
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
                for r in model.relations.iter().filter(|r| r.from_table == t.key()) {
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
                let row = |ti: usize, col: Option<&String>| {
                    col.and_then(|c| model.tables[ti].columns.iter().position(|x| &x.name == c))
                };
                Some(Rel {
                    from: f,
                    to: t,
                    from_row: row(f, r.from_columns.first()),
                    to_row: row(t, r.to_columns.first()),
                    model_ix: mi,
                })
            })
            .collect();
        self.model = model;
        self.sel.clear();
        self.sel_rel = None;
        self.hover_rel = None;
        self.drag = Drag::None;
        self.hidden.clear();
        self.stash = None;
        self.pending_fit = true;
        if let Some(l) = &layout {
            self.apply_saved(l);
        }
        self.recompute_vis();
        if !matches!(self.scope, Scope::All) {
            self.stash_whole();
            self.unplace_visible();
        }
        self.run_layout(true, cx);
        cx.notify();
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

    /// Number of tables in the model (not only the visible ones).
    pub fn table_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
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

    /// Foreign keys between visible tables.
    pub fn visible_relation_count(&self) -> usize {
        self.rel_vis.iter().filter(|v| **v).count()
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
            .map(|r| self.vis[r.from] && self.vis[r.to])
            .collect();
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

    fn row_y(&self, ti: usize, row: Option<usize>) -> f32 {
        let n = &self.nodes[ti];
        match row {
            Some(r) => n.y + HEADER_H + r as f32 * ROW_H + ROW_H / 2.0,
            None => n.y + HEADER_H / 2.0,
        }
    }

    fn route_of(&self, ri: usize) -> Route {
        let r = &self.rels[ri];
        route::route(
            self.rect(r.from),
            self.row_y(r.from, r.from_row),
            self.rect(r.to),
            self.row_y(r.to, r.to_row),
            ri,
        )
    }

    fn hit_table(&self, w: (f32, f32)) -> Option<usize> {
        self.order.iter().rev().copied().find(|&i| {
            let n = &self.nodes[i];
            self.vis[i] && w.0 >= n.x && w.0 <= n.x + n.w && w.1 >= n.y && w.1 <= n.y + n.h
        })
    }

    /// Lines have an invisible hit area about 16 screen pixels wide.
    fn hit_rel(&self, w: (f32, f32)) -> Option<usize> {
        let tol = 8.0 / self.zoom;
        (0..self.rels.len())
            .filter(|&i| self.rel_vis[i])
            .map(|i| (i, route::distance(&self.route_of(i), w)))
            .filter(|(_, d)| *d <= tol)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    fn emit_selection(&self, cx: &mut Context<Self>) {
        cx.emit(ErDiagramEvent::SelectionChanged(self.selection()));
    }

    // ---- input ----------------------------------------------------------

    fn on_down(&mut self, ev: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let p = self.local(ev.position);
        let w = self.to_world(p);
        let shift = ev.modifiers.shift;
        if let Some(t) = self.hit_table(w) {
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
        } else if let Some(r) = self.hit_rel(w) {
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
            Drag::None => {
                let w = self.to_world(p);
                let h = if self.hit_table(w).is_some() {
                    None
                } else {
                    self.hit_rel(w)
                };
                if h != self.hover_rel {
                    self.hover_rel = h;
                    cx.notify();
                }
            }
            _ => {}
        }
        self.drag = if dragging { d } else { Drag::None };
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

    // ---- painting -------------------------------------------------------

    fn ensure_text(&mut self, i: usize, fpx: f32, pal: &Pal, window: &mut Window) {
        let key = (fpx * 2.0) as u32;
        let sig = (pal.fg, pal.muted);
        if let Some(t) = &self.nodes[i].text
            && t.px == key
            && t.sig == sig
        {
            return;
        }
        let font = window.text_style().font();
        let mut bold = font.clone();
        bold.weight = FontWeight::SEMIBOLD;
        let mut mono = font;
        mono.family = pal.mono.clone();
        let ts = window.text_system().clone();
        let shape = |s: &str, f: &Font, size: f32, color: Hsla| {
            let run = TextRun {
                len: s.len(),
                font: f.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            ts.shape_line(SharedString::from(s.to_string()), px(size), &[run], None)
        };
        let t = &self.model.tables[i];
        let count = self
            .counts
            .get(&self.nodes[i].key)
            .map(|c| shape(&fmt_count(*c), &mono, fpx * 0.88, pal.muted));
        let cache = TextCache {
            px: key,
            sig,
            header: shape(&t.name, &bold, fpx, pal.fg),
            count,
            names: t
                .columns
                .iter()
                .map(|c| shape(&c.name, &mono, fpx * 0.96, pal.fg))
                .collect(),
            types: t
                .columns
                .iter()
                .map(|c| {
                    let s = if c.nullable {
                        format!("{}?", c.type_name)
                    } else {
                        c.type_name.clone()
                    };
                    shape(&s, &mono, fpx * 0.85, pal.muted)
                })
                .collect(),
        };
        self.nodes[i].text = Some(cache);
    }

    fn ensure_shared(&mut self, fpx: f32, pal: &Pal, window: &mut Window) {
        let key = (fpx * 2.0) as u32;
        let sig = (pal.key, pal.info, pal.primary, self.centre_label.clone());
        if let Some(s) = &self.shared
            && s.px == key
            && s.sig == sig
        {
            return;
        }
        let font = window.text_style().font();
        let mut mono = font.clone();
        mono.family = pal.mono.clone();
        let mut bold = font;
        bold.weight = FontWeight::SEMIBOLD;
        let ts = window.text_system().clone();
        let shape = |s: &str, f: &Font, size: f32, color: Hsla| {
            let run = TextRun {
                len: s.len(),
                font: f.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            ts.shape_line(SharedString::from(s.to_string()), px(size), &[run], None)
        };
        self.shared = Some(SharedText {
            px: key,
            sig: sig.clone(),
            pk: shape("PK", &bold, fpx * 0.72, pal.key),
            fk: shape("FK", &bold, fpx * 0.72, pal.info),
            centre: shape(&sig.3, &bold, fpx * 0.83, pal.primary),
        });
    }

    fn paint_scene(&mut self, b: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if self.nodes.is_empty() || self.layouting {
            return;
        }
        let (vw, vh) = (b.size.width.as_f32(), b.size.height.as_f32());
        if self.pending_fit && vw > 1.0 {
            let old = self.zoom;
            self.do_fit(vw, vh);
            self.pending_fit = false;
            self.zoom_event(old, cx);
        }
        let pal = {
            let t = cx.theme();
            Pal {
                card: t.popover,
                header: t.muted,
                border: t.border,
                fg: t.foreground,
                muted: t.muted_foreground,
                primary: t.primary,
                key: t.warning,
                info: t.info,
                line: t.muted_foreground,
                mono: t.mono_font_family.clone(),
            }
        };
        let z = self.zoom;
        let (ox, oy) = (
            b.origin.x.as_f32() + self.pan.0,
            b.origin.y.as_f32() + self.pan.1,
        );
        let (bx, by) = (b.origin.x.as_f32(), b.origin.y.as_f32());
        let f = Frame {
            z,
            ox,
            oy,
            bx,
            by,
            vw,
            vh,
        };
        let rc =
            |x: f32, y: f32, w: f32, h: f32| Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
        let mut neighbours: BTreeSet<usize> = BTreeSet::new();
        for (ri, r) in self.rels.iter().enumerate() {
            if self.rel_vis[ri] && self.rel_highlighted(ri) {
                neighbours.insert(r.from);
                neighbours.insert(r.to);
            }
        }
        // highlighted rows: (table, row)
        let mut rows: Vec<(usize, usize)> = vec![];
        for ri in [self.sel_rel, self.hover_rel].into_iter().flatten() {
            let r = &self.rels[ri];
            if let Some(x) = r.from_row {
                rows.push((r.from, x));
            }
            if let Some(x) = r.to_row {
                rows.push((r.to, x));
            }
        }

        let fpx = (BASE_FONT * z).round().max(5.0);
        let lh = fpx * 1.25;
        if z >= 0.3 {
            self.ensure_shared(fpx, &pal, window);
        }
        let sc = Scene {
            f,
            pal: &pal,
            fpx,
            lh,
            neighbours: &neighbours,
            rows: &rows,
        };

        window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
            self.paint_dots(f, &pal, window);
            self.paint_relations(&sc, window);
            self.paint_cards(&sc, window, cx);

            // rubber band
            if let Drag::Rubber { start, cur, .. } = &self.drag {
                let (x0, x1) = (start.0.min(cur.0), start.0.max(cur.0));
                let (y0, y1) = (start.1.min(cur.1), start.1.max(cur.1));
                window.paint_quad(quad(
                    rc(bx + x0, by + y0, x1 - x0, y1 - y0),
                    px(0.0),
                    pal.primary.opacity(0.12),
                    px(1.0),
                    pal.primary,
                    BorderStyle::Solid,
                ));
            }
        });
    }

    /// Is relation `ri` selected, hovered or attached to a selected table?
    fn rel_highlighted(&self, ri: usize) -> bool {
        let r = &self.rels[ri];
        self.sel_rel == Some(ri)
            || self.hover_rel == Some(ri)
            || self.sel.contains(&r.from)
            || self.sel.contains(&r.to)
    }

    /// Dotted canvas background: a fixed 20 px grid that follows the pan.
    fn paint_dots(&self, f: Frame, pal: &Pal, window: &mut Window) {
        let Frame { bx, by, vw, vh, .. } = f;
        let rc =
            |x: f32, y: f32, w: f32, h: f32| Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
        let dot = 2.0f32;
        let dc = pal.fg.opacity(0.22);
        let gx = (self.pan.0).rem_euclid(DOT_GAP);
        let gy = (self.pan.1).rem_euclid(DOT_GAP);
        let mut y = gy;
        while y < vh {
            let mut x = gx;
            while x < vw {
                window.paint_quad(fill(
                    rc(bx + x - dot / 2.0, by + y - dot / 2.0, dot, dot),
                    dc,
                ));
                x += DOT_GAP;
            }
            y += DOT_GAP;
        }
    }

    /// Relation lines (highlighted ones on top) and their end marks.
    fn paint_relations(&self, sc: &Scene, window: &mut Window) {
        let Frame {
            z,
            ox,
            oy,
            bx,
            by,
            vw,
            vh,
        } = sc.f;
        let pal = sc.pal;
        let vis = |x: f32, y: f32, w: f32, h: f32| {
            x + w >= bx && x <= bx + vw && y + h >= by && y <= by + vh
        };
        let sx = |x: f32| ox + x * z;
        let sy = |y: f32| oy + y * z;
        let pt = |x: f32, y: f32| point(px(x), px(y));
        let rc =
            |x: f32, y: f32, w: f32, h: f32| Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
        let lw = (1.5 * z).clamp(1.5, 2.4);
        let mut many_n = PathBuilder::stroke(px(lw));
        let mut many_h = PathBuilder::stroke(px(lw * 1.5));
        let (mut any_n, mut any_h) = (false, false);
        for pass in [false, true] {
            for ri in 0..self.rels.len() {
                if !self.rel_vis[ri] {
                    continue;
                }
                let hl = self.rel_highlighted(ri);
                if hl != pass {
                    continue;
                }
                let r = self.route_of(ri);
                let bb = route::bbox(&r);
                if !vis(sx(bb.0), sy(bb.1), bb.2 * z + 2.0, bb.3 * z + 2.0) {
                    continue;
                }
                let (col, w) = if hl {
                    (pal.primary, lw * 1.5)
                } else {
                    (pal.line, lw)
                };
                for s in r.pts.windows(2) {
                    let (a, c) = (s[0], s[1]);
                    let (x0, x1, y0, y1) = (a.0.min(c.0), a.0.max(c.0), a.1.min(c.1), a.1.max(c.1));
                    window.paint_quad(fill(
                        rc(
                            sx(x0) - w / 2.0,
                            sy(y0) - w / 2.0,
                            (x1 - x0) * z + w,
                            (y1 - y0) * z + w,
                        ),
                        col,
                    ));
                }
                if z >= 0.35 {
                    // many end: crow's foot at pts[0]; one end: bar at pts[3]
                    let (xa, ya) = r.pts[0];
                    let d = r.dir_from;
                    let pb = if hl { &mut many_h } else { &mut many_n };
                    for dy in [-7.0f32, 7.0] {
                        pb.move_to(pt(sx(xa + d * 11.0), sy(ya)));
                        pb.line_to(pt(sx(xa), sy(ya + dy)));
                    }
                    if hl {
                        any_h = true
                    } else {
                        any_n = true
                    }
                    let (xb, yb) = r.pts[3];
                    let bx2 = xb + r.dir_to * 8.0;
                    window.paint_quad(fill(rc(sx(bx2) - w / 2.0, sy(yb - 6.0), w, 12.0 * z), col));
                }
            }
            let (pb, any, col) = if pass {
                (
                    std::mem::replace(&mut many_h, PathBuilder::stroke(px(1.0))),
                    any_h,
                    pal.primary,
                )
            } else {
                (
                    std::mem::replace(&mut many_n, PathBuilder::stroke(px(1.0))),
                    any_n,
                    pal.line,
                )
            };
            if any && let Ok(p) = pb.build() {
                window.paint_path(p, col);
            }
        }
    }

    /// Cards: frame, header and, when zoomed in enough, the column rows.
    fn paint_cards(&mut self, sc: &Scene, window: &mut Window, cx: &mut Context<Self>) {
        let Frame {
            z,
            ox,
            oy,
            bx,
            by,
            vw,
            vh,
        } = sc.f;
        let (pal, fpx, neighbours) = (sc.pal, sc.fpx, sc.neighbours);
        let vis = |x: f32, y: f32, w: f32, h: f32| {
            x + w >= bx && x <= bx + vw && y + h >= by && y <= by + vh
        };
        let sx = |x: f32| ox + x * z;
        let sy = |y: f32| oy + y * z;
        let rc =
            |x: f32, y: f32, w: f32, h: f32| Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
        let order = self.order.clone();
        for i in order {
            if !self.vis[i] {
                continue;
            }
            let (nx, ny, nw, nh) = {
                let n = &self.nodes[i];
                (sx(n.x), sy(n.y), n.w * z, n.h * z)
            };
            if !vis(nx, ny, nw, nh) {
                continue;
            }
            let selected = self.sel.contains(&i);
            let is_centre = self.centre == Some(i);
            let (bc, bw) = if selected || is_centre {
                (pal.primary, 2.0)
            } else if neighbours.contains(&i) && !self.sel.is_empty() {
                (pal.primary.opacity(0.6), 1.5)
            } else {
                (pal.border, 1.0)
            };
            let rad = 8.0 * z;
            window.paint_quad(quad(
                rc(nx, ny, nw, nh),
                px(rad),
                pal.card,
                px(bw),
                bc,
                BorderStyle::Solid,
            ));
            let hh = HEADER_H * z;
            let cb = CardBox { nx, ny, nw, nh, hh };
            window.paint_quad(quad(
                rc(nx + 1.0, ny + 1.0, nw - 2.0, hh - 1.0),
                Corners {
                    top_left: px(rad),
                    top_right: px(rad),
                    bottom_right: px(0.0),
                    bottom_left: px(0.0),
                },
                pal.header,
                px(0.0),
                transparent_black(),
                BorderStyle::Solid,
            ));
            window.paint_quad(fill(rc(nx + 1.0, ny + hh, nw - 2.0, 1.0), pal.border));
            if z < 0.3 {
                continue;
            }
            self.ensure_text(i, fpx, pal, window);
            self.paint_card_header(i, cb, is_centre, sc, window, cx);
            if z < 0.4 {
                continue;
            }
            self.paint_card_rows(i, cb, sc, window, cx);
        }
    }

    /// Table icon, name, centre tag and row count of one card.
    fn paint_card_header(
        &self,
        i: usize,
        cb: CardBox,
        is_centre: bool,
        sc: &Scene,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let CardBox { nx, ny, nw, nh, hh } = cb;
        let (z, pal, lh) = (sc.f.z, sc.pal, sc.lh);
        let pt = |x: f32, y: f32| point(px(x), px(y));
        let rc =
            |x: f32, y: f32, w: f32, h: f32| Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
        let t = self.nodes[i].text.as_ref().unwrap();
        let sh = self.shared.as_ref().unwrap();
        window.with_content_mask(
            Some(ContentMask {
                bounds: rc(nx, ny, nw, nh),
            }),
            |window| {
                // table icon: a small grid
                let (ix, iw, ih) = (nx + 10.0 * z, 13.0 * z, 11.0 * z);
                let iy = ny + (hh - ih) / 2.0;
                window.paint_quad(quad(
                    rc(ix, iy, iw, ih),
                    px(2.0 * z),
                    transparent_black(),
                    px(1.2),
                    pal.muted,
                    BorderStyle::Solid,
                ));
                window.paint_quad(fill(rc(ix, iy + ih / 3.0, iw, 1.0), pal.muted));
                window.paint_quad(fill(
                    rc(ix + iw / 2.0, iy + ih / 3.0, 1.0, ih * 2.0 / 3.0),
                    pal.muted,
                ));
                let _ = t.header.paint(
                    pt(nx + 30.0 * z, ny + (hh - lh) / 2.0),
                    px(lh),
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
                // right side: centre tag, then the row count
                let mut right = nx + nw - 10.0 * z;
                if is_centre {
                    let (tw, th) = (sh.centre.width.as_f32() + 10.0 * z, 16.0 * z);
                    let tx = right - tw;
                    window.paint_quad(quad(
                        rc(tx, ny + (hh - th) / 2.0, tw, th),
                        px(3.0 * z),
                        pal.primary.opacity(0.15),
                        px(0.0),
                        transparent_black(),
                        BorderStyle::Solid,
                    ));
                    let _ = sh.centre.paint(
                        pt(tx + 5.0 * z, ny + (hh - lh) / 2.0),
                        px(lh),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                    right = tx - 8.0 * z;
                }
                if let Some(c) = &t.count {
                    let _ = c.paint(
                        pt(right - c.width.as_f32(), ny + (hh - lh) / 2.0),
                        px(lh),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
            },
        );
    }

    /// Column rows of one card (tag, name and type), only those in view.
    fn paint_card_rows(
        &self,
        i: usize,
        cb: CardBox,
        sc: &Scene,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let CardBox { nx, ny, nw, hh, .. } = cb;
        let (z, pal, lh, rows) = (sc.f.z, sc.pal, sc.lh, sc.rows);
        let (by, vh) = (sc.f.by, sc.f.vh);
        let pt = |x: f32, y: f32| point(px(x), px(y));
        let ncols = self.model.tables[i].columns.len();
        let rc =
            |x: f32, y: f32, w: f32, h: f32| Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
        for r in 0..ncols {
            let ry = ny + hh + r as f32 * ROW_H * z;
            let rh = ROW_H * z;
            if ry + rh < by || ry > by + vh {
                continue;
            }
            if rows.contains(&(i, r)) {
                window.paint_quad(fill(
                    rc(nx + 2.0, ry, nw - 4.0, rh),
                    pal.primary.opacity(0.22),
                ));
            }
            let col = &self.model.tables[i].columns[r];
            let tag = if col.primary_key {
                Some((pal.key, &self.shared.as_ref().unwrap().pk))
            } else if self.nodes[i].fk[r] {
                Some((pal.info, &self.shared.as_ref().unwrap().fk))
            } else {
                None
            };
            let t = self.nodes[i].text.as_ref().unwrap();
            window.with_content_mask(
                Some(ContentMask {
                    bounds: rc(nx, ry, nw, rh),
                }),
                |window| {
                    let ty = ry + (rh - lh) / 2.0;
                    if let Some((c, line)) = tag {
                        let (tw, th) = (20.0 * z, 14.0 * z);
                        let (tx, tyy) = (nx + 10.0 * z, ry + (rh - th) / 2.0);
                        window.paint_quad(quad(
                            rc(tx, tyy, tw, th),
                            px(3.0 * z),
                            c.opacity(0.15),
                            px(0.0),
                            transparent_black(),
                            BorderStyle::Solid,
                        ));
                        let _ = line.paint(
                            pt(tx + (tw - line.width.as_f32()) / 2.0, ty),
                            px(lh),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                    let _ = t.names[r].paint(
                        pt(nx + 37.0 * z, ty),
                        px(lh),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                    let tw = t.types[r].width.as_f32();
                    let _ = t.types[r].paint(
                        pt(nx + nw - 10.0 * z - tw, ty),
                        px(lh),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                },
            );
        }
    }
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
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_down))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_up))
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
        root
    }
}

#[cfg(test)]
mod tests {
    use super::fmt_count;

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

/// Everything the paint helpers share for one frame.
struct Scene<'a> {
    f: Frame,
    pal: &'a Pal,
    fpx: f32,
    lh: f32,
    neighbours: &'a BTreeSet<usize>,
    /// Highlighted rows: (table, row).
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
