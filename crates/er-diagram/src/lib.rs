//! Relationship (ER) diagram view for GPUI: tables as cards, foreign keys as
//! routed lines. Plain data in (`DiagramModel`), events out (`ErDiagramEvent`);
//! the view never touches a database.

mod export;
mod layout;
mod model;
mod route;
pub mod sample;
mod view;

pub use export::{SvgCard, SvgColors, SvgEdge, SvgRow, SvgScene, svg_to_png, to_svg};
pub use layout::{auto_layout, crossings, level_layout};
pub use model::{
    Column, Density, Depth, DiagramLayout, DiagramModel, End, Index, JoinPath, Relation, Scope,
    TableDetail, TableNode, card_size, cardinality, color_groups, fk_without_index, join_clause,
    join_clause_with, join_path_text, join_paths, join_sql, scoped_tables, tables_in_scope,
    to_mermaid,
};
pub use route::{EdgePath, EdgeStyle, Route, Seg, edge_path, route, slot_offset};
pub use view::{ErDiagram, ErDiagramEvent, JoinPathResult, Labels, MAX_ZOOM, MIN_ZOOM, Selection};
