//! Key bindings without GPUI: the default binding table, the user's
//! `keymap.json` (Zed's shape) and the order Esc is handled in.
//!
//! The app turns these strings into GPUI `KeyBinding`s (`app::shell::keymap`).
//! Action names are `locus::<Name>`; `docs/keymap.md` lists them.

/// Context of the shell's root element; every default below is bound in it.
pub const WORKSPACE: &str = "Workspace";

/// Contexts inside the shell whose own bindings use some of our keys (the
/// text input binds Esc, Ctrl+E, Ctrl+F, Ctrl+H, Ctrl+., Ctrl+Shift+E; the
/// results table binds Esc). A binding in a deeper context wins, so each
/// default is also bound as `Workspace > <context>`; ours are added after
/// gpui-kit's, so at the same depth ours win, as when the shell took keys first.
pub const MIRRORED: &[&str] = &["Input", "DataTable"];

/// Context of the results grid (wraps gpui-kit's `DataTable`).
pub const GRID: &str = "Grid";

/// (keystrokes, action name without `locus::`), bound in `Workspace`.
/// When an action has several keys, the one listed last is shown first.
pub const DEFAULTS: &[(&str, &str)] = &[
    ("ctrl-enter", "Run"),
    ("ctrl-shift-enter", "RunScript"),
    ("ctrl-alt-enter", "TxCommit"),
    ("ctrl-alt-z", "TxRollback"),
    ("ctrl-alt-t", "TxMode"),
    ("shift-alt-f", "Format"),
    ("ctrl-e", "Explain"),
    ("ctrl-f", "Find"),
    ("ctrl-h", "Replace"),
    ("escape", "Escape"),
    // Ctrl+. is taken by the zh-TW IME; Shift+F5 (Visual Studio's Stop) is not.
    ("ctrl-.", "Cancel"),
    ("shift-f5", "Cancel"),
    ("ctrl-t", "NewTab"),
    ("ctrl-o", "OpenFile"),
    ("ctrl-s", "Save"),
    ("ctrl-shift-s", "SaveAs"),
    ("ctrl-k", "Palette"),
    ("ctrl-shift-p", "Palette"),
    ("ctrl-j", "ToggleSchema"),
    ("ctrl-shift-h", "History"),
    ("ctrl-shift-g", "ToggleGit"),
    ("ctrl-,", "Settings"),
    ("ctrl-shift-e", "Export"),
    ("ctrl-shift-r", "OpenDiagram"),
    ("ctrl-shift-m", "ToggleZoom"),
    ("ctrl-w", "CloseTab"),
    ("ctrl-tab", "NextTab"),
    ("ctrl-shift-tab", "PrevTab"),
    ("ctrl-b", "ToggleSidebar"),
    ("ctrl-shift-b", "IconSidebar"),
    ("ctrl-\\", "SplitRight"),
    ("ctrl-shift-|", "SplitDown"),
    ("ctrl-shift-\\", "SplitDown"),
    // Ctrl+Shift+K then an arrow (Ctrl+K alone opens the palette).
    ("ctrl-shift-k left", "SplitLeft"),
    ("ctrl-shift-k right", "SplitRight"),
    ("ctrl-shift-k up", "SplitUp"),
    ("ctrl-shift-k down", "SplitDown"),
    ("ctrl-shift-w", "ClosePane"),
    ("f6", "NextPane"),
    ("shift-f6", "PrevPane"),
    ("ctrl-pagedown", "NextSpace"),
    ("ctrl-pageup", "PrevSpace"),
    // Spaces: see `space_keys`.
];

/// The symbol row on a US layout: Ctrl+Shift+digit arrives as these.
pub const SPACE_SYMBOLS: &str = "!@#$%^&*(";

/// Keys of Space `n` (1..=9). The zh-TW IME swallows Ctrl+digit; Ctrl+Shift+digit
/// gets through and arrives as the shifted character, so all four switch Spaces.
pub fn space_keys(n: usize) -> [String; 4] {
    let sym = &SPACE_SYMBOLS[n - 1..n];
    [
        format!("ctrl-shift-{sym}"),
        format!("ctrl-{sym}"),
        format!("ctrl-shift-{n}"),
        format!("ctrl-{n}"),
    ]
}

/// The results grid's keys, bound in `Grid`.
pub const GRID_DEFAULTS: &[(&str, &str)] = &[
    ("ctrl-c", "GridCopy"),
    ("ctrl-shift-c", "GridCopyColumns"),
    ("shift-up", "GridExtendUp"),
    ("shift-down", "GridExtendDown"),
    ("shift-left", "GridExtendLeft"),
    ("shift-right", "GridExtendRight"),
    ("f2", "GridEdit"),
    ("enter", "GridEdit"),
];

/// The SQL editor's own key, bound in `Editor > Input` (the editor wrapper around
/// gpui-kit's input, so a deeper context than the input's own Tab = indent): Tab
/// accepts the highlighted completion while the popup is open and indents otherwise.
pub const EDITOR: &str = "Editor > Input";
pub const EDITOR_DEFAULTS: &[(&str, &str)] = &[("tab", "AcceptCompletion")];

/// Every default as (context, keystrokes, action name without namespace),
/// in the order they are bound.
pub fn default_bindings() -> Vec<(String, String, String)> {
    let mut flat: Vec<(String, String)> = DEFAULTS
        .iter()
        .map(|(k, a)| (k.to_string(), a.to_string()))
        .collect();
    for n in 1..=9 {
        for k in space_keys(n) {
            flat.push((k, format!("Space{n}")));
        }
    }
    let mut out = Vec::new();
    for ctx in workspace_contexts() {
        for (k, a) in &flat {
            out.push((ctx.clone(), k.clone(), a.clone()));
        }
    }
    for (k, a) in GRID_DEFAULTS {
        out.push((GRID.to_string(), k.to_string(), a.to_string()));
    }
    for (k, a) in EDITOR_DEFAULTS {
        out.push((EDITOR.to_string(), k.to_string(), a.to_string()));
    }
    out
}

/// `Workspace` and its mirrors (`Workspace > Input`, ...).
pub fn workspace_contexts() -> Vec<String> {
    std::iter::once(WORKSPACE.to_string())
        .chain(MIRRORED.iter().map(|m| format!("{WORKSPACE} > {m}")))
        .collect()
}

/// One entry of the user's keymap.
#[derive(Clone, Debug, PartialEq)]
pub struct UserBinding {
    pub context: Option<String>,
    pub keys: String,
    /// `None`: `null` in the file, which removes the key's binding in that context.
    pub action: Option<String>,
}

/// Parses `keymap.json`: `[{"context": "QueryTab", "bindings": {"ctrl-enter": "locus::Run"}}]`.
/// Returns the usable entries and one message per entry that could not be read.
pub fn parse_user(json: &str) -> (Vec<UserBinding>, Vec<String>) {
    let mut ok = Vec::new();
    let mut errs = Vec::new();
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => return (ok, vec![format!("keymap.json: {e}")]),
    };
    let Some(sections) = v.as_array() else {
        return (
            ok,
            vec!["keymap.json: expected an array of sections".into()],
        );
    };
    for (i, s) in sections.iter().enumerate() {
        let Some(obj) = s.as_object() else {
            errs.push(format!("section {i}: expected an object"));
            continue;
        };
        let context = match obj.get("context") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(c)) => Some(c.clone()),
            Some(_) => {
                errs.push(format!("section {i}: \"context\" must be a string"));
                continue;
            }
        };
        let Some(bindings) = obj.get("bindings").and_then(|b| b.as_object()) else {
            errs.push(format!("section {i}: \"bindings\" must be an object"));
            continue;
        };
        for (keys, a) in bindings {
            let action = match a {
                serde_json::Value::Null => None,
                serde_json::Value::String(a) => Some(a.clone()),
                _ => {
                    errs.push(format!(
                        "section {i}, \"{keys}\": the action must be a name or null"
                    ));
                    continue;
                }
            };
            ok.push(UserBinding {
                context: context.clone(),
                keys: keys.clone(),
                action,
            });
        }
    }
    (ok, errs)
}

/// What the shell knows when Esc (or Cancel) arrives.
#[derive(Clone, Copy, Debug, Default)]
pub struct EscState {
    /// Renaming a tab / Space in the sidebar.
    pub renaming: bool,
    /// The tab is a page (History, Settings...): no query, results or file.
    pub page: bool,
    /// A query is running or paused.
    pub busy: bool,
    pub find_open: bool,
    /// A grid cell is being edited (and the grid has focus).
    pub cell_editing: bool,
    pub in_editor: bool,
    pub zoomed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EscStep {
    CloseRename,
    CancelQuery,
    CloseFind,
    CancelCellEdit,
    LeaveZoom,
    /// Not ours: the focused element's own binding gets the key (the editor's
    /// completion popup, a menu...).
    PassOn,
}

/// The order Esc is handled in. `esc` is false for the other Cancel keys
/// (Ctrl+., Shift+F5, the palette), which only stop a query.
pub fn esc_step(s: EscState, esc: bool) -> EscStep {
    if s.renaming {
        return EscStep::CloseRename;
    }
    if s.page {
        return EscStep::PassOn;
    }
    if s.busy {
        return EscStep::CancelQuery;
    }
    if !esc {
        return EscStep::PassOn;
    }
    if s.find_open {
        return EscStep::CloseFind;
    }
    if s.cell_editing {
        return EscStep::CancelCellEdit;
    }
    if s.in_editor || !s.zoomed {
        return EscStep::PassOn;
    }
    EscStep::LeaveZoom
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_keymap_parses_and_reports_bad_entries() {
        let (ok, errs) = parse_user(
            r#"[
              {"context": "QueryTab", "bindings": {"ctrl-r": "locus::Run", "ctrl-enter": null, "ctrl-x": 3}},
              {"bindings": {"f9": "locus::Run"}},
              {"context": 1, "bindings": {}},
              "nope"
            ]"#,
        );
        // Key order inside one object follows serde_json's map (sorted, or file order
        // with `preserve_order`, which other crates switch on).
        let mut ok = ok;
        ok.sort_by(|a, b| (&a.context, &a.keys).cmp(&(&b.context, &b.keys)));
        assert_eq!(
            ok,
            vec![
                UserBinding {
                    context: None,
                    keys: "f9".into(),
                    action: Some("locus::Run".into())
                },
                UserBinding {
                    context: Some("QueryTab".into()),
                    keys: "ctrl-enter".into(),
                    action: None
                },
                UserBinding {
                    context: Some("QueryTab".into()),
                    keys: "ctrl-r".into(),
                    action: Some("locus::Run".into())
                },
            ]
        );
        assert_eq!(errs.len(), 3, "{errs:?}");
    }

    #[test]
    fn broken_json_is_one_error() {
        let (ok, errs) = parse_user("[{");
        assert!(ok.is_empty());
        assert_eq!(errs.len(), 1);
        assert_eq!(parse_user("{}").1.len(), 1);
    }

    #[test]
    fn esc_order() {
        let all = EscState {
            renaming: true,
            page: true,
            busy: true,
            find_open: true,
            cell_editing: true,
            in_editor: true,
            zoomed: true,
        };
        let mut s = all;
        assert_eq!(esc_step(s, true), EscStep::CloseRename);
        s.renaming = false;
        assert_eq!(esc_step(s, true), EscStep::PassOn);
        s.page = false;
        assert_eq!(esc_step(s, true), EscStep::CancelQuery);
        s.busy = false;
        assert_eq!(esc_step(s, true), EscStep::CloseFind);
        assert_eq!(esc_step(s, false), EscStep::PassOn);
        s.find_open = false;
        assert_eq!(esc_step(s, true), EscStep::CancelCellEdit);
        s.cell_editing = false;
        // In the editor Esc is the editor's (completion popup), even when maximised.
        assert_eq!(esc_step(s, true), EscStep::PassOn);
        s.in_editor = false;
        assert_eq!(esc_step(s, true), EscStep::LeaveZoom);
        s.zoomed = false;
        assert_eq!(esc_step(s, true), EscStep::PassOn);
        // Ctrl+. stops a query but nothing else.
        let busy = EscState {
            busy: true,
            ..Default::default()
        };
        assert_eq!(esc_step(busy, false), EscStep::CancelQuery);
    }

    #[test]
    fn defaults_are_mirrored() {
        let d = default_bindings();
        let n = DEFAULTS.len() + 9 * 4;
        assert_eq!(
            d.len(),
            n * (1 + MIRRORED.len()) + GRID_DEFAULTS.len() + EDITOR_DEFAULTS.len()
        );
        assert!(
            d.iter()
                .any(|(c, k, a)| c == "Workspace > Input" && k == "ctrl-enter" && a == "Run")
        );
    }
}
