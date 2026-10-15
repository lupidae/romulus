//! Every change kind, on the remus showcase fixture and small hand-built
//! models. Each diff goes through [`check`], which also holds it to its
//! mirror image.

use remus_core::{
    Schema,
    emit::ident::{qualified, quote_ident},
    fixtures::SHOWCASE,
    model::{self as m, Entity},
};

use crate::{
    ColumnChange, ConstraintChange, ConstraintKind, DomainChange, EntityChange, EntityKind,
    EnumDiff, Generated, Identity, IndexChange, PartitionChange, PolicyChange, PolicyCommand,
    SchemaDiff, Status, diff, normalize, render_sql, sql,
};

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

fn column<'a>(schema: &'a mut Schema, qualified_name: &str, name: &str) -> &'a mut m::Column {
    entity(schema, qualified_name)
        .columns
        .iter_mut()
        .find(|c| c.name == name)
        .unwrap()
}

/// The diff, after checking that the other direction is its mirror and that
/// it survives a JSON round trip.
fn check(old: &Schema, new: &Schema) -> SchemaDiff {
    let forward = diff(old, new);
    assert_eq!(
        diff(new, old),
        forward.clone().reversed(),
        "not a mirror image"
    );
    assert_eq!(forward.clone().reversed().reversed(), forward);
    let json = serde_json::to_string(&forward).unwrap();
    assert_eq!(serde_json::from_str::<SchemaDiff>(&json).unwrap(), forward);
    forward
}

/// The one changed entity of a diff.
fn only_entity(d: &SchemaDiff) -> &crate::EntityDiff {
    assert_eq!(d.entities.len(), 1, "{:#?}", d.entities);
    &d.entities[0]
}

fn lines(d: &SchemaDiff) -> Vec<&str> {
    d.lines().collect()
}

fn empty() -> Schema {
    let mut schema = showcase();
    schema.entities.clear();
    schema.enums.clear();
    schema.domains.clear();
    schema.composites.clear();
    schema.partitions.clear();
    schema.extensions.clear();
    schema
}

#[test]
fn a_schema_has_no_diff_with_itself() {
    let d = check(&showcase(), &showcase());
    assert!(d.is_empty());
    assert_eq!(d.summary.text, "no changes");
    assert!(d.summary.counts.is_empty());
    assert_eq!(d.lines().count(), 0);
    let statuses = &d.status;
    assert_eq!(statuses.entities.len(), 13);
    assert!(statuses.entities.values().all(|e| {
        e.status == Status::Unchanged
            && e.columns.values().all(|s| *s == Status::Unchanged)
            && e.constraints.values().all(|s| *s == Status::Unchanged)
    }));
    assert_eq!(statuses.entities["shop.orders"].columns.len(), 8);
    assert_eq!(statuses.enums["shop.order_status"], Status::Unchanged);
    assert_eq!(statuses.domains.len(), 2);
    assert_eq!(statuses.composites["shop.address"], Status::Unchanged);
    assert_eq!(statuses.extensions["pgcrypto"], Status::Unchanged);
}

#[test]
fn metadata_is_not_a_change() {
    let mut new = showcase();
    new.generated_at = Some("2026-10-15T00:00:00Z".into());
    new.server_version = "17.0".into();
    new.database = "elsewhere".into();
    new.remus_version = "9.9".into();
    assert!(check(&showcase(), &new).is_empty());
}

#[test]
fn catalog_order_of_unordered_lists_is_not_a_change() {
    let mut new = showcase();
    new.entities.reverse();
    new.enums.reverse();
    new.domains.reverse();
    new.partitions.reverse();
    new.extensions.reverse();
    for e in &mut new.entities {
        e.constraints.reverse();
        e.indexes.reverse();
        e.policies.reverse();
        e.depends_on.reverse();
    }
    let roles = |schema: &mut Schema, roles: [&str; 2]| {
        let orders = entity(schema, "shop.orders");
        let owner = orders
            .policies
            .iter_mut()
            .find(|p| p.name == "orders_owner");
        owner.unwrap().roles = roles.map(String::from).to_vec();
    };
    roles(&mut new, ["zeta", "alpha"]);
    for d in &mut new.domains {
        d.constraints.push("CHECK ((VALUE IS NOT NULL))".into());
        d.constraints.reverse();
    }
    let mut old = showcase();
    roles(&mut old, ["alpha", "zeta"]);
    for d in &mut old.domains {
        d.constraints.push("CHECK ((VALUE IS NOT NULL))".into());
    }
    assert!(check(&old, &new).is_empty());
    assert_eq!(normalize(&old), normalize(&new));
}

#[test]
fn an_added_table_lists_everything_in_it_as_added() {
    let mut old = showcase();
    old.entities.retain(|e| e.qualified_name() != "shop.stock");
    let d = check(&old, &showcase());
    let stock = only_entity(&d);
    assert_eq!(stock.status, Status::Added);
    assert_eq!(stock.kind, EntityKind::Table);
    assert!(stock.changes.is_empty());
    assert_eq!(stock.columns.len(), 4);
    assert!(stock.columns.iter().all(|c| c.status == Status::Added));
    assert_eq!(stock.columns[0].name, "product_id");
    assert_eq!(stock.columns[0].position.after, Some(0));
    assert_eq!(stock.columns[0].position.before, None);
    assert_eq!(stock.constraints.len(), 3);
    assert_eq!(stock.lines, ["table `shop.stock` added"]);
    assert_eq!(d.summary.text, "1 table added");
    assert!(stock.sql.before.is_none());
    assert!(
        stock
            .sql
            .after
            .as_deref()
            .unwrap()
            .starts_with("CREATE TABLE shop.stock (")
    );
    let status = &d.status.entities["shop.stock"];
    assert_eq!(status.status, Status::Added);
    assert_eq!(status.columns["quantity"], Status::Added);
    assert_eq!(status.constraints["stock_warehouse_fkey"], Status::Added);
}

#[test]
fn a_removed_view_is_one_line() {
    let mut new = showcase();
    new.entities
        .retain(|e| e.qualified_name() != "shop.open_orders_by_customer");
    let d = check(&showcase(), &new);
    assert_eq!(only_entity(&d).status, Status::Removed);
    assert_eq!(lines(&d), ["view `shop.open_orders_by_customer` removed"]);
    assert_eq!(d.summary.text, "1 view removed");
    assert_eq!(
        d.status.entities["shop.open_orders_by_customer"].columns["email"],
        Status::Removed
    );
}

#[test]
fn a_rename_is_a_removal_and_an_addition() {
    let mut new = showcase();
    entity(&mut new, "shop.warehouses").name = "depots".into();
    let d = check(&showcase(), &new);
    let statuses: Vec<(&str, Status)> = d
        .entities
        .iter()
        .map(|e| (e.qualified_name.as_str(), e.status))
        .collect();
    assert_eq!(
        statuses,
        [
            ("shop.depots", Status::Added),
            ("shop.warehouses", Status::Removed)
        ]
    );
    assert_eq!(d.summary.text, "1 table added, 1 removed");
}

#[test]
fn a_kind_change_is_a_change_counted_under_the_new_kind() {
    let mut new = showcase();
    entity(&mut new, "shop.daily_totals").kind = m::EntityKind::View;
    let d = check(&showcase(), &new);
    let e = only_entity(&d);
    assert_eq!(e.kind, EntityKind::View);
    assert_eq!(
        e.changes,
        [EntityChange::Kind {
            before: EntityKind::MaterializedView,
            after: EntityKind::View
        }]
    );
    assert_eq!(e.lines, ["`shop.daily_totals`: materialized view → view"]);
    assert_eq!(d.summary.text, "1 view changed");
    assert_eq!(d.reversed().summary.text, "1 materialized view changed");
}

#[test]
fn entity_flags_and_comments() {
    let mut new = showcase();
    let orders = entity(&mut new, "shop.orders");
    orders.rls = false;
    orders.rls_forced = false;
    orders.unlogged = true;
    orders.comment = Some("Purchases".into());
    let d = check(&showcase(), &new);
    assert_eq!(
        only_entity(&d).lines,
        [
            "`shop.orders`: comment 'Purchases' added",
            "`shop.orders`: row level security disabled",
            "`shop.orders`: row level security no longer forced",
            "`shop.orders`: now unlogged",
        ]
    );
}

#[test]
fn partition_keys_view_definitions_and_dependencies() {
    let mut new = showcase();
    entity(&mut new, "audit.events").partition_key = Some("LIST (customer_id)".into());
    let view = entity(&mut new, "shop.open_orders_by_customer");
    view.view_definition = Some(" SELECT 1 AS one;".into());
    view.depends_on.retain(|r| r.name == "customers");
    view.depends_on.push(m::RelationRef {
        schema: "shop".into(),
        name: "orders".into(),
    });
    let d = check(&showcase(), &new);
    assert_eq!(
        lines(&d),
        [
            "`audit.events`: partition key RANGE (occurred_at) → LIST (customer_id)",
            "`shop.open_orders_by_customer`: definition changed",
            "`shop.open_orders_by_customer`: now reads `shop.orders`; no longer reads `shop.open_orders`",
        ]
    );
    let view = &d.entities[1];
    assert_eq!(
        view.changes[1],
        EntityChange::DependsOn {
            before: vec!["shop.customers".into(), "shop.open_orders".into()],
            after: vec!["shop.customers".into(), "shop.orders".into()],
        }
    );
}

#[test]
fn column_type_change_is_one_line() {
    let mut new = showcase();
    let note = column(&mut new, "shop.orders", "note");
    note.r#type = "character varying(200)".into();
    note.type_name = "varchar".into();
    let d = check(&showcase(), &new);
    let orders = only_entity(&d);
    assert_eq!(orders.status, Status::Changed);
    assert_eq!(orders.columns.len(), 1);
    assert_eq!(
        orders.columns[0].changes,
        [ColumnChange::Type {
            before: "text".into(),
            after: "character varying(200)".into()
        }]
    );
    assert_eq!(
        orders.columns[0].definition.after.as_deref(),
        Some("character varying(200)")
    );
    assert_eq!(
        orders.lines,
        ["`shop.orders.note`: type text → character varying(200)"]
    );
    assert_eq!(d.summary.text, "1 table changed");
    let status = &d.status.entities["shop.orders"];
    assert_eq!(status.status, Status::Changed);
    assert_eq!(status.columns["note"], Status::Changed);
    assert_eq!(status.columns["id"], Status::Unchanged);
}

#[test]
fn same_type_text_with_another_kind_is_reported() {
    let mut new = showcase();
    column(&mut new, "shop.orders", "status").type_kind = m::TypeKind::Domain;
    let d = check(&showcase(), &new);
    assert_eq!(lines(&d), ["`shop.orders.status`: type kind enum → domain"]);
}

#[test]
fn column_nullability_default_identity_and_comment() {
    let mut new = showcase();
    let c = column(&mut new, "shop.customers", "full_name");
    c.not_null = false;
    c.default = Some("''::text".into());
    c.comment = Some("As printed on invoices".into());
    let id = column(&mut new, "shop.customers", "id");
    id.identity = Some(m::Identity::ByDefault);
    let created = column(&mut new, "shop.customers", "created_at");
    created.default = None;
    let referrer = column(&mut new, "shop.customers", "referrer_id");
    referrer.identity = Some(m::Identity::Always);
    let d = check(&showcase(), &new);
    assert_eq!(
        only_entity(&d).lines,
        [
            "`shop.customers.id`: GENERATED ALWAYS AS IDENTITY → GENERATED BY DEFAULT AS IDENTITY",
            "`shop.customers.full_name`: now nullable",
            "`shop.customers.full_name`: default ''::text added",
            "`shop.customers.full_name`: comment 'As printed on invoices' added",
            "`shop.customers.referrer_id`: now GENERATED ALWAYS AS IDENTITY",
            "`shop.customers.created_at`: default now() removed",
        ]
    );
    let id = &only_entity(&d).columns[0];
    assert_eq!(
        id.changes,
        [ColumnChange::Identity {
            before: Some(Identity::Always),
            after: Some(Identity::ByDefault)
        }]
    );
}

#[test]
fn generated_columns_report_their_expression() {
    let mut new = showcase();
    let total = column(&mut new, "shop.orders", "total");
    total.default = Some("((subtotal)::bigint * 2)".into());
    let d = check(&showcase(), &new);
    let total = &only_entity(&d).columns[0];
    assert!(matches!(total.changes[0], ColumnChange::Expression { .. }));
    assert_eq!(
        only_entity(&d).lines,
        [
            "`shop.orders.total`: generation expression ((subtotal)::bigint + (tax)::bigint) → ((subtotal)::bigint * 2)"
        ]
    );

    let mut plain = showcase();
    let total = column(&mut plain, "shop.orders", "total");
    total.generated = None;
    total.default = None;
    let d = check(&showcase(), &plain);
    assert_eq!(
        only_entity(&d).columns[0].changes[1],
        ColumnChange::Generated {
            before: Some(Generated::Stored),
            after: None
        }
    );
    assert_eq!(
        only_entity(&d).lines[1],
        "`shop.orders.total`: no longer generated"
    );
}

#[test]
fn an_inserted_column_does_not_move_the_others() {
    let mut new = showcase();
    let orders = entity(&mut new, "shop.orders");
    let mut coupon = orders.columns[7].clone();
    coupon.name = "coupon".into();
    orders.columns.insert(1, coupon);
    let d = check(&showcase(), &new);
    let orders = only_entity(&d);
    assert_eq!(orders.columns.len(), 1);
    assert_eq!(orders.columns[0].status, Status::Added);
    assert_eq!(orders.columns[0].position.after, Some(1));
    assert_eq!(orders.lines, ["column `shop.orders.coupon` added: text"]);
}

#[test]
fn a_moved_column_is_the_only_one_reported() {
    let mut new = showcase();
    let orders = entity(&mut new, "shop.orders");
    let note = orders.columns.remove(7);
    orders.columns.insert(0, note);
    let d = check(&showcase(), &new);
    let orders = only_entity(&d);
    assert_eq!(orders.columns.len(), 1);
    assert_eq!(
        orders.columns[0].changes,
        [ColumnChange::Position {
            before: 7,
            after: 0
        }]
    );
    assert_eq!(
        orders.lines,
        ["`shop.orders.note`: moved from position 8 to 1"]
    );
}

#[test]
fn changed_columns_come_in_table_order() {
    let mut new = showcase();
    let orders = entity(&mut new, "shop.orders");
    orders.columns.retain(|c| c.name != "status");
    orders.columns[6].comment = Some("free text".into());
    orders.columns[0].comment = Some("key".into());
    let d = check(&showcase(), &new);
    let names: Vec<&str> = only_entity(&d)
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(names, ["id", "status", "note"]);
}

#[test]
fn constraints_added_removed_and_redefined() {
    let mut new = showcase();
    let orders = entity(&mut new, "shop.orders");
    orders
        .constraints
        .retain(|c| c.name != "orders_placed_when_not_draft");
    let fk = orders
        .constraints
        .iter_mut()
        .find(|c| c.name == "orders_customer_id_fkey")
        .unwrap();
    fk.on_delete = Some(m::ReferentialAction::Restrict);
    fk.definition = fk
        .definition
        .replace("ON DELETE CASCADE", "ON DELETE RESTRICT");
    fk.comment = Some("owner".into());
    let mut check_constraint = showcase().entities[0].constraints[0].clone();
    check_constraint.name = "orders_note_short".into();
    check_constraint.kind = m::ConstraintKind::Check;
    check_constraint.columns.clear();
    check_constraint.definition = "CHECK ((length(note) < 500))".into();
    orders.constraints.push(check_constraint);
    let d = check(&showcase(), &new);
    let orders = only_entity(&d);
    let names: Vec<(&str, Status)> = orders
        .constraints
        .iter()
        .map(|c| (c.name.as_str(), c.status))
        .collect();
    assert_eq!(
        names,
        [
            ("orders_customer_id_fkey", Status::Changed),
            ("orders_note_short", Status::Added),
            ("orders_placed_when_not_draft", Status::Removed),
        ]
    );
    assert_eq!(
        orders.lines,
        [
            // Long definitions lose their shared words, never the difference.
            "foreign key `orders_customer_id_fkey` on `shop.orders`: …ON DELETE CASCADE → …ON DELETE RESTRICT",
            "foreign key `orders_customer_id_fkey` on `shop.orders`: comment 'owner' added",
            "check constraint `orders_note_short` on `shop.orders` added: CHECK ((length(note) < 500))",
            "check constraint `orders_placed_when_not_draft` on `shop.orders` removed",
        ]
    );
    assert_eq!(
        d.status.entities["shop.orders"].constraints["orders_placed_when_not_draft"],
        Status::Removed
    );
}

#[test]
fn a_structural_constraint_change_behind_the_same_text_counts() {
    let mut new = showcase();
    let fk = entity(&mut new, "shop.stock")
        .constraints
        .iter_mut()
        .find(|c| c.name == "stock_warehouse_fkey")
        .unwrap();
    fk.deferrable = false;
    let d = check(&showcase(), &new);
    assert!(matches!(
        only_entity(&d).constraints[0].changes[..],
        [ConstraintChange::Definition { .. }]
    ));
}

#[test]
fn a_constraint_can_change_kind() {
    let mut new = showcase();
    let key = entity(&mut new, "shop.products")
        .constraints
        .iter_mut()
        .find(|c| c.name == "products_sku_key")
        .unwrap();
    key.kind = m::ConstraintKind::Exclusion;
    key.definition = "EXCLUDE USING btree (sku WITH =)".into();
    let d = check(&showcase(), &new);
    let c = &only_entity(&d).constraints[0];
    assert_eq!(c.kind, ConstraintKind::Exclusion);
    assert_eq!(
        only_entity(&d).lines[0],
        "exclusion constraint `products_sku_key` on `shop.products`: unique constraint → exclusion constraint"
    );
    assert_eq!(
        d.reversed().entities[0].constraints[0].kind,
        ConstraintKind::Unique
    );
}

#[test]
fn indexes_added_removed_and_redefined() {
    let mut new = showcase();
    let orders = entity(&mut new, "shop.orders");
    orders.indexes.retain(|i| i.name != "orders_open_idx");
    orders.indexes[0].definition = orders.indexes[0].definition.replace("btree", "hash");
    let mut placed = orders.indexes[0].clone();
    placed.name = "orders_placed_idx".into();
    placed.definition =
        "CREATE INDEX orders_placed_idx ON shop.orders USING btree (placed_at)".into();
    orders.indexes.push(placed);
    let d = check(&showcase(), &new);
    assert_eq!(
        only_entity(&d).lines,
        [
            "index `orders_customer_idx` on `shop.orders`: CREATE INDEX orders_customer_idx ON shop.orders USING btree (customer_id) → CREATE INDEX orders_customer_idx ON shop.orders USING hash (customer_id)",
            "index `orders_open_idx` on `shop.orders` removed",
            "index `orders_placed_idx` on `shop.orders` added: CREATE INDEX orders_placed_idx ON shop.orders USING btree (placed_at)",
        ]
    );
    assert_eq!(
        only_entity(&d).indexes[0].changes[0],
        IndexChange::Definition {
            before: "CREATE INDEX orders_customer_idx ON shop.orders USING btree (customer_id)"
                .into(),
            after: "CREATE INDEX orders_customer_idx ON shop.orders USING hash (customer_id)"
                .into(),
        }
    );

    let mut flagged = showcase();
    entity(&mut flagged, "shop.orders").indexes[1].unique = false;
    let d = check(&showcase(), &flagged);
    assert_eq!(only_entity(&d).indexes[0].name, "orders_note_lower_idx");
}

#[test]
fn policies_every_field() {
    let mut new = showcase();
    let orders = entity(&mut new, "shop.orders");
    let owner = orders
        .policies
        .iter_mut()
        .find(|p| p.name == "orders_owner")
        .unwrap();
    owner.command = m::PolicyCommand::Update;
    owner.permissive = false;
    owner.roles = vec!["app".into(), "admin".into()];
    owner.using = None;
    owner.check = Some("true".into());
    orders.policies.retain(|p| p.name != "orders_no_cancelled");
    let d = check(&showcase(), &new);
    let orders = only_entity(&d);
    assert_eq!(
        orders.policies[1].changes[..3],
        [
            PolicyChange::Command {
                before: PolicyCommand::All,
                after: PolicyCommand::Update,
            },
            PolicyChange::Permissive {
                before: true,
                after: false
            },
            PolicyChange::Roles {
                before: vec![],
                after: vec!["admin".into(), "app".into()]
            },
        ]
    );
    assert_eq!(
        orders.lines[0],
        "policy `orders_no_cancelled` on `shop.orders` removed"
    );
    assert_eq!(
        orders.lines[1],
        "policy `orders_owner` on `shop.orders`: command ALL → UPDATE"
    );
    assert_eq!(
        orders.lines[2],
        "policy `orders_owner` on `shop.orders`: now restrictive"
    );
    assert_eq!(
        orders.lines[3],
        "policy `orders_owner` on `shop.orders`: roles PUBLIC → admin, app"
    );
    assert!(
        orders.lines[4].starts_with("policy `orders_owner` on `shop.orders`: USING (customer_id")
    );
    assert!(orders.lines[4].ends_with(" removed"));
    assert!(
        orders.lines[5]
            .starts_with("policy `orders_owner` on `shop.orders`: WITH CHECK (customer_id")
    );
    assert!(orders.lines[5].ends_with("→ true"));
    assert_eq!(
        orders.policies[1].definition.after.as_deref(),
        Some("AS RESTRICTIVE FOR UPDATE TO admin, app WITH CHECK true")
    );
}

#[test]
fn enum_labels_added_removed_and_reordered() {
    let mut new = showcase();
    let status = &mut new.enums[0];
    status.labels = vec![
        "draft".into(),
        "shipped".into(),
        "placed".into(),
        "refunded".into(),
    ];
    status.comment = None;
    let d = check(&showcase(), &new);
    let EnumDiff { labels, lines, .. } = &d.enums[0];
    let labels: Vec<(&str, Status)> = labels
        .iter()
        .map(|l| (l.label.as_str(), l.status))
        .collect();
    assert_eq!(
        labels,
        [
            ("placed", Status::Changed),
            ("shipped", Status::Changed),
            ("cancelled", Status::Removed),
            ("refunded", Status::Added),
        ]
    );
    assert_eq!(
        lines,
        &[
            "`shop.order_status`: comment 'Lifecycle of an order' removed",
            "`shop.order_status`: label 'placed' moved from position 2 to 3",
            "`shop.order_status`: label 'shipped' moved from position 3 to 2",
            "`shop.order_status`: label 'cancelled' removed",
            "`shop.order_status`: label 'refunded' added",
        ]
    );
    assert_eq!(d.status.enums["shop.order_status"], Status::Changed);
    assert_eq!(d.summary.text, "1 enum changed");
    // The table using the enum is untouched: its model did not change.
    assert!(d.entities.is_empty());
}

#[test]
fn enums_added_and_removed() {
    let mut new = showcase();
    let mut mood = new.enums[0].clone();
    mood.name = "mood".into();
    new.enums.push(mood);
    let d = check(&showcase(), &new);
    assert_eq!(lines(&d), ["enum `shop.mood` added"]);
    assert_eq!(d.enums[0].labels.len(), 4);
    assert_eq!(
        d.enums[0].sql.after.as_deref(),
        Some(
            "CREATE TYPE shop.mood AS ENUM ('draft', 'placed', 'shipped', 'cancelled');\n\n\
             COMMENT ON TYPE shop.mood IS 'Lifecycle of an order';\n"
        )
    );
}

#[test]
fn domains_every_field() {
    let mut new = showcase();
    let money = new
        .domains
        .iter_mut()
        .find(|d| d.name == "money_cents")
        .unwrap();
    money.base_type = "numeric".into();
    money.not_null = false;
    money.default = None;
    money.constraints = vec!["CHECK ((VALUE >= (0)::numeric))".into()];
    money.comment = Some("Amounts in cents".into());
    let d = check(&showcase(), &new);
    assert_eq!(
        d.domains[0].changes[0],
        DomainChange::BaseType {
            before: "bigint".into(),
            after: "numeric".into()
        }
    );
    assert_eq!(
        lines(&d),
        [
            "`shop.money_cents`: base type bigint → numeric",
            "`shop.money_cents`: now nullable",
            "`shop.money_cents`: default 0 removed",
            "`shop.money_cents`: check added: CHECK ((VALUE >= (0)::numeric))",
            "`shop.money_cents`: check removed: CHECK ((VALUE >= 0))",
            "`shop.money_cents`: comment 'Amounts are integers of cents' → 'Amounts in cents'",
        ]
    );
    assert_eq!(d.summary.text, "1 domain changed");
}

#[test]
fn composite_attributes() {
    let mut new = showcase();
    let address = &mut new.composites[0];
    address.attributes.swap(0, 1);
    address.attributes[2].r#type = "character varying(10)".into();
    address.attributes.push(m::CompositeAttribute {
        name: "country".into(),
        r#type: "text".into(),
    });
    address.comment = Some("Postal".into());
    let d = check(&showcase(), &new);
    assert_eq!(
        lines(&d),
        [
            "`shop.address`: comment 'Postal' added",
            // Both span positions 1 and 2; the name breaks the tie.
            "`shop.address.city`: moved from position 2 to 1",
            "`shop.address.street`: moved from position 1 to 2",
            "`shop.address.zip code`: type text → character varying(10)",
            "attribute `shop.address.country` added: text",
        ]
    );
}

#[test]
fn partitions_live_under_their_parent() {
    let mut new = showcase();
    let mut next = new.partitions[0].clone();
    next.child = "events_2027".into();
    next.bound =
        Some("FOR VALUES FROM ('2027-01-01 00:00:00+00') TO ('2028-01-01 00:00:00+00')".into());
    new.partitions.push(next);
    new.partitions.retain(|p| p.child != "events_default");
    new.partitions[0].bound =
        Some("FOR VALUES FROM ('2026-01-01 00:00:00+00') TO ('2026-07-01 00:00:00+00')".into());
    let d = check(&showcase(), &new);
    let events = only_entity(&d);
    assert_eq!(events.qualified_name, "audit.events");
    assert_eq!(events.status, Status::Changed);
    assert!(events.columns.is_empty());
    let parts: Vec<(&str, Status)> = events
        .partitions
        .iter()
        .map(|p| (p.name.as_str(), p.status))
        .collect();
    assert_eq!(
        parts,
        [
            ("events_2026", Status::Changed),
            ("events_2027", Status::Added),
            ("events_default", Status::Removed),
        ]
    );
    assert!(matches!(
        events.partitions[0].changes[..],
        [PartitionChange::Bound { .. }]
    ));
    assert_eq!(
        events.lines[1],
        "partition `audit.events_2027` of `audit.events` added: FOR VALUES FROM ('2027-01-01 00:00:00+00') TO ('2028-01-01 00:00:00+00')"
    );
    assert_eq!(
        events.lines[2],
        "partition `audit.events_default` of `audit.events` removed"
    );
    assert!(
        events
            .sql
            .after
            .as_deref()
            .unwrap()
            .contains("CREATE TABLE audit.events_2027 PARTITION OF audit.events FOR VALUES")
    );
    assert_eq!(
        d.summary.text,
        "1 table changed; 1 partition added, 1 changed, 1 removed"
    );
    assert_eq!(d.status.entities["audit.events"].status, Status::Changed);
}

#[test]
fn removing_a_partitioned_table_removes_its_partitions() {
    let mut new = showcase();
    new.entities
        .retain(|e| e.qualified_name() != "audit.events");
    new.partitions.clear();
    let d = check(&showcase(), &new);
    assert_eq!(only_entity(&d).partitions.len(), 2);
    assert_eq!(d.summary.text, "1 table removed; 2 partitions removed");
    assert_eq!(lines(&d), ["partitioned table `audit.events` removed"]);
}

#[test]
fn partitions_of_no_entity_are_orphans() {
    let mut old = showcase();
    old.entities
        .retain(|e| e.qualified_name() != "audit.events");
    let mut new = old.clone();
    new.partitions.pop();
    let d = check(&old, &new);
    assert!(d.entities.is_empty());
    assert_eq!(d.orphan_partitions.len(), 1);
    assert_eq!(d.orphan_partitions[0].parent, "audit.events");
    assert_eq!(d.summary.text, "1 partition removed");
    assert!(!d.is_empty());
}

#[test]
fn extensions_added_removed_and_upgraded() {
    let mut new = showcase();
    new.extensions[1].version = "1.4".into();
    new.extensions[0].schema = "extensions".into();
    new.extensions.push(m::Extension {
        name: "citext".into(),
        schema: "public".into(),
        version: "1.6".into(),
    });
    let d = check(&showcase(), &new);
    assert_eq!(
        lines(&d),
        [
            "`btree_gist`: schema public → extensions",
            "extension `citext` added",
            "`pgcrypto`: version 1.3 → 1.4",
        ]
    );
    assert_eq!(
        d.extensions[1].sql.after.as_deref(),
        Some("CREATE EXTENSION IF NOT EXISTS citext WITH SCHEMA public;\n")
    );
    assert_eq!(d.summary.text, "1 extension added, 2 changed");
}

#[test]
fn summary_puts_the_noun_on_the_first_number() {
    let mut old = showcase();
    old.entities.retain(|e| {
        !["shop.stock", "shop.warehouses", "shop.promotions"].contains(&e.qualified_name().as_str())
    });
    let mut new = showcase();
    column(&mut new, "shop.orders", "note").not_null = true;
    new.enums.clear();
    let d = check(&old, &new);
    assert_eq!(d.summary.text, "3 tables added, 1 changed; 1 enum removed");
    let reversed = d.reversed();
    assert_eq!(
        reversed.summary.text,
        "1 table changed, 3 removed; 1 enum added"
    );
}

#[test]
fn object_sql_is_what_remus_sql_renders_for_it_alone() {
    let mut new = showcase();
    column(&mut new, "shop.orders", "note").comment = Some("free text".into());
    let d = check(&showcase(), &new);
    let after = only_entity(&d).sql.after.as_deref().unwrap();
    assert!(after.starts_with(
        "CREATE SEQUENCE IF NOT EXISTS shop.orders_id_seq;\n\nCREATE TABLE shop.orders (\n"
    ));
    assert!(!after.contains("-- Schema DDL"));
    assert!(!after.contains("CREATE SCHEMA"));
    assert!(
        after
            .contains("ALTER TABLE shop.orders ADD CONSTRAINT orders_customer_id_fkey FOREIGN KEY")
    );
    assert!(after.contains("CREATE INDEX orders_customer_idx ON shop.orders"));
    assert!(after.contains("ALTER TABLE shop.orders ENABLE ROW LEVEL SECURITY;"));
    assert!(after.contains("COMMENT ON COLUMN shop.orders.note IS 'free text';"));
    assert!(after.ends_with(";\n"));
    let before = only_entity(&d).sql.before.as_deref().unwrap();
    assert_eq!(
        after.replace("\nCOMMENT ON COLUMN shop.orders.note IS 'free text';\n", ""),
        before
    );
    assert!(
        !after.contains("shop.customers ("),
        "another table leaked in"
    );
}

/// The one-line definitions are rebuilt here rather than taken from
/// `remus-sql` (which keeps them private); this keeps them from drifting.
#[test]
fn part_definitions_match_what_remus_sql_renders() {
    let d = check(&empty(), &showcase());
    assert_eq!(d.entities.len(), 13);
    for e in &d.entities {
        let sql = e.sql.after.as_deref().unwrap();
        let table = qualified(&e.schema, &e.name);
        if matches!(e.kind, EntityKind::Table | EntityKind::PartitionedTable) {
            for c in &e.columns {
                let line = format!(
                    "    {} {}",
                    quote_ident(&c.name),
                    c.definition.after.as_deref().unwrap()
                );
                assert!(sql.contains(&line), "{line:?} not in\n{sql}");
            }
        }
        for c in &e.constraints {
            let definition = c.definition.after.as_deref().unwrap();
            let line = if c.kind == ConstraintKind::ForeignKey {
                format!(
                    "ALTER TABLE {table} ADD CONSTRAINT {} {definition};",
                    quote_ident(&c.name)
                )
            } else {
                format!("CONSTRAINT {} {definition}", quote_ident(&c.name))
            };
            assert!(sql.contains(&line), "{line:?} not in\n{sql}");
        }
        for i in &e.indexes {
            assert!(sql.contains(&format!("{};", i.definition.after.as_deref().unwrap())));
        }
        for p in &e.policies {
            let line = format!(
                "CREATE POLICY {} ON {table} {};",
                quote_ident(&p.name),
                p.definition.after.as_deref().unwrap()
            );
            assert!(sql.contains(&line), "{line:?} not in\n{sql}");
        }
        for p in &e.partitions {
            let line = format!(
                "PARTITION OF {table} {};",
                p.definition.after.as_deref().unwrap()
            );
            assert!(sql.contains(&line), "{line:?} not in\n{sql}");
        }
    }
    let orders = d.entities.iter().find(|e| e.name == "orders").unwrap();
    assert_eq!(
        orders.columns[5].definition.after.as_deref(),
        Some("bigint GENERATED ALWAYS AS (((subtotal)::bigint + (tax)::bigint)) STORED")
    );
    assert_eq!(
        d.summary.text,
        "10 tables added; 2 views added; 1 materialized view added; 2 partitions added; \
         1 enum added; 2 domains added; 1 composite type added; 2 extensions added"
    );
}

#[test]
fn whole_schemas_render_in_one_order() {
    let mut shuffled = showcase();
    shuffled.entities.reverse();
    shuffled.partitions.reverse();
    shuffled.domains.reverse();
    for e in &mut shuffled.entities {
        e.constraints.reverse();
        e.indexes.reverse();
        e.policies.reverse();
    }
    assert_eq!(render_sql(&shuffled), render_sql(&showcase()));
    // The fixture is already in introspection order, so normalising it
    // changes nothing remus would print.
    assert_eq!(render_sql(&showcase()), remus_sql::render(&showcase()));
}

#[test]
fn every_status_map_entry_matches_the_lists() {
    let mut new = showcase();
    column(&mut new, "shop.orders", "note").not_null = true;
    new.entities.retain(|e| e.name != "scratch");
    let d = check(&showcase(), &new);
    for e in &d.entities {
        let status = &d.status.entities[&e.qualified_name];
        assert_eq!(status.status, e.status);
        for c in &e.columns {
            assert_eq!(status.columns[&c.name], c.status);
        }
    }
    let changed: usize = d
        .status
        .entities
        .values()
        .filter(|e| e.status != Status::Unchanged)
        .count();
    assert_eq!(changed, d.entities.len());
}

#[test]
fn duplicate_names_do_not_panic() {
    let mut new = showcase();
    let orders = entity(&mut new, "shop.orders");
    let twin = orders.columns[0].clone();
    orders.columns.push(twin);
    let dup = new.entities[0].clone();
    new.entities.push(dup);
    let d = check(&showcase(), &new);
    assert!(
        d.is_empty(),
        "the first of each name wins: {:#?}",
        d.entities
    );
}

#[test]
fn long_values_are_shortened_in_lines_only() {
    let mut new = showcase();
    let long = "x".repeat(200);
    entity(&mut new, "shop.orders").comment = Some(format!("{long}\n\nsecond paragraph"));
    let d = check(&showcase(), &new);
    let line = &only_entity(&d).lines[0];
    assert!(line.chars().count() < 120, "{line}");
    assert!(line.contains('…'));
    assert!(!line.contains('\n'));
    assert!(matches!(
        &only_entity(&d).changes[0],
        EntityChange::Comment { after: Some(full), .. } if full.len() > 200
    ));
}

#[test]
fn the_object_sql_helpers_cut_only_the_header() {
    let mut schema = empty();
    schema.extensions.push(m::Extension {
        name: "vector".into(),
        schema: "ai".into(),
        version: "0.8".into(),
    });
    let x = &schema.extensions[0];
    assert_eq!(
        sql::extension_sql(&schema, x),
        "CREATE EXTENSION IF NOT EXISTS vector WITH SCHEMA ai;\n"
    );
}
