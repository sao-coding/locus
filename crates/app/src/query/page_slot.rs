//! History / Settings page shown in place of the editor (set by the shell).

use gpui_kit::*;

#[derive(Default)]
pub struct PageSlot {
    pub kind: Option<locus_core::spaces::TabPage>,
    pub view: Option<AnyView>,
    pub focus: Option<FocusHandle>,
}
