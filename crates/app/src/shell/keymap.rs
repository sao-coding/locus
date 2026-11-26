//! Keyboard shortcuts as GPUI actions: the `locus::*` actions, their default
//! bindings (table in `locus_core::keymap`), the user's `keymap.json`, and the
//! key text the palette and Settings show (read from the live keymap, so it
//! always matches what the keyboard does).
//!
//! Every shell action is handled on the shell's root element (key context
//! `Workspace`); actions bubble up to it from the focused element. The
//! results grid's keys are handled by the query tab (context `Grid`).
use super::*;
use locus_core::keymap as km;
use std::rc::Rc;

macro_rules! commands {
    ($($name:ident),* $(,)?) => {
        /// The shell's actions (`locus::Run`, ...).
        pub(crate) mod act {
            gpui_kit::actions!(locus, [
                $($name,)*
                /// Esc: cancel / close / leave the maximised pane, in `esc_step` order.
                Escape,
                Space1, Space2, Space3, Space4, Space5, Space6, Space7, Space8, Space9,
                GridCopy, GridCopyColumns, GridExtendUp, GridExtendDown, GridExtendLeft,
                GridExtendRight, GridEdit,
                /// Tab in the SQL editor: accept the highlighted completion (popup open) or indent.
                AcceptCompletion,
            ]);
        }

        impl Shortcut {
            /// The action that runs this command (for its key text).
            pub(crate) fn action(self) -> Box<dyn Action> {
                match self {
                    $(Shortcut::$name => Box::new(act::$name),)*
                    Shortcut::Space(i) => space_action(i),
                }
            }
        }

        /// Every shell action's handler, on the root element.
        pub(crate) fn register(el: Div, cx: &mut Context<Locus>) -> Div {
            el
                $(.on_action(cx.listener(|this, _: &act::$name, w, cx| {
                    this.on_shortcut(Shortcut::$name, false, w, cx)
                })))*
                .on_action(cx.listener(|this, _: &act::Escape, w, cx| {
                    this.on_shortcut(Shortcut::Cancel, true, w, cx)
                }))
                .on_action(cx.listener(|this, _: &act::Space1, w, cx| this.on_shortcut(Shortcut::Space(0), false, w, cx)))
                .on_action(cx.listener(|this, _: &act::Space2, w, cx| this.on_shortcut(Shortcut::Space(1), false, w, cx)))
                .on_action(cx.listener(|this, _: &act::Space3, w, cx| this.on_shortcut(Shortcut::Space(2), false, w, cx)))
                .on_action(cx.listener(|this, _: &act::Space4, w, cx| this.on_shortcut(Shortcut::Space(3), false, w, cx)))
                .on_action(cx.listener(|this, _: &act::Space5, w, cx| this.on_shortcut(Shortcut::Space(4), false, w, cx)))
                .on_action(cx.listener(|this, _: &act::Space6, w, cx| this.on_shortcut(Shortcut::Space(5), false, w, cx)))
                .on_action(cx.listener(|this, _: &act::Space7, w, cx| this.on_shortcut(Shortcut::Space(6), false, w, cx)))
                .on_action(cx.listener(|this, _: &act::Space8, w, cx| this.on_shortcut(Shortcut::Space(7), false, w, cx)))
                .on_action(cx.listener(|this, _: &act::Space9, w, cx| this.on_shortcut(Shortcut::Space(8), false, w, cx)))
        }

        /// Names of every action (for docs/keymap.md and the tests).
        #[cfg_attr(not(test), allow(dead_code))]
        pub(crate) const ACTION_NAMES: &[&str] = &[
            $(stringify!($name),)* "Escape",
            "Space1", "Space2", "Space3", "Space4", "Space5", "Space6", "Space7", "Space8", "Space9",
            "GridCopy", "GridCopyColumns", "GridExtendUp", "GridExtendDown", "GridExtendLeft",
            "GridExtendRight", "GridEdit", "AcceptCompletion",
        ];
    };
}

commands!(
    Run,
    RunScript,
    Cancel,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    NextSpace,
    OpenFile,
    Save,
    SaveAs,
    Palette,
    Settings,
    Export,
    ImportData,
    PrevSpace,
    ToggleSidebar,
    IconSidebar,
    SplitRight,
    SplitLeft,
    SplitUp,
    SplitDown,
    ClosePane,
    NextPane,
    PrevPane,
    ToggleSchema,
    Format,
    Find,
    Replace,
    Explain,
    ExplainAnalyze,
    History,
    LockAllProd,
    OpenDiagram,
    BindFolder,
    UnbindFolder,
    ToggleZoom,
    NewSplit,
    Unsplit,
    TxCommit,
    TxRollback,
    TxMode,
    SaveQuery,
    OpenSaved,
    ToggleGit,
    GitCommit,
    GitPull,
    GitPush,
    GitFetch,
    GitSwitchBranch,
    GitRefresh,
);

fn space_action(i: usize) -> Box<dyn Action> {
    match i {
        0 => Box::new(act::Space1),
        1 => Box::new(act::Space2),
        2 => Box::new(act::Space3),
        3 => Box::new(act::Space4),
        4 => Box::new(act::Space5),
        5 => Box::new(act::Space6),
        6 => Box::new(act::Space7),
        7 => Box::new(act::Space8),
        _ => Box::new(act::Space9),
    }
}

fn binding(
    keys: &str,
    action: Box<dyn Action>,
    context: Option<&str>,
    cx: &App,
) -> Result<KeyBinding, String> {
    let pred = match context {
        Some(c) => Some(Rc::new(
            KeyBindingContextPredicate::parse(c).map_err(|e| format!("context \"{c}\": {e}"))?,
        )),
        None => None,
    };
    KeyBinding::load(keys, action, pred, false, None, cx).map_err(|e| format!("\"{keys}\": {e}"))
}

/// The default bindings, built from `locus_core::keymap::default_bindings`.
pub(crate) fn defaults(cx: &App) -> Vec<KeyBinding> {
    km::default_bindings()
        .into_iter()
        .map(|(ctx, keys, name)| {
            let action = cx
                .build_action(&format!("locus::{name}"), None)
                .unwrap_or_else(|e| panic!("default action locus::{name}: {e}"));
            binding(&keys, action, Some(&ctx), cx)
                .unwrap_or_else(|e| panic!("default binding {keys}: {e}"))
        })
        .collect()
}

/// The user's bindings; `Err` for each entry that cannot be used.
/// A binding in context `Workspace` is mirrored like the defaults.
pub(crate) fn user(json: &str, cx: &App) -> (Vec<KeyBinding>, Vec<String>) {
    let (entries, mut errs) = km::parse_user(json);
    let mut out = Vec::new();
    for e in entries {
        let contexts: Vec<Option<String>> = match e.context.as_deref() {
            Some(km::WORKSPACE) => km::workspace_contexts().into_iter().map(Some).collect(),
            c => vec![c.map(str::to_string)],
        };
        for ctx in contexts {
            let action: Box<dyn Action> = match &e.action {
                None => Box::new(NoAction {}),
                Some(name) => match cx.build_action(name, None) {
                    Ok(a) => a,
                    Err(err) => {
                        errs.push(format!("\"{}\": {err}", e.keys));
                        break;
                    }
                },
            };
            match binding(&e.keys, action, ctx.as_deref(), cx) {
                Ok(b) => out.push(b),
                Err(err) => {
                    errs.push(err);
                    break;
                }
            }
        }
    }
    (out, errs)
}

/// Binds the defaults, then `<data dir>/keymap.json` if there is one. Call after
/// `gpui_kit::init` (at the same context depth, later bindings win). Returns the
/// problems with the user's file (logged here; the caller shows a toast).
pub(crate) fn init(cx: &mut App) -> Vec<String> {
    let d = defaults(cx);
    cx.bind_keys(d);
    let Some(path) = locus_core::settings::app_dir().map(|d| d.join("keymap.json")) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let (bindings, errs) = user(&text, cx);
    cx.bind_keys(bindings);
    for e in &errs {
        log::warn!("keymap.json: {e}");
    }
    errs
}

/// "Ctrl+Shift+K ←" for one binding.
fn binding_text(b: &KeyBinding) -> String {
    b.keystrokes()
        .iter()
        .map(|k| keystroke_text(k.modifiers(), k.key()))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn keystroke_text(m: &Modifiers, key: &str) -> String {
    let mut s = String::new();
    if m.control {
        s.push_str("Ctrl+");
    }
    if m.shift {
        s.push_str("Shift+");
    }
    if m.alt {
        s.push_str("Alt+");
    }
    let k = match key {
        "enter" => "Enter".to_string(),
        "escape" => "Esc".to_string(),
        "pagedown" => "PgDn".to_string(),
        "pageup" => "PgUp".to_string(),
        "tab" => "Tab".to_string(),
        "left" => "←".to_string(),
        "right" => "→".to_string(),
        "up" => "↑".to_string(),
        "down" => "↓".to_string(),
        "space" => "Space".to_string(),
        k => k.to_uppercase(),
    };
    s.push_str(&k);
    s
}

/// Every key of `actions` in the live keymap, highest precedence first, without
/// repeats ("Ctrl+K / Ctrl+Shift+P"); empty when unbound.
pub(crate) fn keys_text(actions: &[&dyn Action], cx: &App) -> String {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let mut out: Vec<String> = Vec::new();
    for a in actions {
        for b in keymap.bindings_for_action(*a).rev() {
            let t = binding_text(b);
            if !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out.join(" / ")
}

/// The highest-precedence key of a command ("" when unbound).
pub(crate) fn first_key(s: Shortcut, cx: &App) -> String {
    let a = s.action();
    keys_text(&[a.as_ref()], cx)
        .split(" / ")
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Key text of a palette command (Cancel also lists Esc).
pub(crate) fn command_keys(s: Shortcut, cx: &App) -> String {
    let a = s.action();
    if s == Shortcut::Cancel {
        keys_text(&[&act::Escape, a.as_ref()], cx)
    } else {
        keys_text(&[a.as_ref()], cx)
    }
}

#[cfg(test)]
mod tests;
