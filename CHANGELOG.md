# Changelog
## 0.3.1 (2026-11-27)

### Fixes

#### Relationship diagram: long column names no longer overlap their type

When a column's name and type do not both fit the card, the name is cut with an ellipsis (never below half the row) and then the type, instead of painting over each other. The SVG and PNG export does the same.

## 0.3.0 (2026-11-27)

### Features

- Relationship diagrams show a detail panel for the selected table (columns, keys, defaults, indexes, row count; open data, structure, `SELECT *` into the editor, copy name). The JOIN path button finds the shortest chain of foreign keys between the Shift+clicked tables and inserts the `SELECT … JOIN …`, with a chooser when several are as short. Columns like `customer_id` that match a table but have no foreign key are drawn as dashed "inferred" relations (can be turned off), foreign keys without an index get a warning and a footer count, card headers can be tinted by schema or name prefix with a legend, and the Export menu saves the diagram (or the selection) as PNG or SVG in the current theme.
- Relationship diagrams draw foreign keys as curved lines with crow's-foot ends (optional, mandatory, one-to-one). Lines are curved by default; the header View menu switches between curved and smooth (rounded orthogonal) lines per diagram, next to the card detail; lines entering the same table get their own vertical. Hovering a table highlights its relations and neighbours and dims the rest; hovering a line highlights its key columns and shows `orders.customer_id → customers.id` with the constraint name. A minimap (bottom right, can be turned off) pans the view, Ctrl+F finds a table and flies to it, cards can show all columns, keys only or headers only (also per card with the chevron), and Auto arrange produces fewer crossings.
- Parameters follow DBeaver: Settings › Queries has switches for `:name`, `@name`, `$name` and `?` (and a master switch); on MySQL, SQL Server, PostgreSQL and Oracle an `@name` that the script itself sets, an operator or a SQL*Plus `@file` is left alone. New client-side variables: `@set name = value`, `@unset`, `@echo` and `${name}` text substitution (the Production guard and audit see the substituted statement), an undefined `${name}` asks in the Run dialog, "Show variables" in the palette lists, edits and deletes them, and `${` completes variable names. See docs/parameters.md.

### Fixes

- Space menu shows each Space's own icon (in its colour) with the check mark in a fixed column on the right; snippets now have tab stops: after accepting one the first stop is selected, Tab / Shift+Tab move between stops, `$0` is the last caret position and Esc leaves snippet mode.

## 0.2.1 (2026-11-26)

### Fixes

- Result grid: cell text is vertically centred in its row, and the in-place cell editor fills exactly the cell and keeps the text where it was (no jump when editing starts).
- The Space icon chooser has bigger icons (18 px in 36 px cells, 17 icons) with a clear selection ring and a name tooltip. The schema tree uses icons that match what each node is: server, schema, table, view, index, sequence, function, procedure, trigger, type and extension.

#### Query header and table structure layout

The query header keeps the connection and database readable in a narrow window: the tab title gives way first, the unsaved marker is a dot with a tooltip, and the header text is larger. The table structure tables now line up with the section tabs above them and sit in a framed box.

#### PostgreSQL procedure and function source

Opening the source or parameters of a PostgreSQL procedure or function whose arguments have names or modes (`IN p_id integer`) no longer fails with `syntax error at or near "IN"`.

## 0.2.0 (2026-11-25)

### Features

- Releases now include an installer, `locus-<version>-windows-x64-setup.exe`: per-user install with a Start menu entry and an uninstaller, next to the portable exe.
- Result tabs of a multi-statement run are named after the table a SELECT reads (schema dropped, duplicates numbered, full statement in the tooltip). The result grid gets an off-white body in the light theme and optional subtle zebra striping (Settings → Appearance).
- Redesigned Space colour picker: one to three colours on a soft hue / saturation pad (two or three tint the window as a gradient), nine preset swatches, an intensity slider, and a per-Space light / dark / follow-the-app choice. Spaces saved before keep their colour.
- SQL Server connections, through the built-in sidecar on Microsoft's go-mssqldb: scripts with `GO` batches, streamed result sets, bound parameters, transactions, and a schema browser with tables, views, columns, primary and foreign keys. Cancel and timeout stop only the running statement on the server and keep the connection and an open transaction; when the server ends the transaction itself (`XACT_ABORT ON`, deadlock) the tab says so. Object kinds, DDL, routines, the structure editor and the activity view are not there yet for SQL Server.
- Settings › About has a "Receive test versions (beta)" switch: on, release candidates are offered as well; off, only stable releases. It is on by default in a test version and off in a stable one. On/off settings are now switches, and the About entry in Settings has a new icon.
- Locus now checks GitHub for a new release about once a day and shows it in the status bar; Settings › About has the details, a "Check now" button and an off switch.
- Locus can update itself: a new version is downloaded in the background, its minisign signature checked, and installed when you pick "Restart and update" (installer copies run the new setup silently, portable copies swap the exe). Scoop copies are still updated with `scoop update`.

### Fixes

- SQL editor autocomplete: Tab accepts the highlighted suggestion (Tab still indents when no popup is open), and accepting replaces the word being typed (including the rest of the identifier and the quotes of a quoted name) instead of inserting the whole item after it.
- Release downloads now come with minisign signatures (`*.minisig`); the public key is `minisign.pub` in the repository.
- The Oracle helper (sidecar) now reports its protocol and build first; Locus refuses one from another build before it connects, and skips an outdated `sidecar.exe` next to `locus.exe` in favour of the built-in copy.
- Clicks and drags on the Space popover no longer reach what is under it (the sidebar edge, tabs).
- The "+n" result overflow button now matches the tab height and size, and its menu uses the normal row size with the row count right-aligned.

## 0.1.2 (2026-11-24)

### Fixes

- completion after JOIN, single kill confirm, Structure tab section, cancelled import cleanup (#31)
- Completion now opens right after the space that follows `JOIN`, `FROM`, `UPDATE`, `INTO` or `ON`, listing tables (or columns after `ON`) before keywords. (#11)
- Cancelling an import into a new table now drops the table it created when it is still empty. (#14)
- Killing a query from the activity page on a Production connection asks for confirmation once instead of twice. (#12)
- Reopening a Structure tab returns to the section last used for that table. (#13)

## 0.1.1 (2026-11-23)

### Fixes

- Fix the `dev` feature build

#### Column header filter and order menu

Each result column header has a dropdown with Order by ASC / DESC, filters from the selected cell, a typed value or the clipboard (=, <>, >, <, LIKE, ILIKE), IS NULL / IS NOT NULL, and a raw WHERE clause. Filters re-run the query on the server, show as removable chips above the grid, and can be saved per query.
