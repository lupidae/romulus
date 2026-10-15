//! The JSON the API serves and the web renders, pinned by snapshots. A change
//! here is a change to the contract with the SPA: review it as one.

use remus_core::{
    Schema,
    fixtures::SHOWCASE,
    model::{Entity, Extension},
};
use romulus_diff::diff;

fn showcase() -> Schema {
    Schema::from_json(SHOWCASE).unwrap()
}

fn entity<'a>(schema: &'a mut Schema, qualified_name: &str) -> &'a mut Entity {
    schema
        .entities
        .iter_mut()
        .find(|e| e.qualified_name() == qualified_name)
        .unwrap()
}

/// The release of a small storefront feature: coupons, an email type change,
/// a stricter note, a refund state, a new partition, an upgraded extension.
fn release() -> (Schema, Schema) {
    let old = showcase();
    let mut new = showcase();

    let mut coupons = entity(&mut new, "shop.warehouses").clone();
    coupons.name = "coupons".into();
    coupons.comment = Some("Discount codes".into());
    coupons.constraints[0].name = "coupons_pkey".into();
    new.entities.push(coupons);
    new.entities
        .retain(|e| e.qualified_name() != "audit.scratch");

    let customers = entity(&mut new, "shop.customers");
    let email = customers
        .columns
        .iter_mut()
        .find(|c| c.name == "email")
        .unwrap();
    email.r#type = "citext".into();
    email.type_schema = "public".into();
    email.type_name = "citext".into();
    email.type_kind = remus_core::model::TypeKind::Base;

    let orders = entity(&mut new, "shop.orders");
    let mut coupon = orders.columns[7].clone();
    coupon.name = "coupon_code".into();
    coupon.comment = Some("Applied at checkout".into());
    orders.columns.push(coupon);
    orders.columns[7].not_null = true;
    let mut index = orders.indexes[0].clone();
    index.name = "orders_coupon_idx".into();
    index.definition =
        "CREATE INDEX orders_coupon_idx ON shop.orders USING btree (coupon_code)".into();
    orders.indexes.push(index);
    let fk = orders
        .constraints
        .iter_mut()
        .find(|c| c.name == "orders_customer_id_fkey")
        .unwrap();
    fk.on_delete = Some(remus_core::model::ReferentialAction::Restrict);
    fk.definition = "FOREIGN KEY (customer_id) REFERENCES shop.customers(id) ON UPDATE RESTRICT ON DELETE RESTRICT".into();

    new.enums[0].labels.push("refunded".into());
    let money = new
        .domains
        .iter_mut()
        .find(|d| d.name == "money_cents")
        .unwrap();
    money.constraints = vec!["CHECK ((VALUE >= 0) AND (VALUE < 100000000))".into()];

    let mut next_year = new.partitions[0].clone();
    next_year.child = "events_2027".into();
    next_year.bound =
        Some("FOR VALUES FROM ('2027-01-01 00:00:00+00') TO ('2028-01-01 00:00:00+00')".into());
    new.partitions.push(next_year);

    new.extensions[1].version = "1.4".into();
    new.extensions.push(Extension {
        name: "citext".into(),
        schema: "public".into(),
        version: "1.6".into(),
    });
    (old, new)
}

#[test]
fn change_list() {
    let (old, new) = release();
    let d = diff(&old, &new);
    let mut text = format!("{}\n\n", d.summary.text);
    for line in d.lines() {
        text.push_str(line);
        text.push('\n');
    }
    insta::assert_snapshot!(text);
}

#[test]
fn release_json() {
    let (old, new) = release();
    insta::assert_snapshot!(serde_json::to_string_pretty(&diff(&old, &new)).unwrap());
}

/// The example in the crate documentation, in full.
#[test]
fn documented_example_json() {
    let old = Schema::from_json(
        r#"{
        "remus_version": "0.1", "database": "app", "server_version": "16",
        "entities": [{
            "schema": "public", "name": "users", "kind": "r", "comment": null,
            "partition_key": null, "view_definition": null,
            "columns": [{
                "name": "email", "type": "text", "type_schema": "pg_catalog",
                "type_name": "text", "type_kind": "b", "not_null": true,
                "default": null, "identity": null, "generated": null, "comment": null
            }]
        }]
    }"#,
    )
    .unwrap();
    let mut new = old.clone();
    let email = &mut new.entities[0].columns[0];
    email.r#type = "citext".into();
    email.type_schema = "public".into();
    email.type_name = "citext".into();
    insta::assert_snapshot!(serde_json::to_string_pretty(&diff(&old, &new)).unwrap());
}
