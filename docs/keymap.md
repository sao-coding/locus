# Key bindings

Every shortcut in Locus is a GPUI action (`locus::<Name>`) bound to keys in a
key context. You can add, change or remove bindings in `keymap.json` in the
data folder (`%APPDATA%\Locus\keymap.json`, or `LOCUS_DATA_DIR`). It is read
once at start-up, after the defaults; entries that cannot be used are skipped,
logged, and reported once in a toast.

The file has the same shape as Zed's keymap:

```json
[
  {
    "context": "Workspace",
    "bindings": {
      "f9": "locus::Run",
      "ctrl-enter": null
    }
  }
]
```

- Keys: `ctrl-`, `shift-`, `alt-` plus a key (`enter`, `escape`, `f5`,
  `pagedown`, `a`, `1`, `\`, ...); a space separates the keys of a sequence
  (`ctrl-shift-k left`).
- `null` removes the binding of that key in that context.
- A later binding at the same context depth wins over an earlier one, and a
  binding in a deeper context (closer to the focused element) wins over a
  shallower one.
- A binding in `Workspace` is also added as `Workspace > Input` and
  `Workspace > DataTable` (as the defaults are), so it also wins inside text
  inputs and the results table, which bind some of the same keys themselves.

## Key contexts

| Context | Where |
|---|---|
| `Workspace` | the whole main window (every shell command is handled here) |
| `QueryTab` | a query tab (editor, find bar, results) |
| `Editor` | the SQL editor (gpui-kit's own context there is `Input`) |
| `FindBar` | the editor's find / replace bar |
| `Grid` | a results grid (gpui-kit's table inside it is `DataTable`) |
| `CellEdit` | a grid cell being edited |
| `Palette` | the command palette (a dialog: shell commands do not run there) |

Esc runs `locus::Escape`, which goes in this order: close a rename, stop a
running query, close the find bar, cancel a cell edit, let the editor have it
(completion popup), leave a maximised pane; otherwise the focused element gets
the key.

## Actions

Generated from the action list
(`cargo test -p app --release keymap_table -- --ignored --nocapture`).

| Action | Default keys | Context |
|---|---|---|
| `locus::Run` | `ctrl-enter` | `Workspace` |
| `locus::RunScript` | `ctrl-shift-enter` | `Workspace` |
| `locus::TxCommit` | `ctrl-alt-enter` | `Workspace` |
| `locus::TxRollback` | `ctrl-alt-z` | `Workspace` |
| `locus::TxMode` | `ctrl-alt-t` | `Workspace` |
| `locus::Format` | `shift-alt-f` | `Workspace` |
| `locus::Explain` | `ctrl-e` | `Workspace` |
| `locus::Find` | `ctrl-f` | `Workspace` |
| `locus::Replace` | `ctrl-h` | `Workspace` |
| `locus::Escape` | `escape` | `Workspace` |
| `locus::Cancel` | `ctrl-.`, `shift-f5` | `Workspace` |
| `locus::NewTab` | `ctrl-t` | `Workspace` |
| `locus::OpenFile` | `ctrl-o` | `Workspace` |
| `locus::Save` | `ctrl-s` | `Workspace` |
| `locus::SaveAs` | `ctrl-shift-s` | `Workspace` |
| `locus::Palette` | `ctrl-k`, `ctrl-shift-p` | `Workspace` |
| `locus::ToggleSchema` | `ctrl-j` | `Workspace` |
| `locus::History` | `ctrl-shift-h` | `Workspace` |
| `locus::Settings` | `ctrl-,` | `Workspace` |
| `locus::Export` | `ctrl-shift-e` | `Workspace` |
| `locus::OpenDiagram` | `ctrl-shift-r` | `Workspace` |
| `locus::ToggleZoom` | `ctrl-shift-m` | `Workspace` |
| `locus::CloseTab` | `ctrl-w` | `Workspace` |
| `locus::NextTab` | `ctrl-tab` | `Workspace` |
| `locus::PrevTab` | `ctrl-shift-tab` | `Workspace` |
| `locus::ToggleSidebar` | `ctrl-b` | `Workspace` |
| `locus::IconSidebar` | `ctrl-shift-b` | `Workspace` |
| `locus::SplitRight` | `ctrl-\`, `ctrl-shift-k right` | `Workspace` |
| `locus::SplitDown` | `ctrl-shift-\|`, `ctrl-shift-\`, `ctrl-shift-k down` | `Workspace` |
| `locus::SplitLeft` | `ctrl-shift-k left` | `Workspace` |
| `locus::SplitUp` | `ctrl-shift-k up` | `Workspace` |
| `locus::ClosePane` | `ctrl-shift-w` | `Workspace` |
| `locus::NextPane` | `f6` | `Workspace` |
| `locus::PrevPane` | `shift-f6` | `Workspace` |
| `locus::NextSpace` | `ctrl-pagedown` | `Workspace` |
| `locus::PrevSpace` | `ctrl-pageup` | `Workspace` |
| `locus::Space1` | `ctrl-shift-!`, `ctrl-!`, `ctrl-shift-1`, `ctrl-1` | `Workspace` |
| `locus::Space2` | `ctrl-shift-@`, `ctrl-@`, `ctrl-shift-2`, `ctrl-2` | `Workspace` |
| `locus::Space3` | `ctrl-shift-#`, `ctrl-#`, `ctrl-shift-3`, `ctrl-3` | `Workspace` |
| `locus::Space4` | `ctrl-shift-$`, `ctrl-$`, `ctrl-shift-4`, `ctrl-4` | `Workspace` |
| `locus::Space5` | `ctrl-shift-%`, `ctrl-%`, `ctrl-shift-5`, `ctrl-5` | `Workspace` |
| `locus::Space6` | `ctrl-shift-^`, `ctrl-^`, `ctrl-shift-6`, `ctrl-6` | `Workspace` |
| `locus::Space7` | `ctrl-shift-&`, `ctrl-&`, `ctrl-shift-7`, `ctrl-7` | `Workspace` |
| `locus::Space8` | `ctrl-shift-*`, `ctrl-*`, `ctrl-shift-8`, `ctrl-8` | `Workspace` |
| `locus::Space9` | `ctrl-shift-(`, `ctrl-(`, `ctrl-shift-9`, `ctrl-9` | `Workspace` |
| `locus::GridCopy` | `ctrl-c` | `Grid` |
| `locus::GridCopyColumns` | `ctrl-shift-c` | `Grid` |
| `locus::GridExtendUp` | `shift-up` | `Grid` |
| `locus::GridExtendDown` | `shift-down` | `Grid` |
| `locus::GridExtendLeft` | `shift-left` | `Grid` |
| `locus::GridExtendRight` | `shift-right` | `Grid` |
| `locus::GridEdit` | `f2`, `enter` | `Grid` |
| `locus::AcceptCompletion` | `tab` | `Editor > Input` |
| `locus::PrevSnippetStop` | `shift-tab` | `SnippetMode > Input` |
| `locus::ExitSnippet` | `escape` | `SnippetMode > Input` |
| `locus::ImportData` | (palette only) | `Workspace` |
| `locus::ExplainAnalyze` | (palette only) | `Workspace` |
| `locus::LockAllProd` | (palette only) | `Workspace` |
| `locus::BindFolder` | (palette only) | `Workspace` |
| `locus::UnbindFolder` | (palette only) | `Workspace` |
| `locus::NewSplit` | (palette only) | `Workspace` |
| `locus::Unsplit` | (palette only) | `Workspace` |
| `locus::SaveQuery` | (palette only) | `Workspace` |
| `locus::OpenSaved` | (palette only) | `Workspace` |
| `locus::ShowVariables` | (palette only) | `Workspace` |
