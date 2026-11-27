//! Keyboard shortcuts taken before the editor's bindings, and the actions they run.
use super::*;

/// Shortcuts the shell takes before the editor's own key bindings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Shortcut {
    Run,
    RunScript,
    Cancel,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    Space(usize),
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
    /// Right panel: Schema (Ctrl+J) / History.
    ToggleSchema,
    /// Editor tools.
    Format,
    Find,
    Replace,
    Explain,
    ExplainAnalyze,
    /// Ctrl+Shift+H: the Query History tab.
    History,
    /// Palette: lock every Production connection now.
    LockAllProd,
    /// Ctrl+Shift+R: the diagram of the current connection / schema.
    OpenDiagram,
    /// Palette: bind / unbind the Space to a folder of .sql files.
    BindFolder,
    UnbindFolder,
    /// Ctrl+Shift+M: maximise the focused pane for now, or restore it.
    ToggleZoom,
    /// Palette: a new query in a new pane on the right.
    NewSplit,
    /// Palette: dissolve the focused pane's split group (keeps the tabs).
    Unsplit,
    /// Manual transactions: Commit (Ctrl+Alt+Enter), Rollback (Ctrl+Alt+Z), mode (Ctrl+Alt+T).
    TxCommit,
    TxRollback,
    TxMode,
    /// Palette: save the editor text (or selection) as a saved query.
    SaveQuery,
    /// Palette: the client-side variables of the tab (`@set`, `${name}`).
    ShowVariables,
    /// Palette: the Saved tab (queries and snippets).
    OpenSaved,
}

impl Locus {
    // ---- keys -------------------------------------------------------------

    /// A shell action arrived from a key binding: runs it, or lets the key go on
    /// to the next binding (the focused element's own) when it does not apply.
    pub(crate) fn on_shortcut(
        &mut self,
        s: Shortcut,
        esc: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            cx.propagate();
            return;
        }
        let handled = self.perform(s, esc, window, cx);
        cx.notify();
        if !handled {
            cx.propagate();
        }
    }

    /// Runs one command (a key binding, or a palette / welcome entry). `esc`:
    /// it came from Esc, which does more than the other Cancel keys. Returns
    /// false when the command does not apply here.
    pub(crate) fn perform(
        &mut self,
        s: Shortcut,
        esc: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // Esc on the Space popover closes it, keeping the colours it previews.
        if s == Shortcut::Cancel && self.space_pop.is_some() {
            self.close_space_popover(cx);
            return true;
        }
        if s == Shortcut::Cancel && self.renaming.is_some() {
            self.renaming = None;
            cx.notify();
            return true;
        }
        let tab = self.active_tab(cx);
        // History / Settings pages have no editor, results or file.
        if tab.read(cx).pageslot.kind.is_some()
            && matches!(
                s,
                Shortcut::Run
                    | Shortcut::RunScript
                    | Shortcut::Cancel
                    | Shortcut::Save
                    | Shortcut::SaveAs
                    | Shortcut::Export
                    | Shortcut::SaveQuery
            )
        {
            return false;
        }
        match s {
            Shortcut::Run => self.run_tab(tab, RunMode::Current, None, window, cx),
            Shortcut::RunScript => self.run_tab(tab, RunMode::Script, None, window, cx),
            Shortcut::Cancel => {
                // A diagram first closes its search, then clears its selection.
                if tab.read(cx).diagram.is_some()
                    && tab.update(cx, |t, cx| t.diagram_escape(window, cx))
                {
                    return true;
                }
                // The order is `locus_core::keymap::esc_step` (unit-tested there).
                use locus_core::keymap::{EscState, EscStep, esc_step};
                let t = tab.read(cx);
                let state = EscState {
                    renaming: false,
                    page: false,
                    busy: t.busy(cx) || t.pg.paused,
                    find_open: t.find.is_some(),
                    cell_editing: t.grid_editing(window, cx),
                    in_editor: t
                        .editor
                        .read(cx)
                        .state
                        .read(cx)
                        .focus_handle(cx)
                        .contains_focused(window, cx),
                    zoomed: self.space().zoomed(),
                };
                match esc_step(state, esc) {
                    EscStep::CloseRename | EscStep::PassOn => return false,
                    EscStep::CancelQuery => tab.update(cx, |t, cx| t.cancel(cx)),
                    EscStep::CloseFind => tab.update(cx, |t, cx| t.close_find(window, cx)),
                    EscStep::CancelCellEdit => {
                        tab.update(cx, |t, cx| t.grid_cancel_edit(window, cx));
                    }
                    EscStep::LeaveZoom => {
                        if !self.leave_zoom(cx) {
                            return false;
                        }
                    }
                }
            }
            Shortcut::NewTab => self.new_tab(window, cx),
            Shortcut::CloseTab => {
                let id = tab.read(cx).id.clone();
                self.close_tab(&id, window, cx);
            }
            Shortcut::NextTab => self.cycle_tab(1, window, cx),
            Shortcut::PrevTab => self.cycle_tab(-1, window, cx),
            Shortcut::Space(i) => {
                if i >= self.spaces.len() {
                    return false;
                }
                self.switch_space(i, window, cx);
            }
            Shortcut::NextSpace | Shortcut::PrevSpace => {
                let n = self.spaces.len();
                let i = if s == Shortcut::NextSpace {
                    (self.active_space + 1) % n
                } else {
                    (self.active_space + n - 1) % n
                };
                self.switch_space(i, window, cx);
            }
            Shortcut::ToggleSidebar => {
                let m = if self.sidebar == SidebarMode::Hidden {
                    SidebarMode::Full
                } else {
                    SidebarMode::Hidden
                };
                self.set_sidebar_mode(m, cx);
            }
            Shortcut::IconSidebar => {
                let m = if self.sidebar == SidebarMode::Icons {
                    SidebarMode::Full
                } else {
                    SidebarMode::Icons
                };
                self.set_sidebar_mode(m, cx);
            }
            Shortcut::SplitRight => self.split(None, SplitAxis::Row, false, window, cx),
            Shortcut::SplitDown => self.split(None, SplitAxis::Column, false, window, cx),
            Shortcut::SplitLeft => self.split(None, SplitAxis::Row, true, window, cx),
            Shortcut::SplitUp => self.split(None, SplitAxis::Column, true, window, cx),
            Shortcut::ClosePane => {
                let f = self.space().focused;
                self.close_pane(f, window, cx);
            }
            Shortcut::NextPane | Shortcut::PrevPane => {
                let n = self.space().panes().len();
                let f = self.space().focused;
                let next = if s == Shortcut::NextPane {
                    (f + 1) % n
                } else {
                    (f + n - 1) % n
                };
                self.focus_pane(next, window, cx);
            }
            Shortcut::OpenFile => self.open_file(window, cx),
            Shortcut::Save => self.save_tab(tab, false, false, window, cx),
            Shortcut::SaveAs => self.save_tab(tab, true, false, window, cx),
            Shortcut::Palette => self.open_palette(window, cx),
            Shortcut::Settings => self.open_settings(window, cx),
            Shortcut::Export => tab.update(cx, |t, cx| t.export_current(window, cx)),
            Shortcut::ImportData => {
                if let Some(c) = tab.read(cx).conn_id.clone() {
                    self.open_import(c, String::new(), None, None, window, cx)
                }
            }
            Shortcut::ToggleSchema => self.toggle_right_panel(RightPanel::Schema, cx),
            Shortcut::History => self.open_history(window, cx),
            Shortcut::LockAllProd => self.lock_all_prod(cx),
            Shortcut::Format => self.format_sql(window, cx),
            Shortcut::Find => self.find_replace(false, window, cx),
            Shortcut::Replace => self.find_replace(true, window, cx),
            Shortcut::Explain => self.explain(false, window, cx),
            Shortcut::ExplainAnalyze => self.explain(true, window, cx),
            Shortcut::OpenDiagram => self.open_current_diagram(window, cx),
            Shortcut::BindFolder => self.bind_folder(window, cx),
            Shortcut::UnbindFolder => self.set_folder(None, cx),
            Shortcut::ToggleZoom => self.toggle_zoom(cx),
            Shortcut::NewSplit => self.new_split(window, cx),
            Shortcut::Unsplit => self.unsplit_current(window, cx),
            Shortcut::TxCommit => self.tx_end_active(true, window, cx),
            Shortcut::TxRollback => self.tx_end_active(false, window, cx),
            Shortcut::TxMode => self.tx_toggle_mode(window, cx),
            Shortcut::SaveQuery => self.save_query_dialog(window, cx),
            Shortcut::OpenSaved => self.open_saved(window, cx),
            Shortcut::ShowVariables => self.show_variables(window, cx),
        }
        cx.notify();
        true
    }
}

/// Every key binding for the Settings tab's read-only Shortcuts list: the
/// palette's commands that have keys, then the bindings that have no palette
/// entry. Read from the live keymap (user keymap included).
pub(crate) fn listing(cx: &App) -> Vec<(String, String)> {
    use crate::shell::keymap::{act, command_keys, keys_text};
    let mut v: Vec<(String, String)> = crate::palette::ACTIONS
        .iter()
        .map(|(s, k)| (t!(*k).into_owned(), command_keys(*s, cx)))
        .filter(|(_, keys)| !keys.is_empty())
        .collect();
    v.push((
        t!("sp.k_palette").into_owned(),
        command_keys(Shortcut::Palette, cx),
    ));
    v.push((
        t!("sp.k_space").into_owned(),
        "Ctrl+1…9 / Ctrl+Shift+1…9".into(),
    ));
    v.push((
        t!("sp.k_copy").into_owned(),
        keys_text(&[&act::GridCopy], cx),
    ));
    v
}
