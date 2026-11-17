//! The key binding table against the hand-written parser it replaced, and
//! dispatch through real key contexts (an input inside `Workspace`).
use super::*;
use gpui_kit::{OperatingSystem, TestAppContext};
use locus_core::keymap as km;
// `super::*` brings GPUI's `test` macro; plain `#[test]` must stay Rust's.
use std::prelude::v1::test;

/// The parser the shell used before key bindings (kept as the oracle).
fn old_shortcut(k: &Keystroke) -> Option<Shortcut> {
    let m = &k.modifiers;
    if m.platform {
        return None;
    }
    let (c, s, a) = (m.control, m.shift, m.alt);
    Some(match (k.key.as_str(), c, s, a) {
        ("enter", true, false, false) => Shortcut::Run,
        ("enter", true, false, true) => Shortcut::TxCommit,
        ("z", true, false, true) => Shortcut::TxRollback,
        ("t", true, false, true) => Shortcut::TxMode,
        ("f", false, true, true) => Shortcut::Format,
        ("e", true, false, false) => Shortcut::Explain,
        ("f", true, false, false) => Shortcut::Find,
        ("h", true, false, false) => Shortcut::Replace,
        ("enter", true, true, false) => Shortcut::RunScript,
        ("escape", false, false, false) => Shortcut::Cancel,
        // Ctrl+. is taken by the zh-TW IME; Shift+F5 (Visual Studio's Stop) is not.
        (".", true, false, false) | ("f5", false, true, false) => Shortcut::Cancel,
        ("t", true, false, false) => Shortcut::NewTab,
        ("o", true, false, false) => Shortcut::OpenFile,
        ("s", true, false, false) => Shortcut::Save,
        ("s", true, true, false) => Shortcut::SaveAs,
        ("p", true, true, false) | ("k", true, false, false) => Shortcut::Palette,
        ("j", true, false, false) => Shortcut::ToggleSchema,
        ("h", true, true, false) => Shortcut::History,
        (",", true, false, false) => Shortcut::Settings,
        ("e", true, true, false) => Shortcut::Export,
        ("r", true, true, false) => Shortcut::OpenDiagram,
        ("m", true, true, false) => Shortcut::ToggleZoom,
        ("w", true, false, false) => Shortcut::CloseTab,
        ("tab", true, false, false) => Shortcut::NextTab,
        ("tab", true, true, false) => Shortcut::PrevTab,
        ("b", true, false, false) => Shortcut::ToggleSidebar,
        ("b", true, true, false) => Shortcut::IconSidebar,
        ("\\", true, false, false) => Shortcut::SplitRight,
        ("\\", true, true, false) | ("|", true, true, false) => Shortcut::SplitDown,
        ("w", true, true, false) => Shortcut::ClosePane,
        ("f6", false, false, false) => Shortcut::NextPane,
        ("pagedown", true, false, false) => Shortcut::NextSpace,
        ("pageup", true, false, false) => Shortcut::PrevSpace,
        ("f6", false, true, false) => Shortcut::PrevPane,
        // The zh-TW IME swallows Ctrl+digit; Ctrl+Shift+digit gets through and
        // arrives as the shifted character (US layout), so both switch Spaces.
        (sym, true, _, false) if "!@#$%^&*(".contains(sym) && sym.len() == 1 => {
            Shortcut::Space("!@#$%^&*(".find(sym).unwrap())
        }
        (d, true, true, false)
            if d.len() == 1 && ('1'..='9').contains(&d.chars().next().unwrap()) =>
        {
            Shortcut::Space(d.parse::<usize>().unwrap() - 1)
        }
        (d, true, false, false)
            if d.len() == 1 && ('1'..='9').contains(&d.chars().next().unwrap()) =>
        {
            Shortcut::Space(d.parse::<usize>().unwrap() - 1)
        }
        _ => return None,
    })
}

fn parse(s: &str) -> Keystroke {
    Keystroke::parse_with_os(s, OperatingSystem::Windows).unwrap()
}

/// The action name the old parser gave a keystroke (`Escape` for Esc, which it
/// sent as Cancel with the key attached).
fn oracle(k: &Keystroke) -> Option<String> {
    old_shortcut(k).map(|s| match s {
        Shortcut::Space(i) => format!("Space{}", i + 1),
        Shortcut::Cancel if k.key == "escape" => "Escape".into(),
        s => format!("{s:?}"),
    })
}

/// The action the default table binds to a single keystroke.
fn table(k: &Keystroke) -> Option<String> {
    let mut found = None;
    for (ctx, keys, name) in km::default_bindings() {
        if ctx != km::WORKSPACE || keys.contains(' ') {
            continue;
        }
        let b = parse(&keys);
        if b.key == k.key && b.modifiers == k.modifiers {
            found = Some(name);
        }
    }
    found
}

#[test]
fn defaults_match_the_old_parser_for_every_key() {
    let mut keys: Vec<String> = ('a'..='z').chain('0'..='9').map(String::from).collect();
    keys.extend(r"!@#$%^&*()\|,./;'[]=`~".chars().map(String::from));
    for k in [
        "enter",
        "escape",
        "tab",
        "space",
        "backspace",
        "delete",
        "pageup",
        "pagedown",
        "home",
        "end",
        "left",
        "right",
        "up",
        "down",
    ] {
        keys.push(k.into());
    }
    keys.extend((1..=12).map(|n| format!("f{n}")));
    let mut checked = 0;
    for key in &keys {
        for bits in 0..8u8 {
            let mut s = String::new();
            if bits & 1 != 0 {
                s.push_str("ctrl-");
            }
            if bits & 2 != 0 {
                s.push_str("shift-");
            }
            if bits & 4 != 0 {
                s.push_str("alt-");
            }
            s.push_str(key);
            let ks = parse(&s);
            assert_eq!(table(&ks), oracle(&ks), "{s}");
            checked += 1;
        }
    }
    assert!(checked > 500);
}

#[test]
fn split_chords() {
    let d = km::default_bindings();
    for (arrow, name) in [
        ("left", "SplitLeft"),
        ("right", "SplitRight"),
        ("up", "SplitUp"),
        ("down", "SplitDown"),
    ] {
        let keys = format!("ctrl-shift-k {arrow}");
        assert!(
            d.iter()
                .any(|(c, k, a)| c == km::WORKSPACE && *k == keys && a == name),
            "{keys}"
        );
    }
}

#[test]
fn every_action_is_documented() {
    let doc = include_str!("../../../../../docs/keymap.md");
    for name in ACTION_NAMES {
        assert!(
            doc.contains(&format!("`locus::{name}`")),
            "docs/keymap.md lacks {name}"
        );
    }
}

#[test]
fn key_text() {
    let m = Modifiers {
        control: true,
        shift: true,
        ..Default::default()
    };
    assert_eq!(keystroke_text(&m, "p"), "Ctrl+Shift+P");
    assert_eq!(keystroke_text(&Modifiers::default(), "escape"), "Esc");
}

/// Prints the table in docs/keymap.md:
/// `cargo test -p app --release keymap_table -- --ignored --nocapture`.
#[test]
#[ignore]
fn keymap_table() {
    let mut rows: Vec<(String, Vec<String>, String)> = Vec::new();
    for (ctx, keys, name) in km::default_bindings() {
        if ctx.contains('>') {
            continue;
        }
        match rows.iter_mut().find(|r| r.0 == name) {
            Some(r) => r.1.push(keys),
            None => rows.push((name, vec![keys], ctx)),
        }
    }
    for n in ACTION_NAMES {
        if !rows.iter().any(|r| r.0 == *n) {
            rows.push((n.to_string(), Vec::new(), km::WORKSPACE.into()));
        }
    }
    println!("| Action | Default keys | Context |\n|---|---|---|");
    for (name, keys, ctx) in rows {
        let keys: Vec<String> = keys.iter().map(|k| format!("`{k}`")).collect();
        let keys = if keys.is_empty() {
            "(palette only)".to_string()
        } else {
            keys.join(", ")
        };
        println!(
            "| `locus::{name}` | {} | `{ctx}` |",
            keys.replace('|', r"\|")
        );
    }
}

// ---- dispatch through key contexts ------------------------------------------

/// A stand-in for the shell: `Workspace` with a text input inside, recording
/// the shell actions it receives. Esc is passed on (as in the editor).
struct Probe {
    focus: FocusHandle,
    input: Entity<gpui_kit::component::input::EditorState>,
    got: Vec<&'static str>,
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &act::Run, _, _| this.got.push("Run")))
            .on_action(cx.listener(|this, _: &act::Escape, _, cx| {
                this.got.push("Escape");
                cx.propagate();
            }))
            .on_action(cx.listener(|this, _: &act::Find, _, _| this.got.push("Find")))
            .on_action(cx.listener(|this, _: &act::Explain, _, _| this.got.push("Explain")))
            .child(gpui_kit::component::input::Editor::new(&self.input))
    }
}

fn probe(cx: &mut TestAppContext) -> (Entity<Probe>, &mut gpui_kit::VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        let d = defaults(cx);
        cx.bind_keys(d);
    });
    let (view, vcx) = cx.add_window_view(|window, cx| {
        let input = cx.new(|cx| {
            gpui_kit::component::input::EditorState::new(window, cx)
                .searchable(false)
                .language("sql")
        });
        Probe {
            focus: cx.focus_handle(),
            input,
            got: Vec::new(),
        }
    });
    vcx.update(|window, cx| {
        let input = view.read(cx).input.clone();
        input.read(cx).focus_handle(cx).focus(window, cx);
    });
    (view, vcx)
}

#[gpui_kit::test]
fn ctrl_enter_runs_from_the_editor(cx: &mut TestAppContext) {
    let (view, vcx) = probe(cx);
    vcx.simulate_keystrokes("ctrl-enter ctrl-f ctrl-e");
    let (got, text) = vcx.update(|_, cx| {
        let v = view.read(cx);
        (v.got.clone(), v.input.read(cx).value().to_string())
    });
    // Ours, not the editor's (new line, search panel, end of line).
    assert_eq!(got, ["Run", "Find", "Explain"]);
    assert_eq!(text, "");
}

#[gpui_kit::test]
fn esc_reaches_the_shell_first(cx: &mut TestAppContext) {
    let (view, vcx) = probe(cx);
    vcx.simulate_keystrokes("escape");
    let got = vcx.update(|_, cx| view.read(cx).got.clone());
    // First at the input's depth (the `Workspace > Input` mirror), before the
    // input's own Esc; passed on, it reaches the `Workspace` binding again
    // (a command that does not apply has no effect, so the repeat is harmless).
    assert_eq!(got, ["Escape", "Escape"]);
}

#[gpui_kit::test]
fn user_keymap_overrides_and_reports_bad_entries(cx: &mut TestAppContext) {
    let (view, vcx) = probe(cx);
    let errs = vcx.update(|_, cx| {
        let (b, errs) = user(
            r#"[{"context": "Workspace", "bindings": {
                "ctrl-r": "locus::Run", "ctrl-enter": null, "ctrl-q": "locus::NoSuchAction"}},
                {"context": "Workspace >", "bindings": {"f9": "locus::Run"}}]"#,
            cx,
        );
        cx.bind_keys(b);
        errs
    });
    assert_eq!(errs.len(), 2, "{errs:?}");
    vcx.simulate_keystrokes("ctrl-r ctrl-enter");
    let got = vcx.update(|_, cx| view.read(cx).got.clone());
    assert_eq!(got, ["Run"]);
    // The key text follows the user's keymap.
    let text = vcx.update(|_, cx| command_keys(Shortcut::Run, cx));
    assert_eq!(text, "Ctrl+R");
}
