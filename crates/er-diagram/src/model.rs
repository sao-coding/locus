use db_core::{ColumnInfo, ForeignKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const HEADER_H: f32 = 30.0;
pub const ROW_H: f32 = 22.0;
pub const CARD_PAD_BOTTOM: f32 = 6.0;
const MIN_W: f32 = 170.0;
/// G1: card width cap (was 380); longer names are truncated.
const MAX_W: f32 = 300.0;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub primary_key: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableNode {
    pub schema: String,
    pub name: String,
    pub columns: Vec<Column>,
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relation {
    pub name: String,
    pub from_table: String,
    pub from_columns: Vec<String>,
    pub to_table: String,
    pub to_columns: Vec<String>,
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
                    })
                    .collect(),
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
    let (parent, child) = (quote(bare(&fk.to_table)), quote(bare(&fk.from_table)));
    let conds: Vec<String> = fk
        .to_columns
        .iter()
        .zip(&fk.from_columns)
        .map(|(p, c)| format!("{parent}.{} = {child}.{}", quote(p), quote(c)))
        .collect();
    format!("JOIN {parent} ON {}", conds.join(" AND "))
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
        if set.contains(r.from_table.as_str()) && set.contains(r.to_table.as_str()) {
            linked.insert(&r.from_table);
            linked.insert(&r.to_table);
            out.push_str(&format!(
                "  {} ||--o{{ {} : \"{}\"\n",
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
            columns: vec![],
        };
        let r = |f: &str, to: &str| Relation {
            name: format!("fk_{f}"),
            from_table: format!("s.{f}"),
            from_columns: vec![format!("{to}_id")],
            to_table: format!("s.{to}"),
            to_columns: vec!["id".into()],
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
}
