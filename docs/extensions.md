# Extension points

Where a new feature can plug into the shell, for example a Git panel or an AI panel (neither
exists yet; the MCP server does, see below). Paths are from the repository root.

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

The **MCP server** exists ([mcp.md](mcp.md) for users). Where it sits:

- **Server**: `crates/mcp` (`locus-mcp`, no GPUI) on `locus-core`: `access` (Off /
  Read-only / Read-write per connection, then `locus_core::guard::decide` with the
  connection's `env`), `guard` (Host / Origin / bearer token / body size), `format`
  (compact TSV), `exec` (statements on a pinned session, 30 s timeout, read-only
  transaction), `tools` and `server` (rmcp Streamable HTTP, stateless, on 127.0.0.1).
  Every statement goes to `locus_core::audit` with `source: mcp`. It opens its own
  `AnyDriver` per connection, never the app's `Pool` sessions.
- **App side**: `crates/app/src/mcp/`: the `Mcp` entity (start / stop from the settings
  `mcp_enabled` / `mcp_port`, token in Credential Manager via `locus_core::creds`), the
  `Host` it gives the server (connections, passwords, confirmations, events),
  Settings › MCP (`page.rs`, category `Cat::Mcp`) and the status-bar item
  (`Locus::render_mcp_status`). The Production confirmation is
  `Locus::open_mcp_confirm` in `crates/app/src/prod/mod.rs` (same dialog parts as the
  user's confirm). History entries carry `mcp_client` and show an MCP badge.
- **Per-connection access**: `SavedConnection::mcp` (`None` = default of `env`), the MCP
  page of the connection dialog (`ConnForm::render_mcp`).
- **A new tool**: a method on `tools::Core`, a `Tool` in `server::tools()` (keep the
  description short) and an arm in `Mcp::dispatch`. SQL it sends goes through
  `Core::admit` (access, Production confirmation, audit).

An **AI panel** inside Locus would be a feature module `crates/app/src/ai/` registered as a
right panel (`PANELS`), with palette commands to open it / send the selection.
