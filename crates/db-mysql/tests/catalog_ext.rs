//! Extended catalog (object kinds, children, parameters, DDL, server level)
//! against the local MariaDB. Names start with `dbw_x_`.
mod common;

use common::*;
use db_core::{DbObjectKind as K, Driver, Outcome, QueryOptions};

fn run_ok(d: &impl Driver, sql: &str) {
    let got = collect(&mut d.query(sql.into(), QueryOptions::default()));
    assert!(
        matches!(got.outcome, Outcome::Done { .. }),
        "{sql}: {:?}",
        got.outcome
    );
}

#[test]
fn extended_catalog() {
    let d = connect();
    for s in [
        "DROP TRIGGER IF EXISTS dbw_x_trg",
        "DROP PROCEDURE IF EXISTS dbw_x_proc",
        "DROP FUNCTION IF EXISTS dbw_x_fn",
        "DROP VIEW IF EXISTS dbw_x_view",
        "DROP TABLE IF EXISTS dbw_x_t",
        "CREATE TABLE dbw_x_t (id INT PRIMARY KEY, a VARCHAR(10), b INT, UNIQUE KEY dbw_x_u (a, b), KEY dbw_x_b (b)) ENGINE=InnoDB",
        "CREATE VIEW dbw_x_view AS SELECT id FROM dbw_x_t",
        "CREATE PROCEDURE dbw_x_proc(IN p1 INT, OUT p2 VARCHAR(5)) BEGIN SET p2 = 'x'; END",
        "CREATE FUNCTION dbw_x_fn(n INT) RETURNS INT DETERMINISTIC RETURN n + 1",
        "CREATE TRIGGER dbw_x_trg BEFORE INSERT ON dbw_x_t FOR EACH ROW SET NEW.b = 1",
    ] {
        run_ok(&d, s);
    }
    let c = d.catalog().unwrap();
    let kinds = c.supported_kinds();
    assert!(
        kinds.contains(&K::Table) && kinds.contains(&K::Trigger) && !kinds.contains(&K::Package)
    );
    let names = |v: Vec<db_core::ObjectEntry>| v.into_iter().map(|e| e.name).collect::<Vec<_>>();

    assert!(
        names(block_on(c.list_objects("bench", K::Table, 1000)).unwrap())
            .contains(&"dbw_x_t".to_string())
    );
    let views = names(block_on(c.list_objects("bench", K::View, 1000)).unwrap());
    assert!(views.contains(&"dbw_x_view".to_string()) && !views.contains(&"dbw_x_t".to_string()));
    // the row cap is honoured
    assert_eq!(
        block_on(c.list_objects("bench", K::Table, 1))
            .unwrap()
            .len(),
        1
    );

    let idx = block_on(c.children("bench", "dbw_x_t", K::Index, 100)).unwrap();
    let u = idx.iter().find(|e| e.name == "dbw_x_u").unwrap();
    assert_eq!(
        (u.parent.as_deref(), u.detail.as_deref()),
        (Some("dbw_x_t"), Some("UNIQUE (a, b)"))
    );
    assert!(idx.iter().any(|e| e.name == "PRIMARY"));
    assert!(
        block_on(c.list_objects("bench", K::Index, 1000))
            .unwrap()
            .iter()
            .any(|e| e.name == "dbw_x_b")
    );

    let trg = block_on(c.children("bench", "dbw_x_t", K::Trigger, 100)).unwrap();
    assert_eq!(
        (trg[0].name.as_str(), trg[0].detail.as_deref()),
        ("dbw_x_trg", Some("BEFORE INSERT"))
    );
    assert!(
        names(block_on(c.list_objects("bench", K::Procedure, 100)).unwrap())
            .contains(&"dbw_x_proc".into())
    );
    assert!(
        names(block_on(c.list_objects("bench", K::Function, 100)).unwrap())
            .contains(&"dbw_x_fn".into())
    );
    block_on(c.list_objects("bench", K::Event, 100)).unwrap();

    let ps = block_on(c.routine_params("bench", K::Procedure, "dbw_x_proc", 100)).unwrap();
    assert_eq!(ps.len(), 2);
    assert_eq!(
        (
            ps[0].name.as_deref(),
            ps[0].mode.as_deref(),
            ps[0].type_name.as_str()
        ),
        (Some("p1"), Some("IN"), "int(11)")
    );
    assert_eq!(ps[1].mode.as_deref(), Some("OUT"));
    let fp = block_on(c.routine_params("bench", K::Function, "dbw_x_fn", 100)).unwrap();
    assert_eq!(
        (fp[0].position, fp[0].name.clone(), fp[0].mode.clone()),
        (0, None, None)
    );

    for (kind, name, needle) in [
        (K::Table, "dbw_x_t", "CREATE TABLE"),
        (K::View, "dbw_x_view", "VIEW"),
        (K::Procedure, "dbw_x_proc", "PROCEDURE"),
        (K::Function, "dbw_x_fn", "FUNCTION"),
        (K::Trigger, "dbw_x_trg", "TRIGGER"),
    ] {
        let ddl = block_on(c.ddl("bench", kind, name)).unwrap();
        assert!(
            ddl.to_uppercase().contains(needle) && ddl.contains(name),
            "{kind:?}: {ddl}"
        );
    }
    assert!(block_on(c.ddl("bench", K::Index, "dbw_x_u")).is_err());
    // a hostile name is quoted, not executed
    assert!(block_on(c.ddl("bench", K::Table, "x`; DROP TABLE dbw_x_t; --")).is_err());
    assert_eq!(
        names(block_on(c.list_objects("bench", K::Table, 100)).unwrap())
            .iter()
            .filter(|n| *n == "dbw_x_t")
            .count(),
        1
    );

    let users = block_on(c.principals(100)).unwrap();
    assert!(!users.is_empty() && users.iter().all(|p| p.kind == "user" || p.kind == "role"));
    let info = block_on(c.server_info(2000)).unwrap();
    let g = |grp: &str, n: &str| {
        info.iter()
            .find(|i| i.group == grp && i.name.eq_ignore_ascii_case(n))
            .map(|i| i.value.clone())
    };
    assert!(g("server", "version").is_some());
    assert!(g("charset", "character_set_server").is_some());
    assert!(info.iter().any(|i| i.group == "session variable"));
    println!(
        "[catalog-ext] {} info items, {} principals",
        info.len(),
        users.len()
    );

    for s in [
        "DROP TRIGGER dbw_x_trg",
        "DROP PROCEDURE dbw_x_proc",
        "DROP FUNCTION dbw_x_fn",
        "DROP VIEW dbw_x_view",
        "DROP TABLE dbw_x_t",
    ] {
        run_ok(&d, s);
    }
}
