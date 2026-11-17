# Extension points

Where a new feature can plug into the shell, for example a Git panel or an MCP server / AI
panel (neither exists yet). Paths are from the repository root.

| Hook | Where | How a feature uses it |
|---|---|---|
| Rail slot + right panel | `crates/app/src/panels/mod.rs` (`PANELS`, `Locus::panel_content`), `RightPanel` in `crates/locus-core/src/spaces.rs` | Add a `RightPanel` variant (saved by name in `spaces.json`; a name this build lacks loads as `Unknown`), a `PanelSpec` (rail button id, icon, tooltip key, title key) and an arm in `panel_content` returning header buttons and rows. The rail (`crates/app/src/shell/rail.rs`) draws the buttons from `PANELS` |
| Palette command | `crates/app/src/palette/mod.rs` (`ACTIONS`), `Shortcut` and `Locus::perform` in `crates/app/src/shell/shortcuts.rs` | Add a `Shortcut` variant, an `ACTIONS` row (label key, shortcut text or `""`) and a `perform` arm. A key binding goes into `shortcut()` in the same file; the Settings tab's Shortcuts list follows `ACTIONS` |
| Tab decoration | `Locus::render_tab_row` in `crates/app/src/shell/tab_list.rs` | The row is icon, label, then the close button; a per-tab marker (the old Git status letter) goes between the label and `close` |
| Status-bar item | `Locus::render_status_bar` in `crates/app/src/shell/statusbar.rs` | Children after `left` (connection state); the old Git branch item sat there |
| Settings section | `crates/app/src/settings/page.rs` (`CATS`, `Ctl`, `DEFS`), `Settings` in `crates/locus-core/src/settings.rs` | A field with `#[serde(default)]` in `Settings` (old `settings.json` keeps loading), a `Ctl` variant and `DEFS` row (category, label / description keys); add a `Cat` for a new category |
| Space folder | `Space::folder` (`crates/app/src/shell/mod.rs`), `SpaceData::folder` (`crates/locus-core/src/spaces.rs`), `bind_folder` / `set_folder` / `save_tab` in `crates/app/src/shell/files.rs` | A Git panel reads the bound folder, refreshes after `set_folder` and after a save in `save_tab` |
| Strings | `crates/app/locales/en.yml`, `crates/app/locales/zh-TW.yml` | One top-level section per feature (`git:` was one); both files get every key |

## Git panel

Rail slot + right panel (file list, commit box), palette commands (toggle
panel, commit…), tab decoration (status letter per bound file), status-bar
item (branch · changes), settings section if it needs options, Space folder
as the work tree. Git calls belong off the UI thread (the tokio runtime in
`Locus::pool.rt()`); pure parts (status → rows, secret scan, diff parse)
in `crates/locus-core` so they are unit-tested without GPUI; the git
process wrapper as its own crate under `crates/`.

## MCP server / AI panel

Not implemented. Where it would go:

- **Server**: a new crate under `crates/` (no GPUI) built on
  `crates/locus-core`: `store` (saved connections, never passwords in
  output), `driver` / `pool` (queries), `history`. Every SQL it sends must go
  through the Production guard: `locus_core::guard::decide` with the
  connection's `env`, and the audit log (`locus_core::audit`). The app's
  gate for user-sent SQL is `Locus::gate` in `crates/app/src/prod/mod.rs`;
  an MCP path needs the same decision, not a bypass.
- **AI panel**: a feature module `crates/app/src/ai/` registered as a right
  panel (`PANELS`), palette commands to open it / send the selection, and a
  settings section (server on / off, allowed connections).
- **Startup**: the server would be started from `crates/app/src/main.rs`
  next to the tokio runtime, or as a separate binary of the new crate.
