//! Saved column filters (`colfilters.json` in the data folder): the filters
//! and orders "Save filter settings" keeps for one query on one connection.
//! They are applied again when that same query is run.

use crate::colfilter::ColView;
use crate::settings;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub conn_id: String,
    /// The query as it was before filters (`db_core::sql::select_body`).
    pub sql: String,
    pub view: ColView,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    items: Vec<Saved>,
}

const NAME: &str = "colfilters.json";

fn read() -> Vec<Saved> {
    crate::datafile::load_named::<File>(NAME)
        .map(|f| f.items)
        .unwrap_or_default()
}

fn write(items: Vec<Saved>) -> std::io::Result<()> {
    let d = settings::app_dir().ok_or_else(|| std::io::Error::other("APPDATA is not set"))?;
    std::fs::create_dir_all(&d)?;
    settings::write_atomic(&d.join(NAME), &serde_json::to_vec_pretty(&File { items })?)
}

/// What is saved for `sql` on `conn_id`.
pub fn find(conn_id: &str, sql: &str) -> Option<ColView> {
    read()
        .into_iter()
        .find(|s| s.conn_id == conn_id && s.sql == sql)
        .map(|s| s.view)
}

/// Saves `view` for the query (replacing an earlier one); an empty view removes it.
pub fn put(conn_id: &str, sql: &str, view: &ColView) -> std::io::Result<()> {
    let mut items = read();
    items.retain(|s| !(s.conn_id == conn_id && s.sql == sql));
    if !view.is_empty() {
        items.push(Saved {
            conn_id: conn_id.into(),
            sql: sql.into(),
            view: view.clone(),
        });
    }
    write(items)
}
