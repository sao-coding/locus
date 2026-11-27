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

- Lines: `EdgeStyle::Bezier` (default; cubic curves) or `EdgeStyle::Smooth` (orthogonal with 8-unit rounded corners)
  (`set_edge_style`; the app header offers both and keeps the choice per diagram). Routing is pure
  (`route`, `edge_path`, `channel_offsets`, tested in `route.rs`);
  lines entering the same card from the same side get their own vertical (nearest source
  innermost, so they do not cross each other), self references loop on the
  right. Edge geometry is cached per line and recomputed only when one of its cards moves or
  resizes; each line is one path, so gpui-fast reuses its tessellation while panning.
  Curves are the default (lines should not read as straight segments); smoothstep keeps its
  verticals in the layer gaps and can read better on very dense schemas.
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

Demo: `ER_EDGE=smooth|bezier`, `ER_DENSITY=keys|header`, `ER_MINIMAP=0`, `ER_SEARCH=<text>`,
`ER_FOCUS=<table>`; the toolbar has the same switches.

## Insights and export (issue #23, second part)

- Detail panel: with one table selected, a panel on the right lists its columns (type, PK / FK /
  NULL tags, default, comment), indexes and row count, with buttons that send `OpenTable`,
  `OpenStructure(key)`, `InsertSelect(key)` (the host writes `SELECT *` for its dialect) and copy
  the name. × or Esc closes it until the selection changes.
- JOIN path: `join_path(quote)` on the selected tables (Shift+click) finds the shortest chain of
  declared foreign keys (`join_paths`, intermediate tables allowed), highlights it and sends
  `InsertSql(join_sql(..))`; several of the same length open a chooser. Pure and tested.
- Inferred relations: `DiagramModel::add_inferred` (the app feeds `locus_core::joinsug::infer_fks`:
  `customer_id` / `customerId` → `customer(s)` with a compatible one-column PK, no FK declared).
  Drawn dashed and grey, tooltip `Labels::inferred`; `set_show_inferred` (default on). Not used for
  JOIN paths or Mermaid, not counted as foreign keys.
- FK without index (`fk_without_index`, once `set_table_details` gave the indexes): a warning dot on
  the column with a tooltip; `unindexed_fk_count()` for a footer.
- Colour groups (`set_color_groups`, default off): headers tinted by schema when several are shown,
  else by a name prefix shared by two or more tables (`booking_*`, `sys_*`), muted theme colours,
  legend bottom left (`color_groups`, tested).
- Export: `export_svg(selection)` (vector paths and text, current theme; the selection when there is
  one) and `svg_to_png(svg, scale)` (resvg =0.45.1, system fonts loaded once). Demo:
  `ER_SELECT=a,b`, `ER_JOIN=1`, `ER_GROUPS=1`, `ER_EXPORT=svg|png`, `ER_EXPORT_DIR`.
