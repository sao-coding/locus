---
default: minor
---

# Relationship diagrams show a detail panel for the selected table (columns, keys, defaults, indexes, row count; open data, structure, `SELECT *` into the editor, copy name). The JOIN path button finds the shortest chain of foreign keys between the Shift+clicked tables and inserts the `SELECT … JOIN …`, with a chooser when several are as short. Columns like `customer_id` that match a table but have no foreign key are drawn as dashed "inferred" relations (can be turned off), foreign keys without an index get a warning and a footer count, card headers can be tinted by schema or name prefix with a legend, and the Export menu saves the diagram (or the selection) as PNG or SVG in the current theme.
