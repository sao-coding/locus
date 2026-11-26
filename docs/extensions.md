# Extension points

Where a new feature can plug into the shell, for example an MCP server / AI panel (not
implemented yet; the Git panel is the worked example, see below). Paths are from the repository
root.

| Hook | Where | How a feature uses it |
|---|---|---|
| Rail slot + right panel | `crates/app/src/panels/mod.rs` (`PANELS`, `Locus::panel_content`), `RightPanel` in `crates/locus-core/src/spaces.rs` | Add a `RightPanel` variant (saved by name in `spaces.json`; a name this build lacks loads as `Unknown`), a `PanelSpec` (rail button id, icon, tooltip key, title key) and an arm in `panel_content` returning header buttons and rows. The rail (`crates/app/src/shell/rail.rs`) draws the buttons from `PANELS` |
| Palette command | `crates/app/src/palette/mod.rs` (`ACTIONS`), `Shortcut` and `Locus::perform` in `crates/app/src/shell/shortcuts.rs` | Add a `Shortcut` variant, an `ACTIONS` row (label key, shortcut text or `""`) and a `perform` arm. A key binding goes into `shortcut()` in the same file; the Settings tab's Shortcuts list follows `ACTIONS` |
| Tab decoration | `Locus::render_tab_row` in `crates/app/src/shell/tab_list.rs` | The row is icon, label, then the close button; a per-tab marker (the Git status letter, `Locus::git_tab_marker`) goes between the label and `close` |
| Status-bar item | `Locus::render_status_bar` in `crates/app/src/shell/statusbar.rs` | Children after `left` (connection state); the Git item (`Locus::git_status_item`) sits there |
| Settings section | `crates/app/src/settings/page.rs` (`CATS`, `Ctl`, `DEFS`), `Settings` in `crates/locus-core/src/settings.rs` | A field with `#[serde(default)]` in `Settings` (old `settings.json` keeps loading), a `Ctl` variant and `DEFS` row (category, label / description keys); add a `Cat` for a new category |
| Space folder | `Space::folder` (`crates/app/src/shell/mod.rs`), `SpaceData::folder` (`crates/locus-core/src/spaces.rs`), `bind_folder` / `set_folder` / `save_tab` in `crates/app/src/shell/files.rs` | The Git panel reads the bound folder (`Locus::sync_git`, called from `set_folder`, `switch_space` and start-up) and refreshes after a save in `save_tab` |
| Strings | `crates/app/locales/en.yml`, `crates/app/locales/zh-TW.yml` | One top-level section per feature (`git:` is one); every locale file (en, zh-TW, zh-CN) gets every key |

## Git panel

Implemented (issue #26). It uses every hook above:

- **Engine**: `crates/git` (`locus-git`, no GPUI). It runs the user's own `git` as a process, so
  their credentials, SSH, Git Credential Manager, hooks and config apply. `exec.rs` builds the
  command line (arguments are a vector, never a shell string; `-c core.quotepath=false`,
  `GIT_TERMINAL_PROMPT=0`, `CREATE_NO_WINDOW`, a timeout, a `Cancel` handle). The pure parts
  have unit tests: `status.rs` (`status --porcelain=v2 -z --branch`), `log.rs`, `diff.rs`,
  `branch.rs`, `root.rs` (find the repo above a folder), `secrets.rs` (credential scan of the
  added lines of the staged diff) and the command-line builders in `ops.rs`. `tests/e2e.rs`
  runs the real git against throwaway repositories (a bare repo is the remote).
- **App**: `crates/app/src/git/`: `mod.rs` is the model (`GitPanel`, an entity next to the
  shell; every git call runs on `Locus::pool.rt()` with `spawn_blocking`), `panel.rs` the right
  panel, `diffpage.rs` the read-only diff tab (`TabPage::Diff`, its spec is JSON in the tab's
  `sql`), `glue.rs` the shell side (`sync_git`, tab letters, status-bar item, palette commands).
- **Refresh**: after a folder change, a save, a command of the panel, when the window is
  activated, and on a debounced (500 ms) `notify` watch of the work tree. No polling; the
  optional auto-fetch (Settings › Git) is a timer that fetches every N minutes.
- **Settings › Git** (`Settings::git`): executable path, pull mode (ff-only / rebase / merge),
  auto-fetch interval, credential scan on / off.
- **Not in v1**: stash, merge / rebase UI, conflict resolution editor, blame, several repositories
  per Space, a credentials UI.

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
