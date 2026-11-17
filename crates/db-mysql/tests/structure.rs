//! `Catalog::structure` against the `erfk` fixture on the local MariaDB
//! (a local `erfk` schema). Reads only.
mod common;

use common::*;
use db_core::{DbObjectKind as K, Driver};

#[test]
fn every_section_of_the_erfk_tables() {
    let d = connect();
    let c = d.catalog().unwrap();
    let st = |t: &str| {
        block_on(c.structure("erfk", t, 500))
            .unwrap_or_else(|e| panic!("{t}: {} (the local erfk schema must be loaded)", e.message))
    };

    // Columns: type, default, auto increment, PK, comment, nullability.
    let cust = st("customer");
    let col = |s: &db_core::structure::TableStructure, n: &str| {
        s.columns.iter().find(|c| c.name == n).unwrap().clone()
    };
    assert_eq!(
        cust.columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "name", "email", "created_at"]
    );
    let id = col(&cust, "id");
    assert!(id.primary_key && id.auto_increment && !id.nullable);
    assert_eq!(id.type_name, "int(11)");
    assert_eq!(id.comment.as_deref(), Some("客戶編號"));
    let email = col(&cust, "email");
    assert!(email.nullable && email.default.is_none() && !email.primary_key);
    assert!(
        col(&cust, "created_at")
            .default
            .as_deref()
            .is_some_and(|d| d.to_lowercase().starts_with("current_timestamp"))
    );
    assert_eq!(
        col(&st("orders"), "status")
            .default
            .as_deref()
            .map(|d| d.trim_matches('\'')),
        Some("new")
    );

    // Indexes: primary first, unique flag, columns in order, composite.
    let names: Vec<_> = cust
        .indexes
        .iter()
        .map(|i| (i.name.as_str(), i.unique, i.primary, i.kind.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("PRIMARY", true, true, "BTREE"),
            ("uq_customer_email", true, false, "BTREE")
        ]
    );
    let item = st("order_item");
    assert_eq!(item.indexes[0].columns, ["order_id", "line_no"]);
    let ship = st("shipment");
    assert!(ship.indexes.iter().any(|i| i.name == "idx_shipment_item"
        && i.columns == ["order_id", "line_no"]
        && !i.unique));

    // Foreign keys out: composite, actions.
    assert_eq!(ship.foreign_keys.len(), 1);
    let fk = &ship.foreign_keys[0];
    assert_eq!(
        (fk.name.as_str(), fk.ref_table.as_str()),
        ("fk_shipment_item", "order_item")
    );
    assert_eq!(
        (fk.columns.clone(), fk.ref_columns.clone()),
        (
            vec!["order_id".into(), "line_no".into()],
            vec!["order_id".into(), "line_no".into()]
        )
    );
    assert_eq!(
        (fk.on_delete.as_str(), fk.on_update.as_str()),
        ("RESTRICT", "RESTRICT")
    );
    let addr = st("address");
    assert_eq!(addr.foreign_keys[0].on_delete, "CASCADE");
    let orders = st("orders");
    assert_eq!(
        orders
            .foreign_keys
            .iter()
            .find(|k| k.name == "fk_orders_rep")
            .unwrap()
            .on_delete,
        "SET NULL"
    );

    // Foreign keys in: customer is referenced by address and orders; employee also by itself.
    let mut by: Vec<_> = cust
        .referenced_by
        .iter()
        .map(|k| (k.table.as_str(), k.name.as_str()))
        .collect();
    by.sort();
    assert_eq!(
        by,
        [
            ("address", "fk_address_customer"),
            ("orders", "fk_orders_customer")
        ]
    );
    assert!(cust.foreign_keys.is_empty());
    let emp = st("employee");
    assert!(
        emp.referenced_by.iter().any(|k| k.table == "employee")
            && emp.referenced_by.iter().any(|k| k.table == "orders")
    );
    assert!(st("app_setting").referenced_by.is_empty());

    // Checks and triggers (added to the fixture for this tab).
    let prod = st("product");
    assert_eq!(prod.checks.len(), 1, "{:?}", prod.checks);
    assert_eq!(prod.checks[0].name, "ck_product_price");
    assert!(
        prod.checks[0].expression.contains("price") && prod.checks[0].expression.contains(">= 0"),
        "{}",
        prod.checks[0].expression
    );
    assert!(cust.checks.is_empty());
    assert_eq!(orders.triggers.len(), 1);
    assert_eq!(
        (
            orders.triggers[0].name.as_str(),
            orders.triggers[0].detail.as_deref()
        ),
        ("trg_orders_bu", Some("BEFORE UPDATE"))
    );

    // DDL.
    let ddl = block_on(c.ddl("erfk", K::Table, "shipment")).unwrap();
    assert!(
        ddl.starts_with("CREATE TABLE `shipment`") && ddl.contains("fk_shipment_item"),
        "{ddl}"
    );

    // The row cap holds.
    assert_eq!(
        block_on(c.structure("erfk", "customer", 2))
            .unwrap()
            .columns
            .len(),
        2
    );
}
