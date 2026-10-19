use remus_core::Schema;
use serde_json::{Value, json};

use super::*;

/// A column as remus writes it: `("email", "text")`.
fn column(name: &str, ty: &str) -> Value {
    json!({ "name": name, "type": ty, "type_schema": "pg_catalog", "type_name": ty,
            "type_kind": "b", "not_null": false, "default": null, "identity": null,
            "generated": null, "comment": null })
}

fn table(name: &str, columns: &[(&str, &str)]) -> Value {
    json!({ "schema": "public", "name": name, "kind": "r", "comment": null,
            "partition_key": null, "view_definition": null,
            "columns": columns.iter().map(|(n, t)| column(n, t)).collect::<Vec<_>>() })
}

fn with_fk(mut table: Value, name: &str, column: &str, target: &str) -> Value {
    table["constraints"] = json!([{
        "name": name, "kind": "f", "columns": [column], "ref_schema": "public",
        "ref_table": target, "ref_columns": ["id"], "on_update": "a", "on_delete": "a",
        "deferrable": false,
        "definition": format!("FOREIGN KEY ({column}) REFERENCES public.{target}(id)"),
        "comment": null
    }]);
    table
}

fn model(entities: &[Value]) -> Schema {
    model_with(entities, &json!([]))
}

fn model_with(entities: &[Value], enums: &Value) -> Schema {
    Schema::from_json(
        &json!({ "remus_version": "0.2", "database": "app", "server_version": "17",
                 "entities": entities, "enums": enums })
        .to_string(),
    )
    .unwrap()
}

/// The history of `versions`, each diffed with the one before (the first
/// with nothing).
fn history(versions: &[Schema]) -> History {
    let empty = crate::versions::empty(&versions[0]);
    let steps: Vec<Step> = versions
        .iter()
        .enumerate()
        .map(|(i, v)| step(if i == 0 { &empty } else { &versions[i - 1] }, v))
        .collect();
    fold(&steps.iter().collect::<Vec<_>>())
}

fn spans(states: &[ObjectState]) -> Vec<(u32, Option<u32>)> {
    states.iter().map(|s| (s.from, s.to)).collect()
}

fn value(state: &ObjectState) -> Value {
    serde_json::from_str(state.value.get()).unwrap()
}

fn changes(state: &ObjectState) -> Value {
    serde_json::to_value(&state.changes).unwrap()
}

fn find_table<'h>(history: &'h History, id: &str) -> &'h TableHistory {
    history.tables.iter().find(|t| t.id == id).unwrap()
}

fn find_part<'h>(parts: &'h [PartHistory], id: &str) -> &'h PartHistory {
    parts.iter().find(|p| p.id == id).unwrap()
}

#[test]
fn each_object_has_its_additions_removals_and_changes() {
    let v1 = model(&[table("users", &[("id", "bigint"), ("email", "text")])]);
    let v2 = model(&[
        table(
            "users",
            &[("id", "bigint"), ("email", "text"), ("name", "text")],
        ),
        with_fk(
            table("orders", &[("id", "bigint"), ("user_id", "bigint")]),
            "orders_user_id_fkey",
            "user_id",
            "users",
        ),
    ]);
    let v3 = model(&[table(
        "users",
        &[("id", "bigint"), ("email", "citext"), ("name", "text")],
    )]);
    let h = history(&[v1, v2, v3]);

    assert_eq!(
        h.tables.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        ["public.orders", "public.users"]
    );
    let users = find_table(&h, "public.users");
    // A column added: a new head (its column order), with no change of its own.
    assert_eq!(spans(&users.states), [(0, Some(1)), (1, None)]);
    assert_eq!(changes(&users.states[1]), json!([]));
    assert_eq!(
        value(&users.states[1])["column_order"],
        json!(["id", "email", "name"])
    );
    assert!(value(&users.states[0]).get("columns").is_none());

    let email = find_part(&users.columns, "email");
    assert_eq!(spans(&email.states), [(0, Some(2)), (2, None)]);
    assert_eq!(value(&email.states[0])["type"], "text");
    assert_eq!(value(&email.states[1])["type"], "citext");
    assert_eq!(
        changes(&email.states[1]),
        json!([{ "field": "type", "before": "text", "after": "citext" }])
    );
    assert_eq!(
        spans(&find_part(&users.columns, "name").states),
        [(1, None)]
    );
    assert_eq!(spans(&find_part(&users.columns, "id").states), [(0, None)]);

    // Removed with its table: the table and every part end at v3.
    let orders = find_table(&h, "public.orders");
    assert_eq!(spans(&orders.states), [(1, Some(2))]);
    assert!(
        orders
            .columns
            .iter()
            .all(|c| spans(&c.states) == [(1, Some(2))])
    );
    assert_eq!(
        spans(&find_part(&orders.constraints, "orders_user_id_fkey").states),
        [(1, Some(2))]
    );
}

#[test]
fn an_object_removed_then_added_again_is_one_object_with_a_gap() {
    let with = model(&[
        table("users", &[("id", "bigint")]),
        table("legacy", &[("id", "bigint")]),
    ]);
    let without = model(&[table("users", &[("id", "bigint")])]);
    let h = history(&[with.clone(), without, with]);
    let legacy = find_table(&h, "public.legacy");
    assert_eq!(spans(&legacy.states), [(0, Some(1)), (2, None)]);
    // Back again is an addition, not a change.
    assert_eq!(changes(&legacy.states[1]), json!([]));
    assert_eq!(
        spans(&find_part(&legacy.columns, "id").states),
        [(0, Some(1)), (2, None)]
    );
}

#[test]
fn a_rename_is_a_removal_and_an_addition_while_the_diff_reports_none() {
    let before = model(&[table("users", &[("id", "bigint"), ("email", "text")])]);
    let after = model(&[table(
        "customers",
        &[("id", "bigint"), ("email_address", "text")],
    )]);
    let h = history(&[before, after]);
    assert_eq!(
        spans(&find_table(&h, "public.users").states),
        [(0, Some(1))]
    );
    assert_eq!(
        spans(&find_table(&h, "public.customers").states),
        [(1, None)]
    );
}

#[test]
fn renames_the_diff_reports_continue_the_objects_history_under_the_new_name() {
    let v1 = model(&[
        table("users", &[("id", "bigint"), ("email", "text")]),
        table("plans", &[("id", "bigint"), ("label", "text")]),
    ]);
    let v2 = model(&[
        table("customers", &[("id", "bigint"), ("email", "text")]),
        table("plans", &[("id", "bigint"), ("title", "varchar")]),
    ]);
    let empty = crate::versions::empty(&v1);
    let first = step(&empty, &v1);
    let diff = romulus_diff::diff(&v1, &v2);
    let relation = |name: &str| Relation {
        schema: "public".into(),
        name: name.into(),
    };
    let renames = Renames {
        tables: vec![(relation("users"), relation("customers"))],
        columns: vec![ColumnRename {
            table: relation("plans"),
            from: "label".into(),
            to: "title".into(),
        }],
    };
    let second = step_with(&v1, &v2, &diff, &renames);
    let h = fold(&[&first, &second]);

    // One table, known by its last name, renamed at v2.
    assert_eq!(
        h.tables.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        ["public.customers", "public.plans"]
    );
    let customers = find_table(&h, "public.customers");
    assert_eq!(spans(&customers.states), [(0, Some(1)), (1, None)]);
    assert_eq!(value(&customers.states[0])["name"], "users");
    assert_eq!(
        changes(&customers.states[1]),
        json!([{ "field": "name", "before": "users", "after": "customers" }])
    );
    // Its columns carry on, untouched.
    assert_eq!(
        spans(&find_part(&customers.columns, "email").states),
        [(0, None)]
    );

    // A column renamed (and retyped) in a table that stays.
    let plans = find_table(&h, "public.plans");
    let title = find_part(&plans.columns, "title");
    assert_eq!(spans(&title.states), [(0, Some(1)), (1, None)]);
    assert_eq!(value(&title.states[0])["name"], "label");
    assert_eq!(
        changes(&title.states[1]),
        json!([
            { "field": "name", "before": "label", "after": "title" },
            { "field": "type", "before": "text", "after": "varchar" }
        ])
    );
    assert!(plans.columns.iter().all(|c| c.id != "label"));
}

#[test]
fn a_rename_the_diff_does_not_back_is_ignored() {
    // `users` is still there: not a rename, whatever the seam says.
    let v1 = model(&[table("users", &[("id", "bigint")])]);
    let v2 = model(&[
        table("users", &[("id", "bigint")]),
        table("customers", &[("id", "bigint")]),
    ]);
    let diff = romulus_diff::diff(&v1, &v2);
    let relation = |name: &str| Relation {
        schema: "public".into(),
        name: name.into(),
    };
    let renames = Renames {
        tables: vec![(relation("users"), relation("customers"))],
        columns: Vec::new(),
    };
    let s = step_with(&v1, &v2, &diff, &renames);
    assert_eq!(s.tables.len(), 1);
    assert_eq!(s.tables[0].head.old, None);
}

#[test]
fn enum_labels_are_a_change_of_the_type() {
    let status = |labels: &[&str]| json!([{ "schema": "public", "name": "status", "labels": labels, "comment": null }]);
    let v1 = model_with(&[], &status(&["open"]));
    let v2 = model_with(&[], &status(&["open", "closed"]));
    let h = history(&[v1, v2]);
    assert_eq!(h.types.len(), 1);
    let t = &h.types[0];
    assert_eq!(t.kind, UserTypeKind::Enum);
    assert_eq!(spans(&t.states), [(0, Some(1)), (1, None)]);
    assert_eq!(
        changes(&t.states[1]),
        json!([{ "field": "labels", "before": ["open"], "after": ["open", "closed"] }])
    );
}

#[test]
fn an_unchanged_version_changes_no_state() {
    let v1 = model(&[table("users", &[("id", "bigint")])]);
    let h = history(&[v1.clone(), v1.clone(), v1]);
    let users = find_table(&h, "public.users");
    assert_eq!(spans(&users.states), [(0, None)]);
}

#[test]
fn a_step_weighs_what_it_holds() {
    let v1 = model(&[table("users", &[("id", "bigint"), ("email", "text")])]);
    let s = step(&crate::versions::empty(&v1), &v1);
    let values: usize = s
        .tables
        .iter()
        .flat_map(|t| std::iter::once(&t.head).chain(t.parts.iter().map(|(_, e)| e)))
        .map(|e| e.value.as_ref().map_or(0, |v| v.get().len()))
        .sum();
    assert!(s.weight() > values);
}
