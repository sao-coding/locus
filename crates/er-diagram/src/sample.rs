//! Deterministic generated schema for the demo, benches and tests.

use crate::{Column, DiagramModel, Index, Relation, TableNode};

const BASES: &[&str] = &[
    "customers",
    "orders",
    "order_items",
    "products",
    "categories",
    "suppliers",
    "invoices",
    "payments",
    "shipments",
    "addresses",
    "warehouses",
    "inventory",
    "employees",
    "departments",
    "projects",
    "tasks",
    "comments",
    "tags",
    "users",
    "roles",
    "permissions",
    "sessions",
    "audit_log",
    "carts",
    "cart_items",
    "coupons",
    "reviews",
    "wishlists",
    "returns",
    "refunds",
    "contracts",
    "vendors",
    "price_lists",
    "currencies",
    "countries",
    "regions",
    "stores",
    "shifts",
    "tickets",
    "attachments",
];
const SUFFIX: &[&str] = &[
    "", "archive", "history", "staging", "ext", "map", "meta", "stats",
];
const GENERIC: &[(&str, &str, bool)] = &[
    ("name", "varchar(120)", false),
    ("code", "varchar(32)", false),
    ("status", "tinyint", false),
    ("description", "text", true),
    ("amount", "decimal(12,2)", true),
    ("quantity", "int", false),
    ("created_at", "datetime", false),
    ("updated_at", "datetime", true),
    ("notes", "varchar(500)", true),
    ("email", "varchar(191)", true),
    ("is_active", "tinyint(1)", false),
];

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// `n` tables and about 1.5 `n` foreign keys; same input, same output.
pub fn generate(n: usize) -> DiagramModel {
    let mut rng = Lcg(0x9E3779B97F4A7C15 ^ n as u64);
    let names: Vec<String> = (0..n)
        .map(|i| {
            let s = SUFFIX[(i / BASES.len()) % SUFFIX.len()];
            let round = i / (BASES.len() * SUFFIX.len());
            let mut name = BASES[i % BASES.len()].to_string();
            if !s.is_empty() {
                name = format!("{name}_{s}");
            }
            if round > 0 {
                name = format!("{name}_{round}");
            }
            name
        })
        .collect();
    let mut tables = Vec::with_capacity(n);
    let mut relations = Vec::new();
    for i in 0..n {
        let mut cols = vec![Column {
            name: "id".into(),
            type_name: "bigint unsigned".into(),
            nullable: false,
            primary_key: true,
            ..Default::default()
        }];
        let extra = 2 + (rng.next() % 6) as usize;
        let mut used = std::collections::BTreeSet::new();
        while used.len() < extra {
            used.insert((rng.next() % GENERIC.len() as u64) as usize);
        }
        for g in used {
            let (name, ty, nullable) = GENERIC[g];
            cols.push(Column {
                name: name.into(),
                type_name: ty.into(),
                nullable,
                primary_key: false,
                ..Default::default()
            });
        }
        // foreign keys to earlier tables, biased towards low indices (hubs)
        if i > 0 {
            let k = if rng.next().is_multiple_of(4) {
                0
            } else {
                1 + rng.next().is_multiple_of(3) as usize + rng.next().is_multiple_of(4) as usize
            };
            let mut targets = vec![];
            for _ in 0..k {
                let r = (rng.next() % 1000) as f64 / 1000.0;
                let t = ((r * r) * i as f64) as usize;
                if !targets.contains(&t) {
                    targets.push(t);
                }
            }
            for t in targets {
                let col = format!("{}_id", names[t].trim_end_matches('s'));
                if cols.iter().any(|c| c.name == col) {
                    continue;
                }
                cols.push(Column {
                    name: col.clone(),
                    type_name: "bigint unsigned".into(),
                    nullable: rng.next().is_multiple_of(3),
                    primary_key: false,
                    ..Default::default()
                });
                relations.push(Relation {
                    name: format!("fk_{}_{}", names[i], col),
                    from_table: format!("demo.{}", names[i]),
                    from_columns: vec![col],
                    to_table: format!("demo.{}", names[t]),
                    to_columns: vec!["id".into()],
                });
            }
        }
        // Extras that do not draw from the generator (so the schema of the
        // benches stays the same): self references, 1:1 extension tables and
        // indexes on some foreign-key columns.
        let base = BASES[i % BASES.len()];
        if i < BASES.len() && matches!(base, "employees" | "categories" | "comments") {
            cols.push(Column {
                name: "parent_id".into(),
                type_name: "bigint unsigned".into(),
                nullable: true,
                ..Default::default()
            });
            relations.push(Relation {
                name: format!("fk_{}_parent", names[i]),
                from_table: format!("demo.{}", names[i]),
                from_columns: vec!["parent_id".into()],
                to_table: format!("demo.{}", names[i]),
                to_columns: vec!["id".into()],
            });
        }
        let mut indexes = vec![Index {
            name: "PRIMARY".into(),
            columns: vec!["id".into()],
            unique: true,
            primary: true,
        }];
        let fk_cols: Vec<String> = relations
            .iter()
            .filter(|r| r.from_table == format!("demo.{}", names[i]))
            .map(|r| r.from_columns[0].clone())
            .collect();
        for (k, c) in fk_cols.iter().enumerate() {
            let one_to_one = k == 0 && names[i].ends_with("_ext");
            if one_to_one || (i + k) % 3 != 0 {
                indexes.push(Index {
                    name: format!("ix_{}_{c}", names[i]),
                    columns: vec![c.clone()],
                    unique: one_to_one,
                    primary: false,
                });
            }
        }
        tables.push(TableNode {
            schema: "demo".into(),
            name: names[i].clone(),
            columns: cols,
            indexes,
            ..Default::default()
        });
    }
    DiagramModel { tables, relations }
}
