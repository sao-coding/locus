//! Relationship (ER) diagram view for GPUI: tables as cards, foreign keys as
//! routed lines. Plain data in (`DiagramModel`), events out (`ErDiagramEvent`);
//! the view never touches a database.

mod layout;
mod model;
mod route;
pub mod sample;
mod view;

pub use layout::{auto_layout, level_layout};
pub use model::{
    Column, Depth, DiagramLayout, DiagramModel, Relation, Scope, TableNode, card_size, join_clause,
    join_clause_with, scoped_tables, tables_in_scope, to_mermaid,
};
pub use view::{ErDiagram, ErDiagramEvent, Labels, MAX_ZOOM, MIN_ZOOM, Selection};
