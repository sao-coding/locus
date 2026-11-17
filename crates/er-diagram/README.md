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
