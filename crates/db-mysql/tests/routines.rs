//! Source tab save path against the local MariaDB: SHOW CREATE → schema
//! qualified text → `save_plan` (CREATE OR REPLACE on MariaDB, DROP + CREATE
//! on MySQL) sent through the driver as one statement each. Names start
//! with `wb_r_`; everything is dropped at the end.
mod common;

use common::*;
use db_core::dictionary::Dialect;
use db_core::routine;
use db_core::{DbObjectKind as K, Driver, Outcome, QueryOptions};

fn run(d: &impl Driver, sql: &str) -> Outcome {
    collect(&mut d.query(sql.into(), QueryOptions::default())).outcome
}

fn run_ok(d: &impl Driver, sql: &str) {
    let o = run(d, sql);
    assert!(matches!(o, Outcome::Done { .. }), "{sql}: {o:?}");
}

#[test]
fn edit_and_save_routines() {
    let d = connect();
    let drops = [
        "DROP TRIGGER IF EXISTS wb_r_trg",
        "DROP EVENT IF EXISTS wb_r_ev",
        "DROP PROCEDURE IF EXISTS wb_r_proc",
        "DROP FUNCTION IF EXISTS wb_r_fn",
        "DROP TABLE IF EXISTS wb_r_t",
    ];
    for s in drops {
        run_ok(&d, s);
    }
    for s in [
        "CREATE TABLE wb_r_t (a INT)",
        "CREATE PROCEDURE wb_r_proc() BEGIN SELECT 1; SELECT 2; END",
        "CREATE FUNCTION wb_r_fn() RETURNS INT DETERMINISTIC RETURN 1",
        "CREATE TRIGGER wb_r_trg BEFORE INSERT ON wb_r_t FOR EACH ROW SET NEW.a = 1",
        "CREATE EVENT wb_r_ev ON SCHEDULE EVERY 1 DAY DISABLE DO SELECT 1",
    ] {
        run_ok(&d, s);
    }
    let c = d.catalog().unwrap();
    let cases = [
        (
            K::Procedure,
            "wb_r_proc",
            "SELECT 1; SELECT 2;",
            "SELECT 3; SELECT 4;",
        ),
        (K::Function, "wb_r_fn", "RETURN 1", "RETURN 2"),
        (K::Trigger, "wb_r_trg", "NEW.a = 1", "NEW.a = 2"),
        (K::Event, "wb_r_ev", "DO SELECT 1", "DO SELECT 2"),
    ];
    for (kind, name, from, to) in cases {
        let ddl = block_on(c.ddl("bench", kind, name)).unwrap();
        let text = routine::mysql_qualify(&ddl, "bench", kind);
        assert!(text.contains(&"`bench`.".to_string()), "{text}");
        let edited = text.replace(from, to) + ";\n";
        // MariaDB: one atomic CREATE OR REPLACE, body kept whole
        let plan = routine::save_plan(Dialect::Mysql, true, "bench", kind, name, &edited);
        assert!(plan.atomic && plan.statements.len() == 1, "{plan:?}");
        run_ok(&d, &plan.statements[0]);
        let after = block_on(c.ddl("bench", kind, name)).unwrap();
        assert!(after.contains(to), "{kind:?}: {after}");
        // MySQL style: DROP + CREATE works too (and is reported non-atomic)
        let plan = routine::save_plan(
            Dialect::Mysql,
            false,
            "bench",
            kind,
            name,
            &routine::mysql_qualify(&after, "bench", kind),
        );
        assert!(!plan.atomic && plan.statements.len() == 2);
        for s in &plan.statements {
            run_ok(&d, s);
        }
        assert!(block_on(c.ddl("bench", kind, name)).unwrap().contains(to));
    }
    // a broken body: the server rejects it and the object is unchanged (atomic)
    let plan = routine::save_plan(
        Dialect::Mysql,
        true,
        "bench",
        K::Procedure,
        "wb_r_proc",
        "CREATE PROCEDURE `bench`.`wb_r_proc`() BEGIN SELEC 1; END",
    );
    assert!(matches!(run(&d, &plan.statements[0]), Outcome::Failed(_)));
    assert!(
        block_on(c.ddl("bench", K::Procedure, "wb_r_proc"))
            .unwrap()
            .contains("SELECT 3")
    );

    for s in drops {
        run_ok(&d, s);
    }
    let left = block_on(c.list_objects("bench", K::Procedure, 1000)).unwrap();
    assert!(!left.iter().any(|e| e.name.starts_with("wb_r_")));
}
