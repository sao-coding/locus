# Locus docs

Database client (`locus.exe`): Rust + GPUI Fast UI, in-process MySQL / MariaDB / PostgreSQL /
SQLite / Redis drivers, a Go sidecar for Oracle. This is a map of the source code.

## Repository layout

| Path | What it is |
|---|---|
| `crates/app` | The `locus` binary: GPUI views only, one module per feature (table below). `locales/` (every user-visible string), `assets/` (icon, resource file), `build.rs` |
| `crates/locus-core` | Everything without GPUI: saved files, credentials, driver dispatch and pool, Production guard / audit, grid filter / copy / export, plan model, palette matching. `cargo test -p locus-core` runs without a window |
| `crates/db-core` | Driver interface (query / stream / cancel / catalog), columnar results, SQL classifier and formatter |
| `crates/db-mysql`, `crates/db-postgres`, `crates/db-sqlite`, `crates/db-sidecar-client` | Drivers: MySQL / MariaDB, PostgreSQL, SQLite (bundled, file path), the Go sidecar client |
| `crates/db-redis` | Redis driver (redis-rs, tokio): raw commands as reply trees, paged SCAN / HSCAN / LRANGE / SSCAN / ZRANGE / XRANGE readers. Not a `db_core::Driver` |
| `crates/er-diagram` | Relationship-diagram view (GPUI) and its model |
| `sidecar/` | Go sidecar (`PROTOCOL.md`) |
| `docs/` | This file, `extensions.md` |

## crates/app/src

| Module | Files | What it does |
|---|---|---|
| `main.rs` | | Entry point, language choice, tokio runtime (2 workers), window, bench-mode wiring |
| `shell/` | `mod.rs` | `Locus` and `Space` state, construction / restore, root `Render` (title bar, sidebar, main card). Re-exports its imports so feature modules extend `Locus` with `use crate::shell::*;` |
| | `layout.rs`, `dnd.rs`, `tabs.rs`, `tab_list.rs` | Split layout and panes, tab drag and drop, tab operations, tab rows |
| | `titlebar.rs`, `header.rs`, `sidebar.rs`, `statusbar.rs`, `rail.rs` | Title bar, card header, sidebar, status bar, right rail |
| | `space_ops.rs`, `space_popover.rs`, `persist.rs` | Spaces (switch / create / delete / rename, popover), `spaces.json` save |
| | `pages.rs`, `welcome.rs`, `files.rs`, `shortcuts.rs` | Page tabs (History / Settings), welcome card, .sql files and the Space folder, keystrokes and actions |
| | `style.rs`, `motion.rs`, `theme.rs`, `icons.rs` | Small widgets and window gradient, sidebar motion, design tokens, app icons |
| `panels/` | `mod.rs` | Right-panel registry (`PANELS`): the slot for side panels, see `docs/extensions.md` |
| `saved/` | `mod.rs`, `page.rs`, `ops.rs` | Saved queries and snippets: shared store entity, the Saved tab, saving the editor text, palette `*` entries |
| `palette/` | `mod.rs` | Command palette (`ACTIONS`, scopes, fuzzy match from `locus_core::palette_scope`) |
| `connections/` | `form.rs`, `list.rs`, `meta.rs`, `objects.rs`, `ops.rs` | Connection dialog, sidebar connection rows, server version / round trip, database-object tree, Space connection operations |
| `query/` | `tab.rs`, `editor.rs`, `completion.rs`, `run.rs`, `params.rs`, `tools.rs`, `status.rs` | Query tab entity, SQL editor with the large-file limit, autocomplete, run / cancel, Format / Find / Explain, status bar entity |
| `query/` | `tab.rs`, `editor.rs`, `completion.rs`, `run.rs`, `export.rs`, `tools.rs`, `status.rs` | Query tab entity, SQL editor with the large-file limit, autocomplete, run / cancel, export dialog and jobs, Format / Find / Explain, status bar entity |
| `results/` | `grid.rs`, `ui.rs` | Result grid (sort, copy, staged edits, value viewer), result tabs, filter bar, pager, plan view |
| `diagram/` | `tab.rs`, `ops.rs`, `ui.rs` | Diagram tab state, opening / JOIN insertion / DDL tabs, diagram card header and footer |
| `history/` | `mod.rs`, `page.rs` | Shared history store entity, the History tab |
| `settings/` | `page.rs`, `apply.rs` | The Settings tab, applying saved settings and the language switch |
| `import/` | `mod.rs` | Import wizard dialog (file, columns, options, run) |
| `activity/` | `mod.rs`, `ops.rs` | Server activity page tab: sessions (auto-refresh, kill), variables, status |
| `redis/` | `mod.rs`, `browser.rs`, `key.rs`, `console.rs`, `info.rs`, `ops.rs` | Redis page tabs: key browser, key tab per type, console, server info; Production lock through `Ctx::locked` |
| `routines/` | `mod.rs` | Source tabs of stored code: read source, Spec / Body, Compile / Save through the Production gate, compile errors |
| `structure/` | `mod.rs`, `ui.rs` | Structure tabs: read-only columns, indexes, foreign keys, triggers, checks, DDL of a table |
| `prod/` | `mod.rs` | Production protection UI: unlock state, the gate every SQL-sending path goes through, dialogs, chip and bar |
| `dev/` | `bench.rs` | Bench mode for the local regression harness (`locus.exe <scene> [fixtures-dir]`) |

## crates/locus-core/src

| File | What it does |
|---|---|
| `store.rs`, `creds.rs`, `settings.rs`, `spaces.rs` | `connections.json` (no password), Windows Credential Manager, `settings.json`, `spaces.json`; `LOCUS_DATA_DIR` relocates all files (UI tests) |
| `history.rs` | History entries, cap per connection, `history.json` load / save, filtering and time formatting |
| `redis.rs` | Redis config from a saved connection, command classification (read / write / dangerous), console line splitter, key namespace tree, reply and value formatting, tab specs |
| `driver.rs`, `pool.rs` | Driver dispatch per connection kind, shared lazy connections |
| `guard.rs`, `audit.rs` | Production guard decisions, `audit.log` |
| `apply.rs` | Apply staged grid edits in one transaction |
| `import/` | Import wizard logic: streamed CSV / sheet reading, encoding sniffing, conversion, type inference, per-engine INSERT / CREATE SQL, the batched run |
| `rowfilter.rs`, `copyfmt.rs`, `export.rs`, `export_xlsx.rs`, `export_stream.rs`, `fmt.rs` | Result filter language, copy formats, export writers (CSV / TSV / JSON / JSON Lines / xlsx / SQL / Markdown / HTML), server streaming export, number / duration formatting |
| `selstats.rs`, `fknav.rs` | Selection statistics (exact decimals), foreign-key lookup SQL |
| `plan.rs` | EXPLAIN plan model per engine |
| `activity.rs` | Server activity: SQL per engine, session rows, filter / sort, kill statements |
| `params.rs`, `saved.rs`, `joinsug.rs` | Query placeholders and binding, saved-query / snippet files, FK JOIN suggestions |
| `palette_scope.rs`, `sqlrefs.rs`, `status.rs` | Palette scopes and fuzzy match, table references of a statement, run phase and error titles |
