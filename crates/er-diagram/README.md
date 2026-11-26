# er-diagram

GPUI view of a schema's tables and foreign keys. Data in, events out.

```rust
let model = DiagramModel::from_catalog(schema, &tables /* (name, Vec<ColumnInfo>) */, &fks);
let view = cx.new(|cx| ErDiagram::new(model, labels, saved_layout /* Option<DiagramLayout> */, cx));
cx.subscribe(&view, |_, _, ev: &ErDiagramEvent, cx| match ev {
    ErDiagramEvent::OpenTable(key) => { /* "schema.name" */ }
    ErDiagramEvent::SelectionChanged(sel) => {}
    ErDiagramEvent::LayoutChanged => { /* save view.read(cx).layout() as JSON */ }
}).detach();
// render: div().size_full().child(view.clone())
```

Mouse: drag card = move (selected group moves together); drag background = pan;
Shift+drag background = rubber band; Shift+click = add/remove; click relation =
select; double-click card = `OpenTable`; wheel = scroll (Shift = horizontal);
Ctrl+wheel = zoom at pointer. Toolbar: fit, 100 %, re-layout (texts from `Labels`).

`er-demo [n]`, `er-demo bench-drag|bench-pan <n>`, `er-demo live` (needs
`--features live`). Theme: `ER_THEME=dark`.

## Scope, zoom, joins (Strata v3)

- `set_scope(Scope::{All, Centre{table, depth: Depth::{Hops(n), All}}, Tables(keys)})`; pure
  `tables_in_scope(&model, &scope)` / `scoped_tables` (with hop distance). A centred scope is laid
  out in columns by hop distance (`level_layout`), the centre has a tag in its header
  (`set_centre_label`). `auto_arrange()` drops manual positions; `Scope::All` restores the old ones.
- Zoom 40–160 %: `zoom_in` / `zoom_out` / `set_zoom` / `zoom()`, Ctrl+wheel; `ZoomChanged(f32)` event
  (also after "fit", which may go below 40 % to overview big schemas).
- Click on a line: `RelationClicked(Relation)` → `join_clause(&rel)` (`join_clause_with` to quote names).
- Right click on a table: `TableContextMenu { table, position }`; `hide_table(key)`, `show_hidden_tables()`.
- `mermaid()` / `to_mermaid(&model, &keys)`; `set_row_counts(map)` for the header counts.
- Demo: toolbar for all of it; `ER_SCOPE=centre|list ER_DEPTH=1|2|all er-demo [n]`.

## Lines, highlight, navigation (issue #23)

- Lines: `EdgeStyle::Smooth` (default; orthogonal with 8-unit rounded corners) or `EdgeStyle::Bezier`
  (`set_edge_style`). Routing is pure (`route`, `edge_path`, `slot_offset`, tested in `route.rs`);
  parallel lines between the same two cards get separate verticals, self references loop on the
  right. Edge geometry is cached per line and recomputed only when one of its cards moves or
  resizes; each line is one path, so gpui-fast reuses its tessellation while panning.
  Smoothstep is the default: on dense schemas its verticals line up with the layer gaps, while
  bezier bundles overlap.
- Ends (crow's foot, `cardinality`): referenced side `||` (NOT NULL key) or `|o` (nullable);
  referencing side crow's foot, or one bar when the key is the primary key or a unique index (1:1).
  Indexes come from `set_table_details(map)` (`TableDetail`: indexes, column defaults / comments).
- Hover a card: its lines and neighbours are highlighted, the rest dims to about 35 %. Hover a line:
  its key rows are highlighted and a tooltip shows `orders.customer_id → customers.id` and the
  constraint name. The selection keeps the highlight; Esc (`escape`) clears it. Lines are hit
  within 4 screen pixels of the stroke at any zoom.
- Minimap (bottom right, `set_minimap`): cards and the viewport; click or drag on it to pan.
- Search: Ctrl+F (`open_search`, `search_for`) filters the cards, Enter flies to the next hit
  (`focus_table`: animated camera, selection, one border pulse). Animations request frames only
  while they run.
- Density (`set_density`: `Full`, `Keys` = PK / FK / indexed columns, `Header`); the chevron in a
  card header flips one card (`collapsed_cards` / `set_collapsed_cards`). `ViewOptionsChanged`
  asks the host to save these.
- Layout: Sugiyama-style (dummy nodes on long edges, barycentre sweeps plus adjacent swaps keeping
  the order with the fewest crossings, placement towards neighbours); unrelated tables in a grid
  below; `crossings(pos, sizes, edges)` measures a layout.

Demo: `ER_EDGE=bezier`, `ER_DENSITY=keys|header`, `ER_MINIMAP=0`, `ER_SEARCH=<text>`,
`ER_FOCUS=<table>`; the toolbar has the same switches.
