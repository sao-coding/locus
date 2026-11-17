//! Query plans: the statement each engine wants, and a parser that turns the
//! answer (MySQL / MariaDB JSON, PostgreSQL JSON, MySQL's `EXPLAIN ANALYZE`
//! text tree, or the plain tabular `EXPLAIN`) into an indented list of
//! operations with a share of cost / time / rows. Pure: no UI, no I/O.

use serde_json::Value as J;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Mysql,
    Postgres,
    /// `EXPLAIN QUERY PLAN`: a tree of rows (id, parent, detail), no cost numbers.
    Sqlite,
}

/// What the bar of a node is a share of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    /// The server's cost estimate.
    Cost,
    /// Measured time (`EXPLAIN ANALYZE`).
    Time,
    /// Estimated rows examined (the server gave no cost).
    Rows,
    None,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanNode {
    pub depth: usize,
    pub op: String,
    pub detail: String,
    /// 0..=100, share of the plan's total of `Plan::basis`.
    pub share: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub nodes: Vec<PlanNode>,
    pub basis: Basis,
    pub total_cost: Option<f64>,
    pub exec_ms: Option<f64>,
    pub plan_ms: Option<f64>,
    pub analyze: bool,
}

/// One row set read back from the server: column names and text cells.
#[derive(Clone, Debug, Default)]
pub struct Answer {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// The statement variants to try, in order (the first the server accepts wins).
pub fn explain_sql(engine: Engine, stmt: &str, analyze: bool) -> Vec<String> {
    let stmt = stmt.trim().trim_end_matches(';').trim();
    match (engine, analyze) {
        // MariaDB: `ANALYZE FORMAT=JSON`; MySQL 8: `EXPLAIN ANALYZE` (text tree).
        (Engine::Mysql, true) => vec![
            format!("ANALYZE FORMAT=JSON {stmt}"),
            format!("EXPLAIN ANALYZE {stmt}"),
        ],
        (Engine::Mysql, false) => vec![
            format!("EXPLAIN FORMAT=JSON {stmt}"),
            format!("EXPLAIN {stmt}"),
        ],
        (Engine::Postgres, true) => vec![format!("EXPLAIN (ANALYZE, FORMAT JSON) {stmt}")],
        (Engine::Postgres, false) => vec![format!("EXPLAIN (FORMAT JSON) {stmt}")],
        // No ANALYZE in SQLite: the plan is the same either way.
        (Engine::Sqlite, _) => vec![format!("EXPLAIN QUERY PLAN {stmt}")],
    }
}

/// Turns the server's answer into a plan.
pub fn parse(engine: Engine, analyze: bool, a: &Answer) -> Result<Plan, String> {
    if engine == Engine::Sqlite {
        return Ok(from_sqlite(a));
    }
    let first = a
        .rows
        .first()
        .and_then(|r| r.first())
        .cloned()
        .flatten()
        .unwrap_or_default();
    let trimmed = first.trim_start();
    let mut plan = if a.columns.len() == 1 && (trimmed.starts_with('{') || trimmed.starts_with('['))
    {
        let v: J = serde_json::from_str(&first).map_err(|e| format!("plan JSON: {e}"))?;
        match engine {
            Engine::Postgres => from_pg_json(&v)?,
            Engine::Mysql => from_mysql_json(&v)?,
            Engine::Sqlite => unreachable!("handled above"),
        }
    } else if a.columns.len() == 1
        && a.rows.iter().any(|r| {
            r[0].as_deref()
                .is_some_and(|l| l.trim_start().starts_with("->"))
        })
    {
        // MySQL's tree: one line per row (or one cell with many lines).
        let text = a
            .rows
            .iter()
            .filter_map(|r| r[0].clone())
            .collect::<Vec<_>>()
            .join("\n");
        from_text_tree(&text)?
    } else if a.columns.len() == 1 && first.contains("\n-> ") {
        from_text_tree(&first)?
    } else if a.columns.iter().any(|c| c.eq_ignore_ascii_case("table")) {
        from_table(a)
    } else {
        return Err("the server answered with a plan format that is not understood".into());
    };
    plan.analyze = analyze;
    Ok(plan)
}

fn empty_plan() -> Plan {
    Plan {
        nodes: Vec::new(),
        basis: Basis::None,
        total_cost: None,
        exec_ms: None,
        plan_ms: None,
        analyze: false,
    }
}

// ---- shared helpers ------------------------------------------------------------

fn num(v: Option<&J>) -> Option<f64> {
    match v? {
        J::Number(n) => n.as_f64(),
        J::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn text(v: Option<&J>) -> Option<String> {
    match v? {
        J::String(s) if !s.is_empty() => Some(s.clone()),
        J::Number(n) => Some(n.to_string()),
        J::Array(a) => {
            let parts: Vec<String> = a.iter().filter_map(|x| text(Some(x))).collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        _ => None,
    }
}

fn compact(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Fills `share` from the nodes' metric, relative to their sum.
fn finish(mut p: Plan, metric: &[Option<f64>], basis: Basis) -> Plan {
    let sum: f64 = metric.iter().flatten().filter(|m| **m > 0.0).sum();
    if sum > 0.0 {
        for (n, m) in p.nodes.iter_mut().zip(metric) {
            n.share = m.map(|m| (m.max(0.0) / sum * 100.0).clamp(0.0, 100.0));
        }
        p.basis = basis;
    }
    p
}

// ---- MySQL / MariaDB JSON ------------------------------------------------------

fn access_name(t: &str) -> String {
    match t {
        "ALL" => "Full table scan",
        "index" => "Index scan",
        "range" => "Index range scan",
        "ref" => "Index lookup",
        "eq_ref" => "Unique index lookup",
        "const" | "system" => "Constant lookup",
        "ref_or_null" => "Index lookup (or NULL)",
        "fulltext" => "Full-text search",
        "index_merge" => "Index merge",
        "unique_subquery" | "index_subquery" => "Subquery index lookup",
        other => other,
    }
    .to_string()
}

struct Walk {
    nodes: Vec<PlanNode>,
    metric: Vec<Option<f64>>,
    cost_metric: bool,
    time_metric: bool,
    total_cost: Option<f64>,
    exec_ms: Option<f64>,
}

impl Walk {
    fn push(
        &mut self,
        depth: usize,
        op: impl Into<String>,
        detail: impl Into<String>,
        metric: Option<f64>,
    ) {
        self.nodes.push(PlanNode {
            depth,
            op: op.into(),
            detail: detail.into(),
            share: None,
        });
        self.metric.push(metric);
    }

    fn table(&mut self, depth: usize, t: &J) {
        let name = text(t.get("table_name")).unwrap_or_default();
        let access = text(t.get("access_type"));
        let op = access
            .as_deref()
            .map(access_name)
            .unwrap_or_else(|| "Table".into());
        let mut d = vec![name];
        if let Some(k) = text(t.get("key")) {
            d.push(format!("key {k}"));
        }
        let est = num(t.get("rows")).or_else(|| num(t.get("rows_examined_per_scan")));
        if let Some(r) = num(t.get("r_rows")) {
            d.push(format!("{} rows", r as i64));
        } else if let Some(r) = est {
            d.push(format!("~{} rows", r as i64));
        }
        if let Some(c) = text(t.get("attached_condition")) {
            d.push(compact(&c));
        }
        // Metric by preference: measured time, server cost, estimated rows.
        let time = num(t.get("r_total_time_ms")).or_else(|| {
            match (num(t.get("r_table_time_ms")), num(t.get("r_other_time_ms"))) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
            }
        });
        let cost = t.get("cost_info").and_then(|c| {
            match (num(c.get("read_cost")), num(c.get("eval_cost"))) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
            }
        });
        let metric = if let Some(ms) = time {
            self.time_metric = true;
            Some(ms)
        } else if let Some(c) = cost {
            self.cost_metric = true;
            Some(c)
        } else {
            est
        };
        self.push(
            depth,
            op,
            d.into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("  ·  "),
            metric,
        );
        // A table can hold a derived table / subquery.
        for k in [
            "materialized_from_subquery",
            "attached_subqueries",
            "subqueries",
            "optimized_away_subqueries",
        ] {
            if let Some(v) = t.get(k) {
                self.container(depth + 1, k, v);
            }
        }
    }

    fn container(&mut self, depth: usize, key: &str, v: &J) {
        match key {
            "materialized_from_subquery" => {
                self.push(
                    depth,
                    "Derived table",
                    text(v.get("using_temporary_table")).unwrap_or_default(),
                    None,
                );
                self.object(depth + 1, v);
            }
            "attached_subqueries" | "subqueries" | "optimized_away_subqueries" => {
                if let Some(a) = v.as_array() {
                    for s in a {
                        self.push(
                            depth,
                            "Subquery",
                            if key == "optimized_away_subqueries" {
                                "optimized away"
                            } else {
                                ""
                            },
                            None,
                        );
                        self.object(depth + 1, s);
                    }
                }
            }
            _ => self.object(depth, v),
        }
    }

    fn object(&mut self, depth: usize, v: &J) {
        let Some(m) = v.as_object() else {
            if let Some(a) = v.as_array() {
                for x in a {
                    self.object(depth, x);
                }
            }
            return;
        };
        for (k, val) in m {
            match k.as_str() {
                "table" if val.is_object() => self.table(depth, val),
                "query_block" => {
                    let id = text(val.get("select_id"))
                        .map(|i| format!("select #{i}"))
                        .unwrap_or_default();
                    let cost = num(val.get("cost_info").and_then(|c| c.get("query_cost")))
                        .or_else(|| num(val.get("cost")));
                    if depth == 0 {
                        self.total_cost = cost;
                        self.exec_ms = num(val.get("r_total_time_ms"));
                    }
                    self.push(depth, "Query block", id, None);
                    self.object(depth + 1, val);
                }
                "nested_loop" | "block-nl-join" => {
                    let n = val.as_array().map(|a| a.len()).unwrap_or(1);
                    if n > 1 || k == "block-nl-join" {
                        self.push(
                            depth,
                            if k == "nested_loop" {
                                "Nested loop join"
                            } else {
                                "Block nested loop join"
                            },
                            "",
                            None,
                        );
                        self.object(depth + 1, val);
                    } else {
                        self.object(depth, val);
                    }
                }
                "ordering_operation" => {
                    let d = if val.get("using_filesort").and_then(J::as_bool) == Some(true) {
                        "filesort"
                    } else {
                        ""
                    };
                    self.push(depth, "Sort", d, None);
                    self.object(depth + 1, val);
                }
                "grouping_operation" => {
                    self.push(
                        depth,
                        "Group",
                        if val.get("using_temporary_table").and_then(J::as_bool) == Some(true) {
                            "temporary table"
                        } else {
                            ""
                        },
                        None,
                    );
                    self.object(depth + 1, val);
                }
                "duplicates_removal" => {
                    self.push(depth, "Distinct", "", None);
                    self.object(depth + 1, val);
                }
                "buffer_result" => {
                    self.push(depth, "Buffer result", "", None);
                    self.object(depth + 1, val);
                }
                "windowing" | "window_operation" => {
                    self.push(depth, "Window", "", None);
                    self.object(depth + 1, val);
                }
                "union_result" => {
                    self.push(
                        depth,
                        "Union",
                        text(val.get("table_name")).unwrap_or_default(),
                        None,
                    );
                    if let Some(q) = val.get("query_specifications") {
                        self.object(depth + 1, q);
                    }
                }
                "materialized_from_subquery"
                | "attached_subqueries"
                | "subqueries"
                | "optimized_away_subqueries" => self.container(depth, k, val),
                "message" => {
                    if let Some(s) = text(Some(val)) {
                        self.push(depth, "Note", s, None);
                    }
                }
                "select_id"
                | "cost_info"
                | "cost"
                | "r_total_time_ms"
                | "using_filesort"
                | "using_temporary_table"
                | "table_name" => {}
                _ => {
                    if val.is_object() || val.is_array() {
                        self.object(depth, val);
                    }
                }
            }
        }
    }
}

fn from_mysql_json(v: &J) -> Result<Plan, String> {
    let mut w = Walk {
        nodes: Vec::new(),
        metric: Vec::new(),
        cost_metric: false,
        time_metric: false,
        total_cost: None,
        exec_ms: None,
    };
    w.object(0, v);
    if w.nodes.is_empty() {
        return Err("empty plan".into());
    }
    let basis = if w.time_metric {
        Basis::Time
    } else if w.cost_metric {
        Basis::Cost
    } else {
        Basis::Rows
    };
    let mut p = empty_plan();
    p.total_cost = w.total_cost;
    p.exec_ms = w.exec_ms;
    p.nodes = w.nodes;
    Ok(finish(p, &w.metric, basis))
}

// ---- PostgreSQL JSON -----------------------------------------------------------

fn from_pg_json(v: &J) -> Result<Plan, String> {
    let top = match v {
        J::Array(a) => a.first().ok_or("empty plan")?,
        other => other,
    };
    let root = top.get("Plan").ok_or("no \"Plan\" in the answer")?;
    let analyze = root.get("Actual Total Time").is_some();
    let mut nodes = Vec::new();
    let mut metric = Vec::new();
    fn walk(
        n: &J,
        depth: usize,
        analyze: bool,
        nodes: &mut Vec<PlanNode>,
        metric: &mut Vec<Option<f64>>,
    ) -> f64 {
        let ty = text(n.get("Node Type")).unwrap_or_default();
        let op = match text(n.get("Join Type")) {
            Some(j) if ty.contains("Join") || ty.contains("Loop") => format!("{ty} ({j})"),
            _ => ty,
        };
        let mut d = Vec::new();
        if let Some(r) = text(n.get("Relation Name")) {
            let alias = text(n.get("Alias")).filter(|a| *a != r);
            d.push(match alias {
                Some(a) => format!("{r} {a}"),
                None => r,
            });
        }
        if let Some(i) = text(n.get("Index Name")) {
            d.push(format!("index {i}"));
        }
        for k in [
            "Index Cond",
            "Hash Cond",
            "Merge Cond",
            "Join Filter",
            "Filter",
            "Recheck Cond",
            "Sort Key",
            "Group Key",
        ] {
            if let Some(c) = text(n.get(k)) {
                d.push(format!("{k}: {}", compact(&c)));
            }
        }
        match (analyze, num(n.get("Actual Rows"))) {
            (true, Some(r)) => d.push(format!("{} rows", r as i64)),
            _ => {
                if let Some(r) = num(n.get("Plan Rows")) {
                    d.push(format!("~{} rows", r as i64));
                }
            }
        }
        let idx = nodes.len();
        nodes.push(PlanNode {
            depth,
            op,
            detail: d.join("  ·  "),
            share: None,
        });
        metric.push(None);
        let loops = num(n.get("Actual Loops")).unwrap_or(1.0).max(1.0);
        let incl = if analyze {
            num(n.get("Actual Total Time")).unwrap_or(0.0) * loops
        } else {
            num(n.get("Total Cost")).unwrap_or(0.0)
        };
        let mut kids = 0.0;
        if let Some(a) = n.get("Plans").and_then(J::as_array) {
            for c in a {
                kids += walk(c, depth + 1, analyze, nodes, metric);
            }
        }
        metric[idx] = Some((incl - kids).max(0.0));
        incl
    }
    let total = walk(root, 0, analyze, &mut nodes, &mut metric);
    let mut p = empty_plan();
    p.nodes = nodes;
    p.total_cost = num(root.get("Total Cost"));
    p.exec_ms = num(top.get("Execution Time"));
    p.plan_ms = num(top.get("Planning Time"));
    let _ = total;
    Ok(finish(
        p,
        &metric,
        if analyze { Basis::Time } else { Basis::Cost },
    ))
}

// ---- MySQL 8 `EXPLAIN ANALYZE` tree --------------------------------------------

fn from_text_tree(text: &str) -> Result<Plan, String> {
    struct Raw {
        depth: usize,
        op: String,
        detail: String,
        incl: Option<f64>,
    }
    let mut raw: Vec<Raw> = Vec::new();
    for line in text.lines() {
        let Some(at) = line.find("->") else { continue };
        let indent = line[..at].chars().count();
        let body = line[at + 2..].trim();
        // Split `name: detail  (cost=.. rows=..) (actual time=a..b rows=r loops=l)`.
        let (head, stats) = match body.find("  (") {
            Some(i) => (&body[..i], &body[i..]),
            None => match body.find(" (") {
                Some(i) => (&body[..i], &body[i..]),
                None => (body, ""),
            },
        };
        let (op, detail) = match head.split_once(": ") {
            Some((o, d)) => (o.trim().to_string(), d.trim().to_string()),
            None => (head.trim().to_string(), String::new()),
        };
        let grab = |key: &str| -> Option<f64> {
            let i = stats.find(key)? + key.len();
            let s = &stats[i..];
            let e = s
                .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e'))
                .unwrap_or(s.len());
            s[..e].parse().ok()
        };
        let incl = stats.find("actual time=").and_then(|i| {
            let part = &stats[i + "actual time=".len()..];
            let end = part.split("..").nth(1)?;
            let end_ms: f64 = end
                .split(|c: char| !(c.is_ascii_digit() || c == '.'))
                .next()?
                .parse()
                .ok()?;
            let loops = grab("loops=").unwrap_or(1.0);
            Some(end_ms * loops)
        });
        let rows = grab("rows=");
        let detail = match (detail.is_empty(), rows) {
            (_, Some(r)) => {
                let tail = format!("{} rows", r as i64);
                if detail.is_empty() {
                    tail
                } else {
                    format!("{detail}  ·  {tail}")
                }
            }
            (_, None) => detail,
        };
        raw.push(Raw {
            depth: indent / 4,
            op,
            detail,
            incl,
        });
    }
    if raw.is_empty() {
        return Err("empty plan".into());
    }
    // Self time = own total minus the totals of the direct children.
    let mut metric = vec![None; raw.len()];
    let timed = raw.iter().any(|r| r.incl.is_some());
    if timed {
        for i in 0..raw.len() {
            let Some(own) = raw[i].incl else { continue };
            let mut kids = 0.0;
            let mut j = i + 1;
            while j < raw.len() && raw[j].depth > raw[i].depth {
                if raw[j].depth == raw[i].depth + 1 {
                    kids += raw[j].incl.unwrap_or(0.0);
                }
                j += 1;
            }
            metric[i] = Some((own - kids).max(0.0));
        }
    }
    let mut p = empty_plan();
    p.exec_ms = raw.first().and_then(|r| r.incl);
    p.nodes = raw
        .into_iter()
        .map(|r| PlanNode {
            depth: r.depth,
            op: r.op,
            detail: r.detail,
            share: None,
        })
        .collect();
    Ok(finish(p, &metric, Basis::Time))
}

// ---- plain tabular EXPLAIN -----------------------------------------------------

/// SQLite: rows `(id, parent, notused, detail)`; `parent` is the id of the
/// enclosing step (0 = top level). `detail` reads `SCAN t`, `SEARCH t USING
/// INDEX i (a=?)`, `USE TEMP B-TREE FOR ORDER BY`.
fn from_sqlite(a: &Answer) -> Plan {
    let col = |name: &str| a.columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let (cid, cparent, cdetail) = (col("id"), col("parent"), col("detail"));
    let get =
        |r: &Vec<Option<String>>, c: Option<usize>| c.and_then(|i| r.get(i).cloned().flatten());
    let mut p = empty_plan();
    let mut depth_of: Vec<(i64, usize)> = Vec::new();
    for r in &a.rows {
        let id = get(r, cid).and_then(|s| s.parse::<i64>().ok());
        let parent = get(r, cparent)
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let depth = depth_of
            .iter()
            .find(|(i, _)| *i == parent)
            .map_or(0, |(_, d)| d + 1);
        if let Some(id) = id {
            depth_of.push((id, depth));
        }
        let d = compact(&get(r, cdetail).unwrap_or_default());
        let (op, detail) = match d.split_once(" ") {
            Some((w, rest))
                if matches!(
                    w,
                    "SCAN"
                        | "SEARCH"
                        | "MERGE"
                        | "CORRELATED"
                        | "MATERIALIZE"
                        | "CO-ROUTINE"
                        | "COMPOUND"
                        | "LIST"
                ) =>
            {
                (w.to_string(), rest.to_string())
            }
            _ => (d, String::new()),
        };
        p.nodes.push(PlanNode {
            depth,
            op,
            detail,
            share: None,
        });
    }
    p
}

fn from_table(a: &Answer) -> Plan {
    let col = |name: &str| a.columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let (ct, cty, ck, cr, ce, csel) = (
        col("table"),
        col("type"),
        col("key"),
        col("rows"),
        col("Extra"),
        col("select_type"),
    );
    let get = |r: &Vec<Option<String>>, c: Option<usize>| {
        c.and_then(|i| r.get(i).cloned().flatten())
            .filter(|s| !s.is_empty())
    };
    let mut p = empty_plan();
    let mut metric = Vec::new();
    for r in &a.rows {
        let op = get(r, cty)
            .map(|t| access_name(&t))
            .unwrap_or_else(|| "Table".into());
        let mut d = Vec::new();
        if let Some(t) = get(r, ct) {
            d.push(t);
        }
        if let Some(k) = get(r, ck) {
            d.push(format!("key {k}"));
        }
        let rows = get(r, cr).and_then(|s| s.parse::<f64>().ok());
        if let Some(n) = rows {
            d.push(format!("~{} rows", n as i64));
        }
        if let Some(e) = get(r, ce) {
            d.push(e);
        }
        let depth = if get(r, csel).is_some_and(|s| s != "SIMPLE" && s != "PRIMARY") {
            1
        } else {
            0
        };
        p.nodes.push(PlanNode {
            depth,
            op,
            detail: d.join("  ·  "),
            share: None,
        });
        metric.push(rows);
    }
    finish(p, &metric, Basis::Rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(s: &str) -> Answer {
        Answer {
            columns: vec!["EXPLAIN".into()],
            rows: vec![vec![Some(s.into())]],
        }
    }

    #[::core::prelude::v1::test]
    fn statements_per_engine() {
        assert_eq!(
            explain_sql(Engine::Mysql, "select 1;", false),
            vec!["EXPLAIN FORMAT=JSON select 1", "EXPLAIN select 1"]
        );
        assert_eq!(
            explain_sql(Engine::Mysql, " delete from t ", true),
            vec![
                "ANALYZE FORMAT=JSON delete from t",
                "EXPLAIN ANALYZE delete from t"
            ]
        );
        assert_eq!(
            explain_sql(Engine::Postgres, "select 1", false),
            vec!["EXPLAIN (FORMAT JSON) select 1"]
        );
        assert_eq!(
            explain_sql(Engine::Postgres, "select 1", true),
            vec!["EXPLAIN (ANALYZE, FORMAT JSON) select 1"]
        );
    }

    #[::core::prelude::v1::test]
    fn sqlite_query_plan_tree() {
        let a = Answer {
            columns: vec![
                "id".into(),
                "parent".into(),
                "notused".into(),
                "detail".into(),
            ],
            rows: vec![
                vec![
                    Some("2".into()),
                    Some("0".into()),
                    Some("0".into()),
                    Some("SCAN a".into()),
                ],
                vec![
                    Some("5".into()),
                    Some("0".into()),
                    Some("0".into()),
                    Some("SEARCH b USING INDEX b_x (x=?)".into()),
                ],
                vec![
                    Some("9".into()),
                    Some("5".into()),
                    Some("0".into()),
                    Some("USE TEMP B-TREE FOR ORDER BY".into()),
                ],
            ],
        };
        assert_eq!(
            explain_sql(Engine::Sqlite, "select 1;", true),
            vec!["EXPLAIN QUERY PLAN select 1"]
        );
        let p = parse(Engine::Sqlite, false, &a).unwrap();
        let n: Vec<(&str, &str, usize)> = p
            .nodes
            .iter()
            .map(|n| (n.op.as_str(), n.detail.as_str(), n.depth))
            .collect();
        assert_eq!(
            n,
            vec![
                ("SCAN", "a", 0),
                ("SEARCH", "b USING INDEX b_x (x=?)", 0),
                ("USE TEMP B-TREE FOR ORDER BY", "", 1)
            ]
        );
        assert_eq!(p.basis, Basis::None);
    }

    #[::core::prelude::v1::test]
    fn mariadb_json_plain() {
        let j = r#"{"query_block":{"select_id":1,"nested_loop":[
          {"table":{"table_name":"a","access_type":"ALL","rows":300,"filtered":100,"attached_condition":"a.x > 1"}},
          {"table":{"table_name":"b","access_type":"eq_ref","key":"PRIMARY","rows":1,"filtered":100}}]}}"#;
        let p = parse(Engine::Mysql, false, &one(j)).unwrap();
        let ops: Vec<(&str, usize)> = p.nodes.iter().map(|n| (n.op.as_str(), n.depth)).collect();
        assert_eq!(
            ops,
            vec![
                ("Query block", 0),
                ("Nested loop join", 1),
                ("Full table scan", 2),
                ("Unique index lookup", 2)
            ]
        );
        assert_eq!(p.basis, Basis::Rows);
        let a = p.nodes[2].share.unwrap();
        let b = p.nodes[3].share.unwrap();
        assert!(
            (a - 300.0 / 301.0 * 100.0).abs() < 1e-6 && (b - 100.0 / 301.0).abs() < 1e-6,
            "{a} {b}"
        );
        assert!(
            p.nodes[2].detail.contains("a  ·  ~300 rows  ·  a.x > 1"),
            "{}",
            p.nodes[2].detail
        );
        assert!(p.nodes[3].detail.contains("key PRIMARY"));
    }

    #[::core::prelude::v1::test]
    fn mariadb_json_analyze_uses_time() {
        let j = r#"{"query_block":{"select_id":1,"r_total_time_ms":4.0,"nested_loop":[
          {"table":{"table_name":"a","access_type":"ALL","rows":10,"r_rows":10,"r_table_time_ms":3.0,"r_other_time_ms":0.0}},
          {"table":{"table_name":"b","access_type":"ref","key":"k","rows":2,"r_rows":2,"r_table_time_ms":0.5,"r_other_time_ms":0.5}}]}}"#;
        let p = parse(Engine::Mysql, true, &one(j)).unwrap();
        assert_eq!(p.basis, Basis::Time);
        assert!(p.analyze);
        assert_eq!(p.exec_ms, Some(4.0));
        assert!((p.nodes[2].share.unwrap() - 75.0).abs() < 1e-9);
        assert!((p.nodes[3].share.unwrap() - 25.0).abs() < 1e-9);
        assert!(p.nodes[2].detail.contains("10 rows") && !p.nodes[2].detail.contains('~'));
    }

    #[::core::prelude::v1::test]
    fn mysql8_json_uses_cost_and_wrappers() {
        let j = r#"{"query_block":{"select_id":1,"cost_info":{"query_cost":"12.50"},"ordering_operation":{"using_filesort":true,
          "nested_loop":[{"table":{"table_name":"t","access_type":"range","key":"i","rows_examined_per_scan":5,
          "cost_info":{"read_cost":"3.0","eval_cost":"1.0","prefix_cost":"4.0"},"attached_condition":"(`t`.`x` > 3)"}}]}}}"#;
        let p = parse(Engine::Mysql, false, &one(j)).unwrap();
        assert_eq!(p.total_cost, Some(12.5));
        assert_eq!(p.basis, Basis::Cost);
        let ops: Vec<&str> = p.nodes.iter().map(|n| n.op.as_str()).collect();
        assert_eq!(ops, vec!["Query block", "Sort", "Index range scan"]);
        assert_eq!(p.nodes[1].detail, "filesort");
        assert_eq!(p.nodes[2].share, Some(100.0));
    }

    #[::core::prelude::v1::test]
    fn postgres_json_self_cost_share() {
        let j = r#"[{"Plan":{"Node Type":"Hash Join","Join Type":"Inner","Total Cost":100.0,"Plan Rows":50,"Hash Cond":"(a.id = b.id)",
          "Plans":[
            {"Node Type":"Seq Scan","Relation Name":"a","Alias":"a","Total Cost":60.0,"Plan Rows":1000,"Filter":"(x > 1)"},
            {"Node Type":"Hash","Total Cost":30.0,"Plan Rows":10,"Plans":[{"Node Type":"Index Scan","Relation Name":"orders","Alias":"b","Index Name":"orders_pkey","Total Cost":25.0,"Plan Rows":10}]}]}}]"#;
        let p = parse(Engine::Postgres, false, &one(j)).unwrap();
        assert_eq!(p.basis, Basis::Cost);
        assert_eq!(p.total_cost, Some(100.0));
        let ops: Vec<(String, usize)> = p.nodes.iter().map(|n| (n.op.clone(), n.depth)).collect();
        assert_eq!(
            ops,
            vec![
                ("Hash Join (Inner)".into(), 0),
                ("Seq Scan".into(), 1),
                ("Hash".into(), 1),
                ("Index Scan".into(), 2)
            ]
        );
        // self costs: join 100-60-30 = 10, scan 60, hash 30-25 = 5, index 25.
        let s: Vec<f64> = p.nodes.iter().map(|n| n.share.unwrap().round()).collect();
        assert_eq!(s, vec![10.0, 60.0, 5.0, 25.0]);
        assert!(
            p.nodes[3].detail.contains("orders b")
                && p.nodes[3].detail.contains("index orders_pkey"),
            "{}",
            p.nodes[3].detail
        );
    }

    #[::core::prelude::v1::test]
    fn postgres_analyze_uses_time_and_loops() {
        let j = r#"[{"Plan":{"Node Type":"Nested Loop","Join Type":"Inner","Total Cost":9.0,"Actual Total Time":10.0,"Actual Loops":1,"Actual Rows":4,
          "Plans":[{"Node Type":"Seq Scan","Relation Name":"a","Alias":"a","Total Cost":1.0,"Actual Total Time":2.0,"Actual Loops":1,"Actual Rows":2},
                   {"Node Type":"Index Scan","Relation Name":"b","Alias":"b","Index Name":"i","Total Cost":4.0,"Actual Total Time":3.0,"Actual Loops":2,"Actual Rows":2}]},
          "Planning Time":0.2,"Execution Time":10.4}]"#;
        let p = parse(Engine::Postgres, true, &one(j)).unwrap();
        assert!(p.analyze);
        assert_eq!(p.basis, Basis::Time);
        assert_eq!((p.exec_ms, p.plan_ms), (Some(10.4), Some(0.2)));
        // join self = 10 - 2 - 6 = 2; scan 2; index 6.
        let s: Vec<f64> = p.nodes.iter().map(|n| n.share.unwrap().round()).collect();
        assert_eq!(s, vec![20.0, 20.0, 60.0]);
        assert!(p.nodes[0].detail.contains("4 rows"));
    }

    #[::core::prelude::v1::test]
    fn mysql_explain_analyze_tree() {
        let t = "-> Nested loop inner join  (cost=1.0 rows=4) (actual time=0.1..1.0 rows=4 loops=1)\n    -> Table scan on a  (cost=0.5 rows=2) (actual time=0.05..0.2 rows=2 loops=1)\n    -> Single-row index lookup on b using PRIMARY (id=a.id)  (cost=0.3 rows=1) (actual time=0.1..0.2 rows=1 loops=2)\n";
        let a = Answer {
            columns: vec!["EXPLAIN".into()],
            rows: vec![vec![Some(t.into())]],
        };
        let p = parse(Engine::Mysql, true, &a).unwrap();
        assert_eq!(p.nodes.len(), 3);
        assert_eq!(p.nodes[0].depth, 0);
        assert_eq!(p.nodes[1].depth, 1);
        assert_eq!(p.nodes[1].op, "Table scan on a");
        assert_eq!(p.basis, Basis::Time);
        // join self = 1.0 - 0.2 - 0.4 = 0.4 of total 1.0.
        assert_eq!(
            p.nodes
                .iter()
                .map(|n| (n.share.unwrap() * 10.0).round() as i64)
                .collect::<Vec<_>>(),
            vec![400, 200, 400]
        );
        assert_eq!(p.exec_ms, Some(1.0));
    }

    #[::core::prelude::v1::test]
    fn plain_tabular_explain() {
        let a = Answer {
            columns: [
                "id",
                "select_type",
                "table",
                "type",
                "possible_keys",
                "key",
                "key_len",
                "ref",
                "rows",
                "Extra",
            ]
            .map(String::from)
            .to_vec(),
            rows: vec![
                vec![
                    Some("1".into()),
                    Some("SIMPLE".into()),
                    Some("a".into()),
                    Some("ALL".into()),
                    None,
                    None,
                    None,
                    None,
                    Some("30".into()),
                    Some("Using where".into()),
                ],
                vec![
                    Some("1".into()),
                    Some("SIMPLE".into()),
                    Some("b".into()),
                    Some("ref".into()),
                    None,
                    Some("k".into()),
                    None,
                    None,
                    Some("10".into()),
                    None,
                ],
            ],
        };
        let p = parse(Engine::Mysql, false, &a).unwrap();
        assert_eq!(p.basis, Basis::Rows);
        assert_eq!(p.nodes[0].op, "Full table scan");
        assert_eq!(p.nodes[0].share, Some(75.0));
        assert!(p.nodes[1].detail.contains("key k"));
    }

    #[::core::prelude::v1::test]
    fn garbage_is_an_error() {
        assert!(parse(Engine::Mysql, false, &one("hello")).is_err());
        assert!(parse(Engine::Postgres, false, &one("{\"x\":1}")).is_err());
        assert!(parse(Engine::Mysql, false, &one("{bad json")).is_err());
    }
}
