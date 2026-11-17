//! State of a tab that shows a relationship diagram instead of an editor
//! (the methods are in `crate::diagram::tab`).

use crate::diagram::tab::DiagramState;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::*;
use std::sync::Arc;

pub struct DiagramPane {
    pub view: Option<Entity<er_diagram::ErDiagram>>,
    /// Scope and hidden tables; the view is the truth once it exists.
    pub state: DiagramState,
    pub(crate) driver: Option<Arc<locus_core::driver::AnyDriver>>,
    pub error: Option<String>,
    pub loading: bool,
    pub(crate) task: Option<Task<()>>,
    pub(crate) save: Option<Task<()>>,
    pub(crate) sub: Option<Subscription>,
    pub(crate) menu: Option<(Entity<PopupMenu>, Point<Pixels>, Subscription)>,
    /// Right click on a card, turned into a menu by the next render (it needs the window).
    pub(crate) menu_req: Option<(String, Point<Pixels>)>,
}
