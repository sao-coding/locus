use db_core::{ColumnInfo, ForeignKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const HEADER_H: f32 = 30.0;
pub const ROW_H: f32 = 22.0;
pub const CARD_PAD_BOTTOM: f32 = 6.0;
const MIN_W: f32 = 170.0;
/// G1: card width cap (was 380); longer names are truncated.
const MAX_W: f32 = 300.0;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub primary_key: bool,
    /// Default expression, once the host supplied the table's structure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// An index of a table (columns in index order).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableNode {
    pub schema: String,
    pub name: String,
    pub columns: Vec<Column>,
    /// Indexes; empty until the host supplies them (`ErDiagram::set_table_details`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<Index>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Indexes (and comments) are known: set by `apply_detail`. Until then
    /// no foreign key is reported as lacking an index.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_details: bool,
}

/// What the host can add to a table after the first load (from the driver's
/// table structure): indexes, the table comment, column defaults / comments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableDetail {
    pub indexes: Vec<Index>,
    pub comment: Option<String>,
    /// (column, default, comment)
    pub columns: Vec<(String, Option<String>, Option<String>)>,
}

impl TableNode {
    /// Merge a [`TableDetail`] into the table.
    pub fn apply_detail(&mut self, d: &TableDetail) {
        self.indexes = d.indexes.clone();
        self.has_details = true;
        self.comment = d.comment.clone().filter(|c| !c.is_empty());
        for (name, def, com) in &d.columns {
            if let Some(c) = self.columns.iter_mut().find(|c| &c.name == name) {
                c.default = def.clone();
                c.comment = com.clone().filter(|c| !c.is_empty());
            }
        }
    }

    fn pk_columns(&self) -> Vec<&str> {
        self.columns
            .iter()
            .filter(|c| c.primary_key)
            .map(|c| c.name.as_str())
            .collect()
    }

    /// `cols` (as a set) is the primary key or the columns of a unique index.
    pub fn is_unique_set(&self, cols: &[String]) -> bool {
        use std::collections::BTreeSet;
        if cols.is_empty() {
            return false;
        }
        let want: BTreeSet<&str> = cols.iter().map(|s| s.as_str()).collect();
        if self.pk_columns().into_iter().collect::<BTreeSet<_>>() == want {
            return true;
        }
        self.indexes.iter().any(|i| {
            (i.unique || i.primary)
                && i.columns
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<BTreeSet<_>>()
                    == want
        })
    }

    /// Column `name` is in the primary key or in some index.
    pub fn in_any_index(&self, name: &str) -> bool {
        self.indexes
            .iter()
            .any(|i| i.columns.iter().any(|c| c == name))
    }

    /// Some index (or the primary key) starts with `cols`, in any order of
    /// those leading columns, so lookups by these columns can use it.
    pub fn has_index_on(&self, cols: &[String]) -> bool {
        use std::collections::BTreeSet;
        if cols.is_empty() {
            return true;
        }
        let want: BTreeSet<&str> = cols.iter().map(|s| s.as_str()).collect();
        let leads = |list: &[&str]| {
            list.len() >= want.len()
                && list[..want.len()].iter().copied().collect::<BTreeSet<_>>() == want
        };
        if leads(&self.pk_columns()) {
            return true;
        }
        self.indexes.iter().any(|i| {
            let l: Vec<&str> = i.columns.iter().map(|s| s.as_str()).collect();
            leads(&l)
        })
    }
}

/// End mark of a relation line (crow's-foot notation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// Crow's foot: many rows.
    Many,
    /// One bar: at most one row (the referencing side of a 1:1).
    One,
    /// Two bars: exactly one row (referenced side of a NOT NULL key).
    OnlyOne,
    /// Circle and bar: zero or one row (referenced side of a nullable key).
    ZeroOrOne,
}

/// (referencing end, referenced end) of `rel`, `from` being its referencing
/// table. The referenced side is "one": mandatory when every key column is
/// NOT NULL, optional when one is nullable. The referencing side is "many",
/// or "one" when its key columns are the primary key or a unique index (1:1).
pub fn cardinality(rel: &Relation, from: &TableNode) -> (End, End) {
    let nullable = rel.from_columns.iter().any(|c| {
        from.columns
            .iter()
            .find(|x| &x.name == c)
            .is_some_and(|x| x.nullable)
    });
    let to_end = if nullable {
        End::ZeroOrOne
    } else {
        End::OnlyOne
    };
    let from_end = if from.is_unique_set(&rel.from_columns) {
        End::One
    } else {
        End::Many
    };
    (from_end, to_end)
}

impl TableNode {
    /// Stable identifier used by relations, events and the saved layout.
    pub fn key(&self) -> String {
        table_key(&self.schema, &self.name)
    }
}

pub fn table_key(schema: &str, name: &str) -> String {
    format!("{schema}.{name}")
}

/// `from_table.from_columns` references `to_table.to_columns`. Tables are
/// given by [`TableNode::key`]. The "many" end is `from`, the "one" end `to`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relation {
    pub name: String,
    pub from_table: String,
    pub from_columns: Vec<String>,
    pub to_table: String,
    pub to_columns: Vec<String>,
    /// Not a declared key: guessed from column and table names (drawn dashed).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inferred: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagramModel {
    pub tables: Vec<TableNode>,
    pub relations: Vec<Relation>,
}

impl DiagramModel {
    /// Build a model for one schema from `Catalog` results. `tables` is
    /// (table name, columns); `fks` are the foreign keys of that schema.
    /// A key whose referencing or referenced table is not in `tables` (a view,
    /// another schema that was not loaded) is dropped.
    pub fn from_catalog(
        schema: &str,
        tables: &[(String, Vec<ColumnInfo>)],
        fks: &[ForeignKey],
    ) -> Self {
        let tables_out: Vec<TableNode> = tables
            .iter()
            .map(|(name, cols)| TableNode {
                schema: schema.to_string(),
                name: name.clone(),
                columns: cols
                    .iter()
                    .map(|c| Column {
                        name: c.name.clone(),
                        type_name: c.type_name.clone(),
                        nullable: c.nullable,
                        primary_key: c.primary_key,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            })
            .collect();
        let known: std::collections::HashSet<String> = tables_out.iter().map(|t| t.key()).collect();
        let relations = fks
            .iter()
            .filter_map(|fk| {
                let from = table_key(schema, &fk.table);
                let ref_schema = if fk.ref_schema.is_empty() {
                    schema
                } else {
                    &fk.ref_schema
                };
                let to = table_key(ref_schema, &fk.ref_table);
                (known.contains(&from) && known.contains(&to)).then(|| Relation {
                    name: fk.name.clone(),
                    from_table: from,
                    from_columns: fk.columns.clone(),
                    to_table: to,
                    to_columns: fk.ref_columns.clone(),
                    ..Default::default()
                })
            })
            .collect();
        Self {
            tables: tables_out,
            relations,
        }
    }
}

/// How far to follow foreign keys from the centre table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Depth {
    /// This many foreign-key hops (1 = direct neighbours).
    Hops(u32),
    /// Everything connected to the centre table.
    All,
}

/// Which tables of the model are shown.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    /// The whole schema.
    #[default]
    All,
    /// `table` (a table key) plus its neighbours within `depth` hops.
    Centre { table: String, depth: Depth },
    /// An explicit list of table keys (unknown keys are ignored).
    Tables(Vec<String>),
}

/// Tables of `scope` as `(key, hops from the centre)`. Only `Scope::Centre`
/// has distances (centre = 0); the others report `None`. Centre scope is in
/// breadth-first order (by hop, then by discovery: parents in that order,
/// their neighbours in model order), the others in model order.
/// A centre that is not in the model gives an empty list.
pub fn scoped_tables(model: &DiagramModel, scope: &Scope) -> Vec<(String, Option<u32>)> {
    match scope {
        Scope::All => model.tables.iter().map(|t| (t.key(), None)).collect(),
        Scope::Tables(list) => {
            let want: std::collections::HashSet<&str> = list.iter().map(|s| s.as_str()).collect();
            model
                .tables
                .iter()
                .filter(|t| want.contains(t.key().as_str()))
                .map(|t| (t.key(), None))
                .collect()
        }
        Scope::Centre { table, depth } => {
            let idx: std::collections::HashMap<String, usize> = model
                .tables
                .iter()
                .enumerate()
                .map(|(i, t)| (t.key(), i))
                .collect();
            let Some(&c) = idx.get(table) else {
                return vec![];
            };
            let n = model.tables.len();
            let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
            for r in &model.relations {
                if let (Some(&a), Some(&b)) = (idx.get(&r.from_table), idx.get(&r.to_table))
                    && a != b
                {
                    adj[a].push(b);
                    adj[b].push(a);
                }
            }
            for l in &mut adj {
                l.sort_unstable();
                l.dedup();
            }
            let limit = match depth {
                Depth::Hops(h) => *h,
                Depth::All => u32::MAX,
            };
            let mut dist: Vec<Option<u32>> = vec![None; n];
            dist[c] = Some(0);
            let mut order = vec![c];
            let mut head = 0;
            while head < order.len() {
                let a = order[head];
                head += 1;
                let d = dist[a].unwrap();
                if d >= limit {
                    continue;
                }
                for &b in &adj[a] {
                    if dist[b].is_none() {
                        dist[b] = Some(d + 1);
                        order.push(b);
                    }
                }
            }
            order
                .into_iter()
                .map(|i| (model.tables[i].key(), dist[i]))
                .collect()
        }
    }
}

/// Keys of the tables in `scope` (see [`scoped_tables`]).
pub fn tables_in_scope(model: &DiagramModel, scope: &Scope) -> Vec<String> {
    scoped_tables(model, scope)
        .into_iter()
        .map(|(k, _)| k)
        .collect()
}

fn bare(key: &str) -> &str {
    key.split_once('.').map_or(key, |(_, n)| n)
}

/// `JOIN <parent> ON <parent>.<col> = <child>.<col>` for a foreign key
/// (`to_table` is the parent, `from_table` the child). Table names are
/// written without the schema; composite keys are joined with `AND`.
pub fn join_clause(fk: &Relation) -> String {
    join_clause_with(fk, |s| s.to_string())
}

/// Like [`join_clause`], with every table / column name passed through `quote`.
pub fn join_clause_with(fk: &Relation, quote: impl Fn(&str) -> String) -> String {
    format!(
        "JOIN {} ON {}",
        quote(bare(&fk.to_table)),
        on_clause(fk, &quote)
    )
}

/// `parent.pk = child.fk AND ...` of a key, names passed through `quote`.
fn on_clause(fk: &Relation, quote: &impl Fn(&str) -> String) -> String {
    let (parent, child) = (quote(bare(&fk.to_table)), quote(bare(&fk.from_table)));
    let conds: Vec<String> = fk
        .to_columns
        .iter()
        .zip(&fk.from_columns)
        .map(|(p, c)| format!("{parent}.{} = {child}.{}", quote(p), quote(c)))
        .collect();
    conds.join(" AND ")
}

/// A way to join a set of tables: the tables in join order (the first is
/// the `FROM` table) and the relation (index into `DiagramModel::relations`)
/// that brings in each following table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinPath {
    pub tables: Vec<String>,
    pub relations: Vec<usize>,
}

/// Shortest ways to join `tables` (keys, at least two) through declared
/// foreign keys, intermediate tables allowed. Each further table is reached
/// from the tables joined so far by a shortest path; when several paths of
/// the same length exist, every combination is returned (at most `max`), so
/// the caller can let the user choose. Empty when some table cannot be
/// reached or fewer than two tables are given.
pub fn join_paths(model: &DiagramModel, tables: &[String], max: usize) -> Vec<JoinPath> {
    use std::collections::HashMap;
    if tables.len() < 2 || max == 0 {
        return vec![];
    }
    let key_ix: HashMap<String, usize> = model
        .tables
        .iter()
        .enumerate()
        .map(|(i, t)| (t.key(), i))
        .collect();
    let n = model.tables.len();
    // adjacency: (neighbour, relation)
    let mut adj: Vec<Vec<(usize, usize)>> = vec![vec![]; n];
    for (ri, r) in model.relations.iter().enumerate() {
        if r.inferred {
            continue;
        }
        if let (Some(&a), Some(&b)) = (key_ix.get(&r.from_table), key_ix.get(&r.to_table))
            && a != b
        {
            adj[a].push((b, ri));
            adj[b].push((a, ri));
        }
    }
    let Some(targets) = tables
        .iter()
        .map(|k| key_ix.get(k).copied())
        .collect::<Option<Vec<usize>>>()
    else {
        return vec![];
    };
    // partial results: (joined tables in order, relations)
    let mut partial: Vec<(Vec<usize>, Vec<usize>)> = vec![(vec![targets[0]], vec![])];
    for &t in &targets[1..] {
        let mut next = vec![];
        for (joined, rels) in &partial {
            if joined.contains(&t) {
                next.push((joined.clone(), rels.clone()));
                continue;
            }
            // multi-source BFS from the joined set; preds keep every shortest way in
            let mut dist = vec![usize::MAX; n];
            let mut preds: Vec<Vec<(usize, usize)>> = vec![vec![]; n];
            let mut queue = std::collections::VecDeque::new();
            for &s in joined {
                dist[s] = 0;
                queue.push_back(s);
            }
            while let Some(u) = queue.pop_front() {
                if u == t {
                    continue;
                }
                for &(v, ri) in &adj[u] {
                    if dist[v] == usize::MAX {
                        dist[v] = dist[u] + 1;
                        queue.push_back(v);
                    }
                    if dist[v] == dist[u] + 1 && !preds[v].contains(&(u, ri)) {
                        preds[v].push((u, ri));
                    }
                }
            }
            if dist[t] == usize::MAX {
                return vec![];
            }
            // walk back every shortest way from t to the joined set
            let mut ways: Vec<Vec<(usize, usize)>> = vec![];
            let mut stack: Vec<(usize, Vec<(usize, usize)>)> = vec![(t, vec![])];
            while let Some((v, way)) = stack.pop() {
                if dist[v] == 0 {
                    ways.push(way);
                    continue;
                }
                if ways.len() >= max {
                    break;
                }
                for &(u, ri) in &preds[v] {
                    let mut w = way.clone();
                    w.push((v, ri));
                    stack.push((u, w));
                }
            }
            for way in ways {
                let (mut j, mut r) = (joined.clone(), rels.clone());
                // `way` runs from t back to the joined set: join in reverse
                for &(v, ri) in way.iter().rev() {
                    j.push(v);
                    r.push(ri);
                }
                next.push((j, r));
            }
        }
        next.sort();
        next.dedup();
        next.truncate(max);
        partial = next;
    }
    partial
        .into_iter()
        .map(|(j, r)| JoinPath {
            tables: j.iter().map(|&i| model.tables[i].key()).collect(),
            relations: r,
        })
        .collect()
}

/// `SELECT *\nFROM a\nJOIN b ON ...\nJOIN c ON ...` for a join path; names
/// passed through `quote` (as [`join_clause_with`]).
pub fn join_sql(model: &DiagramModel, path: &JoinPath, quote: impl Fn(&str) -> String) -> String {
    let Some(first) = path.tables.first() else {
        return String::new();
    };
    let mut out = format!("SELECT *\nFROM {}", quote(bare(first)));
    for (t, &ri) in path.tables[1..].iter().zip(&path.relations) {
        let r = &model.relations[ri];
        out.push_str(&format!(
            "\nJOIN {} ON {}",
            quote(bare(t)),
            on_clause(r, &quote)
        ));
    }
    out
}

/// `a → b → c` for a chooser.
pub fn join_path_text(path: &JoinPath) -> String {
    path.tables
        .iter()
        .map(|t| bare(t))
        .collect::<Vec<_>>()
        .join(" → ")
}

/// Declared foreign keys (indexes into `relations`) of tables whose indexes
/// are known, where no index (nor the primary key) starts with the key
/// columns: lookups from the parent and deletes on it scan the child table.
pub fn fk_without_index(model: &DiagramModel) -> Vec<usize> {
    model
        .relations
        .iter()
        .enumerate()
        .filter(|(_, r)| !r.inferred)
        .filter(|(_, r)| {
            model
                .tables
                .iter()
                .find(|t| t.key() == r.from_table)
                .is_some_and(|t| t.has_details && !t.has_index_on(&r.from_columns))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Colour groups of `tables`: by schema when there are several, else by the
/// name prefix before the first `_` that at least two tables share
/// (`booking_*`, `sys_*`). Returns the group names (sorted) and each
/// table's group; tables in no group get `None`.
pub fn color_groups(tables: &[&TableNode]) -> (Vec<String>, Vec<Option<usize>>) {
    use std::collections::BTreeMap;
    let schemas: std::collections::BTreeSet<&str> =
        tables.iter().map(|t| t.schema.as_str()).collect();
    let label = |t: &TableNode| -> Option<String> {
        if schemas.len() > 1 {
            Some(t.schema.clone())
        } else {
            t.name
                .split_once('_')
                .map(|(p, _)| p.to_lowercase())
                .filter(|p| !p.is_empty())
        }
    };
    let labels: Vec<Option<String>> = tables.iter().map(|t| label(t)).collect();
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    for l in labels.iter().flatten() {
        *count.entry(l.as_str()).or_default() += 1;
    }
    let names: Vec<String> = count
        .iter()
        .filter(|(_, n)| schemas.len() > 1 || **n >= 2)
        .map(|(k, _)| k.to_string())
        .collect();
    let of = labels
        .iter()
        .map(|l| l.as_ref().and_then(|l| names.iter().position(|n| n == l)))
        .collect();
    (names, of)
}

impl DiagramModel {
    /// Add relations guessed from names (e.g. from `locus_core::joinsug`).
    /// They are flagged `inferred`; one that duplicates a relation already in
    /// the model (same table and columns) or names an unknown table is skipped.
    pub fn add_inferred(&mut self, rels: Vec<Relation>) {
        let known: std::collections::HashSet<String> =
            self.tables.iter().map(|t| t.key()).collect();
        for mut r in rels {
            let dup = self
                .relations
                .iter()
                .any(|x| x.from_table == r.from_table && x.from_columns == r.from_columns);
            if dup || !known.contains(&r.from_table) || !known.contains(&r.to_table) {
                continue;
            }
            r.inferred = true;
            self.relations.push(r);
        }
    }
}

fn mermaid_name(s: &str) -> String {
    let plain = !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
    if plain {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('"', "'"))
    }
}

/// Mermaid `erDiagram` text: one `parent ||--o{ child : "fk_col"` line per
/// foreign key whose two tables are both in `tables` (table keys). Tables
/// without relations are listed as bare entities so they are not lost.
pub fn to_mermaid(model: &DiagramModel, tables: &[String]) -> String {
    let set: std::collections::HashSet<&str> = tables.iter().map(|s| s.as_str()).collect();
    let mut out = String::from("erDiagram\n");
    let mut linked: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for r in &model.relations {
        if r.inferred {
            continue;
        }
        if set.contains(r.from_table.as_str()) && set.contains(r.to_table.as_str()) {
            linked.insert(&r.from_table);
            linked.insert(&r.to_table);
            let (from_end, to_end) = model
                .tables
                .iter()
                .find(|t| t.key() == r.from_table)
                .map_or((End::Many, End::OnlyOne), |t| cardinality(r, t));
            let left = if to_end == End::ZeroOrOne { "|o" } else { "||" };
            let right = if from_end == End::One { "o|" } else { "o{" };
            out.push_str(&format!(
                "  {} {left}--{right} {} : \"{}\"\n",
                mermaid_name(bare(&r.to_table)),
                mermaid_name(bare(&r.from_table)),
                r.from_columns.join(", ").replace('"', "'")
            ));
        }
    }
    for t in tables {
        if !linked.contains(t.as_str()) && model.tables.iter().any(|x| &x.key() == t) {
            out.push_str(&format!("  {}\n", mermaid_name(bare(t))));
        }
    }
    out
}

/// How much of each card is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Density {
    /// Every column.
    #[default]
    Full,
    /// Primary-key, foreign-key and indexed columns.
    Keys,
    /// The header only.
    Header,
}

impl Density {
    /// Is column `c` of `t` (`fk`: it is a referencing column) shown?
    pub fn shows(self, t: &TableNode, c: usize, fk: bool) -> bool {
        match self {
            Density::Full => true,
            Density::Header => false,
            Density::Keys => {
                let col = &t.columns[c];
                col.primary_key || fk || t.in_any_index(&col.name)
            }
        }
    }
}

/// Table position by table key, plus the camera. Serializable so the host can
/// store it per schema. `zoom` / `pan` of `None` mean "fit to view".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DiagramLayout {
    pub tables: BTreeMap<String, [f32; 2]>,
    #[serde(default)]
    pub zoom: Option<f32>,
    #[serde(default)]
    pub pan: Option<[f32; 2]>,
}

/// A column row's name and type cut to fit a card `w` wide (world units) when both
/// do not fit: the name gives way first, never below half the room. Uses the
/// same per-character estimate as [`card_size`].
pub fn fit_row(name: &str, ty: &str, w: f32) -> (String, String) {
    const GAP: f32 = 10.0;
    let avail = (w - 47.0).max(0.0);
    let (nw, tw) = (text_units(name) * 7.0, text_units(ty) * 6.4);
    if nw + GAP + tw <= avail {
        return (name.to_string(), ty.to_string());
    }
    let name_budget = (avail - GAP - tw).max(avail * 0.5);
    let name = cut(name, name_budget, 7.0);
    let ty_budget = (avail - GAP - text_units(&name) * 7.0).max(0.0);
    (name, cut(ty, ty_budget, 6.4))
}

fn cut(s: &str, budget: f32, per_unit: f32) -> String {
    if text_units(s) * per_unit <= budget {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = per_unit; // the ellipsis
    for ch in s.chars() {
        used += text_units(ch.encode_utf8(&mut [0; 4])) * per_unit;
        if used > budget {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

fn text_units(s: &str) -> f32 {
    s.chars()
        .map(|c| if (c as u32) >= 0x2E80 { 1.9 } else { 1.0 })
        .sum()
}

/// Card size in world units. A heuristic (no font access), so the automatic
/// layout can run on any thread.
pub fn card_size(t: &TableNode) -> (f32, f32) {
    let name_w = t
        .columns
        .iter()
        .map(|c| text_units(&c.name))
        .fold(0.0, f32::max);
    let type_w = t
        .columns
        .iter()
        .map(|c| text_units(&c.type_name) + 1.0)
        .fold(0.0, f32::max);
    let head_w = text_units(&t.name) * 8.2 + 52.0; // icon, row count, centre tag
    let w = (36.0 + name_w * 7.0 + 18.0 + type_w * 6.4 + 12.0)
        .max(head_w + 28.0)
        .clamp(MIN_W, MAX_W);
    let h = HEADER_H + t.columns.len().max(1) as f32 * ROW_H + CARD_PAD_BOTTOM;
    (w.ceil(), h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_rows_are_cut_so_name_and_type_never_overlap() {
        assert_eq!(
            fit_row("id", "bigint", 170.0),
            ("id".into(), "bigint".into())
        );
        let (n, t) = fit_row(
            "a_very_long_column_name_that_keeps_going",
            "timestamp with time zone",
            200.0,
        );
        assert!(n.ends_with('…'));
        assert!(text_units(&n) * 7.0 + 10.0 + text_units(&t) * 6.4 <= 200.0 - 47.0 + 0.01);
    }

    fn ci(n: &str, pk: bool) -> ColumnInfo {
        ColumnInfo {
            name: n.into(),
            type_name: "int".into(),
            nullable: !pk,
            primary_key: pk,
        }
    }

    #[test]
    fn from_catalog_maps_tables_and_keeps_only_known_fks() {
        let tables = vec![
            ("a".to_string(), vec![ci("id", true)]),
            ("b".to_string(), vec![ci("id", true), ci("a_id", false)]),
        ];
        let fk = |t: &str, rt: &str, rs: &str| ForeignKey {
            name: format!("fk_{t}_{rt}"),
            table: t.into(),
            columns: vec!["a_id".into()],
            ref_schema: rs.into(),
            ref_table: rt.into(),
            ref_columns: vec!["id".into()],
        };
        let fks = vec![
            fk("b", "a", "s"),
            fk("b", "missing", "s"),
            fk("b", "a", "other"),
        ];
        let m = DiagramModel::from_catalog("s", &tables, &fks);
        assert_eq!(m.tables.len(), 2);
        assert_eq!(m.tables[1].key(), "s.b");
        assert!(m.tables[0].columns[0].primary_key && !m.tables[1].columns[1].primary_key);
        assert!(m.tables[1].columns[1].nullable);
        assert_eq!(m.relations.len(), 1);
        let r = &m.relations[0];
        assert_eq!((r.from_table.as_str(), r.to_table.as_str()), ("s.b", "s.a"));
        assert_eq!(
            (r.from_columns.clone(), r.to_columns.clone()),
            (vec!["a_id".to_string()], vec!["id".to_string()])
        );
    }

    fn chain() -> DiagramModel {
        // d -> c -> b -> a (child -> parent), e -> a, x alone
        let t = |n: &str| TableNode {
            schema: "s".into(),
            name: n.into(),
            ..Default::default()
        };
        let r = |f: &str, to: &str| Relation {
            name: format!("fk_{f}"),
            from_table: format!("s.{f}"),
            from_columns: vec![format!("{to}_id")],
            to_table: format!("s.{to}"),
            to_columns: vec!["id".into()],
            ..Default::default()
        };
        DiagramModel {
            tables: ["a", "b", "c", "d", "x", "e"]
                .iter()
                .map(|n| t(n))
                .collect(),
            relations: vec![r("b", "a"), r("c", "b"), r("d", "c"), r("e", "a")],
        }
    }

    fn keys(v: Vec<String>) -> Vec<String> {
        v.into_iter()
            .map(|k| k.trim_start_matches("s.").to_string())
            .collect()
    }

    #[test]
    fn scope_all_and_list() {
        let m = chain();
        assert_eq!(tables_in_scope(&m, &Scope::All).len(), 6);
        let l = Scope::Tables(vec!["s.d".into(), "s.a".into(), "s.nope".into()]);
        assert_eq!(keys(tables_in_scope(&m, &l)), ["a", "d"]);
    }

    #[test]
    fn scope_centre_hops() {
        let m = chain();
        let c = |h| Scope::Centre {
            table: "s.b".into(),
            depth: h,
        };
        assert_eq!(keys(tables_in_scope(&m, &c(Depth::Hops(0)))), ["b"]);
        assert_eq!(
            keys(tables_in_scope(&m, &c(Depth::Hops(1)))),
            ["b", "a", "c"]
        );
        let two = scoped_tables(&m, &c(Depth::Hops(2)));
        let two: Vec<_> = two.iter().map(|(k, d)| (k.as_str(), *d)).collect();
        assert_eq!(
            two,
            [
                ("s.b", Some(0)),
                ("s.a", Some(1)),
                ("s.c", Some(1)),
                ("s.e", Some(2)),
                ("s.d", Some(2))
            ]
        );
        // "all connected" excludes the unrelated table
        let all = keys(tables_in_scope(&m, &c(Depth::All)));
        assert_eq!(all.len(), 5);
        assert!(!all.contains(&"x".to_string()));
        // unknown centre
        assert!(
            tables_in_scope(
                &m,
                &Scope::Centre {
                    table: "s.zz".into(),
                    depth: Depth::All
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn join_clause_single_and_composite() {
        let m = chain();
        assert_eq!(join_clause(&m.relations[0]), "JOIN a ON a.id = b.a_id");
        let r = Relation {
            name: "fk".into(),
            from_table: "s.line".into(),
            from_columns: vec!["order_id".into(), "tenant".into()],
            to_table: "s.orders".into(),
            to_columns: vec!["id".into(), "tenant".into()],
            ..Default::default()
        };
        assert_eq!(
            join_clause(&r),
            "JOIN orders ON orders.id = line.order_id AND orders.tenant = line.tenant"
        );
        assert_eq!(
            join_clause_with(&r, |s| format!("`{s}`")),
            "JOIN `orders` ON `orders`.`id` = `line`.`order_id` AND `orders`.`tenant` = `line`.`tenant`"
        );
    }

    #[test]
    fn mermaid_lists_fks_in_scope_only() {
        let m = chain();
        let all = tables_in_scope(&m, &Scope::All);
        let s = to_mermaid(&m, &all);
        assert!(s.starts_with("erDiagram\n"));
        assert!(s.contains("  a ||--o{ b : \"a_id\"\n"));
        assert_eq!(s.matches("||--o{").count(), 4);
        assert!(s.contains("\n  x\n"), "{s}");
        let part = to_mermaid(&m, &["s.a".into(), "s.b".into()]);
        assert_eq!(part, "erDiagram\n  a ||--o{ b : \"a_id\"\n");
    }

    fn col(n: &str, pk: bool, nullable: bool) -> Column {
        Column {
            name: n.into(),
            type_name: "int".into(),
            nullable,
            primary_key: pk,
            ..Default::default()
        }
    }

    fn rel(from: &str, cols: &[&str]) -> Relation {
        Relation {
            name: "fk".into(),
            from_table: format!("s.{from}"),
            from_columns: cols.iter().map(|s| s.to_string()).collect(),
            to_table: "s.p".into(),
            to_columns: vec!["id".into()],
            ..Default::default()
        }
    }

    #[test]
    fn cardinality_from_nullability_and_uniqueness() {
        let mut child = TableNode {
            schema: "s".into(),
            name: "c".into(),
            columns: vec![
                col("id", true, false),
                col("p_id", false, false),
                col("q_id", false, true),
            ],
            ..Default::default()
        };
        // NOT NULL key: exactly one parent; many children
        assert_eq!(
            cardinality(&rel("c", &["p_id"]), &child),
            (End::Many, End::OnlyOne)
        );
        // nullable key: zero or one parent
        assert_eq!(
            cardinality(&rel("c", &["q_id"]), &child),
            (End::Many, End::ZeroOrOne)
        );
        // the key is the primary key: 1:1
        assert_eq!(
            cardinality(&rel("c", &["id"]), &child),
            (End::One, End::OnlyOne)
        );
        // a unique index on the key: 1:1; a plain index changes nothing
        child.indexes = vec![
            Index {
                name: "ix_q".into(),
                columns: vec!["q_id".into()],
                ..Default::default()
            },
            Index {
                name: "uq_p".into(),
                columns: vec!["p_id".into()],
                unique: true,
                primary: false,
            },
        ];
        assert_eq!(cardinality(&rel("c", &["p_id"]), &child).0, End::One);
        assert_eq!(cardinality(&rel("c", &["q_id"]), &child).0, End::Many);
        // composite: unique only as the whole set, in any order
        child.indexes.push(Index {
            name: "uq_pq".into(),
            columns: vec!["q_id".into(), "p_id".into()],
            unique: true,
            primary: false,
        });
        assert!(child.is_unique_set(&["p_id".into(), "q_id".into()]));
        assert!(!child.is_unique_set(&["q_id".into(), "id".into()]));
        // the mermaid text follows the marks
        let m = DiagramModel {
            tables: vec![
                TableNode {
                    schema: "s".into(),
                    name: "p".into(),
                    columns: vec![col("id", true, false)],
                    ..Default::default()
                },
                child.clone(),
            ],
            relations: vec![rel("c", &["q_id"]), rel("c", &["p_id"])],
        };
        let s = to_mermaid(&m, &["s.p".into(), "s.c".into()]);
        assert!(s.contains("  p |o--o{ c : \"q_id\"\n"), "{s}");
        assert!(s.contains("  p ||--o| c : \"p_id\"\n"), "{s}");
    }

    #[test]
    fn density_picks_columns() {
        let mut t = TableNode {
            schema: "s".into(),
            name: "t".into(),
            columns: vec![
                col("id", true, false),
                col("p_id", false, false),
                col("code", false, false),
                col("note", false, true),
            ],
            ..Default::default()
        };
        t.indexes.push(Index {
            name: "ix_code".into(),
            columns: vec!["code".into()],
            ..Default::default()
        });
        let shown = |d: Density| -> Vec<usize> {
            (0..4)
                .filter(|&c| d.shows(&t, c, c == 1))
                .collect::<Vec<_>>()
        };
        assert_eq!(shown(Density::Full), [0, 1, 2, 3]);
        assert_eq!(shown(Density::Keys), [0, 1, 2]);
        assert!(shown(Density::Header).is_empty());
        // index detection: an index must start with the key columns
        assert!(t.has_index_on(&["code".into()]));
        assert!(t.has_index_on(&["id".into()]), "the primary key counts");
        assert!(!t.has_index_on(&["p_id".into()]));
        t.indexes.push(Index {
            name: "ix_note_p".into(),
            columns: vec!["note".into(), "p_id".into()],
            ..Default::default()
        });
        assert!(!t.has_index_on(&["p_id".into()]), "p_id is not leading");
        assert!(t.has_index_on(&["p_id".into(), "note".into()]));
    }

    #[test]
    fn details_merge_into_the_table() {
        let mut t = TableNode {
            schema: "s".into(),
            name: "t".into(),
            columns: vec![col("id", true, false), col("n", false, true)],
            ..Default::default()
        };
        t.apply_detail(&TableDetail {
            indexes: vec![Index {
                name: "PRIMARY".into(),
                columns: vec!["id".into()],
                unique: true,
                primary: true,
            }],
            comment: Some(String::new()),
            columns: vec![
                ("n".into(), Some("0".into()), Some("a note".into())),
                ("gone".into(), None, None),
            ],
        });
        assert_eq!(t.indexes.len(), 1);
        assert_eq!(t.comment, None, "empty comment is no comment");
        assert_eq!(t.columns[1].default.as_deref(), Some("0"));
        assert_eq!(t.columns[1].comment.as_deref(), Some("a note"));
    }

    #[test]
    fn layout_roundtrips_through_json() {
        let mut l = DiagramLayout::default();
        l.tables.insert("s.a".into(), [10.0, 20.5]);
        l.zoom = Some(0.75);
        let s = serde_json::to_string(&l).unwrap();
        assert_eq!(serde_json::from_str::<DiagramLayout>(&s).unwrap(), l);
        // older saves without camera fields still load
        let old: DiagramLayout = serde_json::from_str(r#"{"tables":{"s.a":[1,2]}}"#).unwrap();
        assert!(old.zoom.is_none() && old.pan.is_none());
    }

    /// a <- b <- c, a <- d <- c (two equal ways from a to c), e alone,
    /// f -> a; table names are bare in `tables`.
    fn graph() -> DiagramModel {
        let t = |n: &str| TableNode {
            schema: "s".into(),
            name: n.into(),
            columns: vec![col("id", true, false)],
            ..Default::default()
        };
        let r = |name: &str, f: &str, fc: &str, to: &str| Relation {
            name: name.into(),
            from_table: format!("s.{f}"),
            from_columns: vec![fc.into()],
            to_table: format!("s.{to}"),
            to_columns: vec!["id".into()],
            ..Default::default()
        };
        DiagramModel {
            tables: ["a", "b", "c", "d", "e", "f"]
                .iter()
                .map(|n| t(n))
                .collect(),
            relations: vec![
                r("fk_b_a", "b", "a_id", "a"),
                r("fk_c_b", "c", "b_id", "b"),
                r("fk_d_a", "d", "a_id", "a"),
                r("fk_c_d", "c", "d_id", "d"),
                r("fk_f_a", "f", "a_id", "a"),
            ],
        }
    }

    fn keys_of(p: &JoinPath) -> Vec<&str> {
        p.tables.iter().map(|t| bare(t)).collect()
    }

    #[test]
    fn join_path_direct_and_through_intermediates() {
        let m = graph();
        let p = join_paths(&m, &["s.b".into(), "s.a".into()], 8);
        assert_eq!(p.len(), 1);
        assert_eq!(keys_of(&p[0]), ["b", "a"]);
        assert_eq!(p[0].relations, [0]);
        // f to b goes through a
        let p = join_paths(&m, &["s.f".into(), "s.b".into()], 8);
        assert_eq!(p.len(), 1);
        assert_eq!(keys_of(&p[0]), ["f", "a", "b"]);
        assert_eq!(join_path_text(&p[0]), "f → a → b");
        // three tables: the third joins onto what is joined already
        let p = join_paths(&m, &["s.f".into(), "s.a".into(), "s.d".into()], 8);
        assert_eq!(p.len(), 1);
        assert_eq!(keys_of(&p[0]), ["f", "a", "d"]);
    }

    #[test]
    fn join_path_lists_ties_and_fails_cleanly() {
        let m = graph();
        // a to c: through b or through d
        let p = join_paths(&m, &["s.a".into(), "s.c".into()], 8);
        let mut ways: Vec<Vec<&str>> = p.iter().map(keys_of).collect();
        ways.sort();
        assert_eq!(ways, [vec!["a", "b", "c"], vec!["a", "d", "c"]]);
        assert_eq!(join_paths(&m, &["s.a".into(), "s.c".into()], 1).len(), 1);
        // unreachable, unknown, too few
        assert!(join_paths(&m, &["s.a".into(), "s.e".into()], 8).is_empty());
        assert!(join_paths(&m, &["s.a".into(), "s.zz".into()], 8).is_empty());
        assert!(join_paths(&m, &["s.a".into()], 8).is_empty());
        // inferred relations are not used
        let mut m2 = graph();
        m2.relations.push(Relation {
            name: String::new(),
            from_table: "s.e".into(),
            from_columns: vec!["a_id".into()],
            to_table: "s.a".into(),
            to_columns: vec!["id".into()],
            inferred: true,
        });
        assert!(join_paths(&m2, &["s.a".into(), "s.e".into()], 8).is_empty());
    }

    #[test]
    fn join_sql_joins_each_table_once_in_either_direction() {
        let m = graph();
        let p = &join_paths(&m, &["s.f".into(), "s.b".into()], 8)[0];
        assert_eq!(
            join_sql(&m, p, |s| s.to_string()),
            "SELECT *\nFROM f\nJOIN a ON a.id = f.a_id\nJOIN b ON a.id = b.a_id"
        );
        assert_eq!(
            join_sql(&m, p, |s| format!("`{s}`")),
            "SELECT *\nFROM `f`\nJOIN `a` ON `a`.`id` = `f`.`a_id`\nJOIN `b` ON `a`.`id` = `b`.`a_id`"
        );
    }

    #[test]
    fn foreign_keys_without_an_index() {
        let mut m = graph();
        // nothing known yet: nothing reported
        assert!(fk_without_index(&m).is_empty());
        for t in &mut m.tables {
            t.apply_detail(&TableDetail::default());
        }
        // only primary keys: every key column lacks an index
        assert_eq!(fk_without_index(&m), [0, 1, 2, 3, 4]);
        m.tables[1].indexes.push(Index {
            name: "ix".into(),
            columns: vec!["a_id".into(), "x".into()],
            ..Default::default()
        });
        assert_eq!(
            fk_without_index(&m),
            [1, 2, 3, 4],
            "b.a_id leads an index now"
        );
    }

    #[test]
    fn colour_groups_by_prefix_or_schema() {
        let t = |s: &str, n: &str| TableNode {
            schema: s.into(),
            name: n.into(),
            ..Default::default()
        };
        let one = [
            t("s", "booking_room"),
            t("s", "booking_guest"),
            t("s", "sys_user"),
            t("s", "sys_role"),
            t("s", "orders"),
            t("s", "misc_x"),
        ];
        let refs: Vec<&TableNode> = one.iter().collect();
        let (names, of) = color_groups(&refs);
        assert_eq!(names, ["booking", "sys"]);
        assert_eq!(of, [Some(0), Some(0), Some(1), Some(1), None, None]);
        let two = [t("a", "x"), t("b", "y"), t("a", "z")];
        let refs: Vec<&TableNode> = two.iter().collect();
        assert_eq!(
            color_groups(&refs),
            (
                vec!["a".into(), "b".into()],
                vec![Some(0), Some(1), Some(0)]
            )
        );
    }

    #[test]
    fn inferred_relations_are_flagged_and_deduplicated() {
        let mut m = graph();
        let n = m.relations.len();
        let r = |f: &str, c: &str, to: &str| Relation {
            from_table: format!("s.{f}"),
            from_columns: vec![c.into()],
            to_table: format!("s.{to}"),
            to_columns: vec!["id".into()],
            ..Default::default()
        };
        m.add_inferred(vec![
            r("e", "a_id", "a"),
            r("b", "a_id", "a"),  // declared already
            r("e", "q_id", "zz"), // unknown table
        ]);
        assert_eq!(m.relations.len(), n + 1);
        assert!(m.relations[n].inferred);
        // not in the Mermaid text
        assert!(!to_mermaid(&m, &tables_in_scope(&m, &Scope::All)).contains(" e : "));
    }
}
