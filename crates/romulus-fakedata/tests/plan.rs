//! The plan without a database: determinism, load order and cycles, keys
//! unique by construction, CHECKs, partitions, the report.

use std::collections::{BTreeMap, HashMap, HashSet};

use remus_core::{Schema, fixtures::SHOWCASE};
use romulus_fakedata::{
    DropReason, FallbackKind, Options, Plan, Profile, Sample, Size, Source, Step, plan,
    plan_with_rows,
};
use serde_json::{Value, json};

fn column(name: &str, ty: &str, catalog: &str, not_null: bool) -> Value {
    json!({
        "name": name, "type": ty, "type_schema": "pg_catalog", "type_name": catalog,
        "type_kind": "b", "is_array": ty.ends_with("[]"), "not_null": not_null,
        "default": null, "identity": null, "generated": null, "comment": null
    })
}

fn constraint(name: &str, kind: &str, columns: &[&str], definition: &str) -> Value {
    json!({
        "name": name, "kind": kind, "columns": columns, "ref_schema": null, "ref_table": null,
        "ref_columns": [], "on_update": null, "on_delete": null, "deferrable": false,
        "definition": definition, "comment": null
    })
}

fn fk(name: &str, columns: &[&str], parent: &str, ref_columns: &[&str]) -> Value {
    json!({
        "name": name, "kind": "f", "columns": columns, "ref_schema": "public", "ref_table": parent,
        "ref_columns": ref_columns, "on_update": null, "on_delete": null, "deferrable": false,
        "definition": format!("FOREIGN KEY ({}) REFERENCES public.{parent}({})", columns.join(", "), ref_columns.join(", ")),
        "comment": null
    })
}

#[allow(clippy::needless_pass_by_value)]
fn table(name: &str, columns: Vec<Value>, constraints: Vec<Value>) -> Value {
    json!({
        "schema": "public", "name": name, "kind": "r", "comment": null, "partition_key": null,
        "view_definition": null, "columns": columns, "constraints": constraints
    })
}

#[allow(clippy::needless_pass_by_value)]
fn model(entities: Vec<Value>, extra: Value) -> Schema {
    let mut m = json!({
        "remus_version": "0.2", "database": "t", "server_version": "17", "entities": entities
    });
    if let (Some(m), Some(extra)) = (m.as_object_mut(), extra.as_object()) {
        for (k, v) in extra {
            m.insert(k.clone(), v.clone());
        }
    }
    Schema::from_json(&m.to_string()).expect("a model")
}

fn options(seed: u64) -> Options {
    Options::new(Size::Small, Profile::Realistic, seed)
}

/// Every row of every table, by table.
fn rows(plan: &Plan) -> HashMap<String, Sample> {
    plan.report()
        .tables
        .iter()
        .filter_map(|t| {
            plan.sample(&t.schema, &t.table, u64::MAX)
                .map(|r| (format!("{}.{}", t.schema, t.table), r))
        })
        .collect()
}

fn values<'a>(
    rows: &'a (Vec<String>, Vec<Vec<Option<String>>>),
    column: &str,
) -> Vec<Option<&'a str>> {
    let i = rows
        .0
        .iter()
        .position(|c| c == column)
        .unwrap_or_else(|| panic!("no column {column}: {:?}", rows.0));
    rows.1.iter().map(|r| r[i].as_deref()).collect()
}

fn showcase() -> Schema {
    Schema::from_json(SHOWCASE).expect("the showcase")
}

#[test]
fn the_same_seed_gives_the_same_rows() {
    let model = showcase();
    let a = rows(&plan(&model, &options(42)).unwrap());
    let b = rows(&plan(&model, &options(42)).unwrap());
    assert_eq!(a, b);
    let c = rows(&plan(&model, &options(43)).unwrap());
    assert_ne!(a["shop.customers"].1, c["shop.customers"].1);
}

#[test]
fn each_step_sequence_is_the_same_for_the_same_seed() {
    let model = showcase();
    let sql = |p: &Plan| -> Vec<String> {
        p.steps()
            .into_iter()
            .map(|s| match s {
                Step::Sql(s) | Step::Outcome(s) => s,
                Step::CopyTable(c) => c.statement(),
            })
            .collect()
    };
    assert_eq!(
        sql(&plan(&model, &options(1)).unwrap()),
        sql(&plan(&model, &options(1)).unwrap())
    );
}

#[test]
fn adding_a_column_leaves_the_others_as_they_were() {
    let users = |extra: bool| {
        let mut columns = vec![
            column("id", "integer", "int4", true),
            column("email", "text", "text", true),
            column(
                "created_at",
                "timestamp with time zone",
                "timestamptz",
                true,
            ),
        ];
        if extra {
            columns.insert(1, column("nickname", "text", "text", false));
        }
        model(
            vec![table(
                "users",
                columns,
                vec![constraint("users_pkey", "p", &["id"], "PRIMARY KEY (id)")],
            )],
            json!({}),
        )
    };
    let before = rows(&plan(&users(false), &options(5)).unwrap());
    let after = rows(&plan(&users(true), &options(5)).unwrap());
    for c in ["id", "email", "created_at"] {
        assert_eq!(
            values(&before["public.users"], c),
            values(&after["public.users"], c),
            "{c}"
        );
    }
}

/// The table each load step is about, in order.
fn load_order(p: &Plan) -> Vec<String> {
    p.steps()
        .into_iter()
        .filter_map(|s| match s {
            Step::Sql(s) if s.starts_with("-- romulus_fakedata: rows of ") => Some(
                s.lines().next().unwrap_or("")["-- romulus_fakedata: rows of ".len()..]
                    .split(' ')
                    .next()
                    .unwrap_or("")
                    .to_owned(),
            ),
            _ => None,
        })
        .collect()
}

#[test]
fn parents_load_before_their_children() {
    let p = plan(&showcase(), &options(1)).unwrap();
    let order = load_order(&p);
    let pos = |t: &str| {
        order
            .iter()
            .position(|x| x == t)
            .unwrap_or_else(|| panic!("{t} not in {order:?}"))
    };
    assert!(pos("shop.customers") < pos("shop.orders"));
    assert!(pos("shop.orders") < pos("shop.order_products"));
    assert!(pos("shop.products") < pos("shop.order_products"));
    assert!(pos("shop.warehouses") < pos("shop.stock"));
    assert!(pos("shop.customers") < pos("audit.events"));
}

fn cycle(head_nullable: bool) -> Schema {
    model(
        vec![
            table(
                "departments",
                vec![
                    column("id", "integer", "int4", true),
                    column("name", "text", "text", true),
                    column("head_id", "integer", "int4", !head_nullable),
                ],
                vec![
                    constraint("departments_pkey", "p", &["id"], "PRIMARY KEY (id)"),
                    fk("departments_head_fkey", &["head_id"], "employees", &["id"]),
                ],
            ),
            table(
                "employees",
                vec![
                    column("id", "integer", "int4", true),
                    column("department_id", "integer", "int4", true),
                    column("mentor_id", "integer", "int4", false),
                    column("name", "text", "text", true),
                ],
                vec![
                    constraint("employees_pkey", "p", &["id"], "PRIMARY KEY (id)"),
                    fk(
                        "employees_department_fkey",
                        &["department_id"],
                        "departments",
                        &["id"],
                    ),
                    fk(
                        "employees_mentor_fkey",
                        &["mentor_id"],
                        "employees",
                        &["id"],
                    ),
                ],
            ),
        ],
        json!({}),
    )
}

#[test]
fn a_cycle_breaks_at_its_nullable_foreign_key() {
    let p = plan(&cycle(true), &options(3)).unwrap();
    let steps: Vec<String> = p
        .steps()
        .into_iter()
        .filter_map(|s| match s {
            Step::Sql(s) => Some(s),
            _ => None,
        })
        .collect();
    let first_line = |s: &String| s.lines().next().unwrap_or("").to_owned();
    let labels: Vec<String> = steps.iter().map(first_line).collect();
    let pos = |needle: &str| {
        labels
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle} not in {labels:#?}"))
    };
    // departments first (head_id NULL), employees, then head_id filled.
    assert!(pos("rows of public.departments") < pos("rows of public.employees"));
    assert!(pos("rows of public.employees") < pos("departments_head_fkey of public.departments"));
    // The self-reference is filled after the rows too.
    assert!(pos("rows of public.employees") < pos("employees_mentor_fkey of public.employees"));
    let report = p.report();
    let departments = report
        .tables
        .iter()
        .find(|t| t.table == "departments")
        .unwrap();
    let head = departments
        .columns
        .iter()
        .find(|c| c.name == "head_id")
        .unwrap();
    assert!(
        matches!(&head.source, Source::ForeignKey { how, .. } if how.contains("after")),
        "{head:?}"
    );
}

#[test]
fn a_cycle_not_null_both_ways_is_loaded_in_one_block() {
    let p = plan(&cycle(false), &options(3)).unwrap();
    let held: Vec<String> = p
        .steps()
        .into_iter()
        .filter_map(|s| match s {
            Step::Sql(s) if s.starts_with("-- romulus_fakedata: rows of the cycle") => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(held.len(), 1, "one block for the cycle");
    assert!(
        held[0].contains("DROP CONSTRAINT") && held[0].contains("ADD CONSTRAINT"),
        "{}",
        held[0]
    );
}

#[test]
fn self_references_point_at_earlier_rows() {
    let p = plan(&cycle(true), &options(9)).unwrap();
    let all = rows(&p);
    let employees = &all["public.employees"];
    let ids: Vec<i64> = values(employees, "id")
        .iter()
        .map(|v| v.unwrap().parse().unwrap())
        .collect();
    for (row, mentor) in values(employees, "mentor_id").iter().enumerate() {
        if let Some(m) = mentor {
            let m: i64 = m.parse().unwrap();
            let at = ids
                .iter()
                .position(|&i| i == m)
                .expect("an existing employee");
            assert!(at < row, "row {row} points at row {at}");
        }
    }
}

#[test]
fn foreign_keys_hold_parent_keys() {
    let p = plan(&showcase(), &options(4)).unwrap();
    let all = rows(&p);
    let products: HashSet<&str> = values(&all["shop.products"], "id")
        .into_iter()
        .flatten()
        .collect();
    for v in values(&all["shop.order_products"], "product_id")
        .into_iter()
        .flatten()
    {
        assert!(products.contains(v), "{v} is no product");
    }
    // A composite key, its columns in another order than the parent's.
    let warehouses: HashSet<(Option<&str>, Option<&str>)> = {
        let w = &all["shop.warehouses"];
        values(w, "code")
            .into_iter()
            .zip(values(w, "region"))
            .collect()
    };
    let s = &all["shop.stock"];
    for pair in values(s, "warehouse_code")
        .into_iter()
        .zip(values(s, "region"))
    {
        assert!(warehouses.contains(&pair), "{pair:?} is no warehouse");
    }
}

#[test]
fn unique_sets_never_repeat() {
    let p = plan(&showcase(), &options(6)).unwrap();
    let all = rows(&p);
    let unique = |table: &str, columns: &[&str]| {
        let t = &all[table];
        let cols: Vec<Vec<Option<&str>>> = columns.iter().map(|c| values(t, c)).collect();
        let mut seen = HashSet::new();
        for i in 0..t.1.len() {
            let key: Vec<Option<&str>> = cols.iter().map(|c| c[i]).collect();
            if key.iter().all(Option::is_some) {
                assert!(seen.insert(key.clone()), "{table} repeats {key:?}");
            }
        }
        assert!(!seen.is_empty(), "{table}: no keys");
    };
    unique("shop.customers", &["email"]);
    unique("shop.products", &["sku"]);
    unique("shop.order_products", &["order_id", "product_id"]);
    unique("shop.stock", &["product_id", "region", "warehouse_code"]);
    unique("shop.warehouses", &["region", "code"]);
    unique("shop.customer_profiles", &["customer_id"]);
    // Case-insensitively unique through `lower(note)`.
    let notes: Vec<String> = values(&all["shop.orders"], "note")
        .into_iter()
        .flatten()
        .map(str::to_lowercase)
        .collect();
    assert_eq!(notes.iter().collect::<HashSet<_>>().len(), notes.len());
}

#[test]
fn checks_hold_by_construction() {
    let m = model(
        vec![table(
            "measurements",
            vec![
                column("id", "integer", "int4", true),
                column("reading", "double precision", "float8", true),
                column("code", "text", "text", true),
                column("kind", "text", "text", true),
                column("starts", "date", "date", true),
                column("ends", "date", "date", true),
                column("a", "integer", "int4", false),
                column("b", "integer", "int4", false),
            ],
            vec![
                constraint("m_pkey", "p", &["id"], "PRIMARY KEY (id)"),
                constraint(
                    "m_reading",
                    "c",
                    &[],
                    "CHECK (((reading >= ('-1000'::integer)::double precision) AND (reading <= (1000)::double precision)))",
                ),
                constraint(
                    "m_code",
                    "c",
                    &[],
                    "CHECK ((code ~ '^[A-Z]{3}-[0-9]{2}$'::text))",
                ),
                constraint(
                    "m_kind",
                    "c",
                    &[],
                    "CHECK ((kind = ANY (ARRAY['x'::text, 'y'::text])))",
                ),
                constraint("m_order", "c", &[], "CHECK ((ends > starts))"),
                constraint("m_arc", "c", &[], "CHECK ((num_nonnulls(a, b) = 1))"),
            ],
        )],
        json!({}),
    );
    let code = regex::Regex::new("^[A-Z]{3}-[0-9]{2}$").unwrap();
    for profile in [Profile::Realistic, Profile::EdgeCases] {
        let p = plan(&m, &Options::new(Size::Small, profile, 2)).unwrap();
        let all = rows(&p);
        let t = &all["public.measurements"];
        for i in 0..t.1.len() {
            let get = |c: &str| values(t, c)[i];
            let reading: f64 = get("reading").unwrap().parse().unwrap();
            assert!((-1000.0..=1000.0).contains(&reading), "{reading}");
            assert!(code.is_match(get("code").unwrap()), "{:?}", get("code"));
            assert!(matches!(get("kind"), Some("x" | "y")));
            assert!(get("ends").unwrap() > get("starts").unwrap());
            assert_eq!(
                usize::from(get("a").is_some()) + usize::from(get("b").is_some()),
                1
            );
        }
        let report = p.report();
        assert!(
            report.tables[0].fallbacks.is_empty(),
            "{:?}",
            report.tables[0].fallbacks
        );
    }
}

#[test]
fn partition_keys_land_in_a_partition() {
    let mut events = table(
        "events",
        vec![
            column("id", "bigint", "int8", true),
            column(
                "occurred_at",
                "timestamp with time zone",
                "timestamptz",
                true,
            ),
            column("region", "text", "text", true),
        ],
        vec![],
    );
    events["kind"] = json!("p");
    events["partition_key"] = json!("RANGE (occurred_at)");
    let m = model(
        vec![events],
        json!({"partitions": [
            {"parent_schema": "public", "parent": "events", "child_schema": "public", "child": "events_2026_09",
             "bound": "FOR VALUES FROM ('2026-09-01 00:00:00+00') TO ('2026-10-01 00:00:00+00')"},
            {"parent_schema": "public", "parent": "events", "child_schema": "public", "child": "events_2026_10",
             "bound": "FOR VALUES FROM ('2026-10-01 00:00:00+00') TO ('2026-11-01 00:00:00+00')"}
        ]}),
    );
    let p = plan(&m, &options(1)).unwrap();
    let all = rows(&p);
    let t = &all["public.events"];
    assert_ne!(t.1.len(), 0);
    for v in values(t, "occurred_at") {
        let v = v.unwrap();
        assert!(("2026-09-01".."2026-11-01").contains(&v), "{v}");
    }
}

#[test]
fn a_join_table_draws_every_pair_once() {
    let m = model(
        vec![
            table(
                "a",
                vec![column("id", "integer", "int4", true)],
                vec![constraint("a_pkey", "p", &["id"], "PRIMARY KEY (id)")],
            ),
            table(
                "b",
                vec![column("id", "integer", "int4", true)],
                vec![constraint("b_pkey", "p", &["id"], "PRIMARY KEY (id)")],
            ),
            table(
                "a_b",
                vec![
                    column("a_id", "integer", "int4", true),
                    column("b_id", "integer", "int4", true),
                ],
                vec![
                    constraint(
                        "a_b_pkey",
                        "p",
                        &["a_id", "b_id"],
                        "PRIMARY KEY (a_id, b_id)",
                    ),
                    fk("a_b_a_fkey", &["a_id"], "a", &["id"]),
                    fk("a_b_b_fkey", &["b_id"], "b", &["id"]),
                ],
            ),
        ],
        json!({}),
    );
    let p = plan(
        &m,
        &Options {
            max_rows_per_table: Some(30),
            ..options(8)
        },
    )
    .unwrap();
    let report = p.report();
    let rows_of = |t: &str| {
        report
            .tables
            .iter()
            .find(|x| x.table == t)
            .unwrap()
            .rows_planned
    };
    // a and b are lookups (referenced, narrow): at most 30 each; the pairs fit.
    assert!(rows_of("a_b") <= rows_of("a") * rows_of("b"));
    let all = rows(&p);
    let t = &all["public.a_b"];
    let pairs: HashSet<(Option<&str>, Option<&str>)> = values(t, "a_id")
        .into_iter()
        .zip(values(t, "b_id"))
        .collect();
    assert_eq!(pairs.len(), t.1.len());
}

#[test]
fn sizes_follow_the_preset_and_its_cap() {
    let small = plan(&showcase(), &options(1)).unwrap().report();
    assert!(small.rows_planned <= 50_000);
    let orders = small
        .tables
        .iter()
        .find(|t| t.table == "order_products")
        .unwrap();
    assert_eq!(orders.rows_planned, 2_000);
    let medium = plan(
        &showcase(),
        &Options::new(Size::Medium, Profile::Realistic, 1),
    )
    .unwrap()
    .report();
    assert!(medium.rows_planned <= 1_000_000);
    assert!(medium.rows_planned > small.rows_planned * 10);
    let capped = plan(
        &showcase(),
        &Options {
            max_rows_per_table: Some(10),
            ..options(1)
        },
    )
    .unwrap()
    .report();
    assert!(capped.tables.iter().all(|t| t.rows_planned <= 10));
}

#[test]
fn the_report_says_how_columns_are_filled() {
    let report = plan(&showcase(), &options(1)).unwrap().report();
    let customers = report
        .tables
        .iter()
        .find(|t| t.table == "customers")
        .unwrap();
    let source = |c: &str| {
        customers
            .columns
            .iter()
            .find(|x| x.name == c)
            .unwrap()
            .source
            .clone()
    };
    assert!(matches!(source("email"), Source::Heuristic { .. }));
    assert!(
        matches!(source("id"), Source::Sequence),
        "referenced: written"
    );
    let orders = report.tables.iter().find(|t| t.table == "orders").unwrap();
    let total = orders.columns.iter().find(|x| x.name == "total").unwrap();
    assert!(matches!(&total.source, Source::Database { reason } if reason.contains("generated")));
    // Serialises (the playground stores it as JSON).
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(
        json["generator_version"],
        romulus_fakedata::GENERATOR_VERSION
    );
}

#[test]
fn the_outcome_fills_the_report() {
    let p = plan(&showcase(), &options(1)).unwrap();
    let mut report = p.report();
    let outcome = json!([
        {"seq": 1, "schema": "shop", "table": "orders", "kind": "inserted", "name": null, "rows": 190, "message": null},
        {"seq": 2, "schema": "shop", "table": "orders", "kind": "check", "name": "orders_placed_when_not_draft", "rows": 6, "message": null},
        {"seq": 3, "schema": "shop", "table": "orders", "kind": "valid", "name": null, "rows": 200, "message": null},
        {"seq": 4, "schema": "shop", "table": "orders", "kind": "judged", "name": null, "rows": 6, "message": null},
        {"seq": 5, "schema": null, "table": null, "kind": "refresh_failed", "name": "\"shop\".\"daily_totals\"", "rows": 0, "message": "boom"}
    ]);
    report.record_outcome(&outcome.to_string()).unwrap();
    let orders = report.tables.iter().find(|t| t.table == "orders").unwrap();
    assert_eq!(orders.rows_inserted, Some(190));
    assert!(
        orders
            .dropped
            .iter()
            .any(|d| d.reason == DropReason::Check && d.rows == 6)
    );
    assert!(
        orders
            .dropped
            .iter()
            .any(|d| d.reason == DropReason::Duplicate && d.rows == 4)
    );
    assert!(
        report
            .lines()
            .iter()
            .any(|l| l.contains("6 of 200 rows broke `orders_placed_when_not_draft`")),
        "{:#?}",
        report.lines()
    );
    assert!(report.notes.iter().any(|n| n.contains("boom")));
    assert!(report.record_outcome("not json").is_err());
}

#[test]
fn copy_targets_are_staging_tables_with_every_written_column() {
    let p = plan(&showcase(), &options(1)).unwrap();
    for step in p.steps() {
        if let Step::CopyTable(c) = step {
            assert_eq!(c.columns[0], "romulus_ord");
            assert!(c.schema.starts_with("romulus_fakedata_"), "{}", c.schema);
            assert!(c.statement().starts_with("COPY \"romulus_fakedata_"));
            let n = c.columns.len();
            for row in c.rows.take(5) {
                assert_eq!(row.len(), n);
            }
        }
    }
}

#[test]
fn a_plan_and_its_rows_move_across_threads() {
    fn send<T: Send + 'static>() {}
    fn sync<T: Sync>() {}
    send::<Plan>();
    sync::<Plan>();
    send::<Step>();
    send::<romulus_fakedata::Rows>();
    send::<romulus_fakedata::Report>();
}

#[test]
fn every_sql_step_is_one_statement() {
    let p = plan(&showcase(), &options(1)).unwrap();
    for step in p.steps() {
        if let Step::Sql(sql) = step {
            // Outside a DO block's dollar quotes, at most a final `;`.
            let outside = match sql.find("\nDO $") {
                Some(i) => {
                    let rest = &sql[i + 4..];
                    let tag_end = rest[1..].find('$').unwrap() + 2;
                    let tag = &rest[..tag_end];
                    let close = rest[tag_end..].find(tag).unwrap() + tag_end + tag.len();
                    format!("{}{}", &sql[..i], &rest[close..])
                }
                None => sql.clone(),
            };
            let semicolons = outside
                .trim_end()
                .trim_end_matches(';')
                .matches(';')
                .count();
            assert_eq!(semicolons, 0, "{sql}");
        }
    }
}

#[test]
fn rows_per_table_can_be_set_and_the_cap_holds() {
    let model = showcase();
    let planned = |rows: &BTreeMap<String, u64>, table: &str| {
        let report = plan_with_rows(&model, &options(1), rows).unwrap().report();
        let t = report
            .tables
            .iter()
            .find(|t| t.table == table)
            .unwrap()
            .clone();
        (report.rows_planned, t)
    };
    let mut rows = BTreeMap::new();
    rows.insert("shop.orders".to_owned(), 1_234);
    assert_eq!(planned(&rows, "orders").1.rows_planned, 1_234);
    // Past the cap: the hand-set table shrinks too, and the report says so.
    rows.insert("audit.events".to_owned(), 1_000_000);
    let (total, events) = planned(&rows, "events");
    assert!(total <= 50_000, "{total}");
    assert!(events.rows_planned < 1_000_000);
    assert!(
        events
            .fallbacks
            .iter()
            .any(|f| f.kind == FallbackKind::Capped)
    );
}

#[test]
fn awkward_columns_plan_without_panicking_and_stay_valid() {
    let mut nnd = constraint(
        "t_code_key",
        "u",
        &["code"],
        "UNIQUE NULLS NOT DISTINCT (code)",
    );
    nnd["columns"] = json!(["code"]);
    let mut events = table(
        "t",
        vec![
            column("id", "integer", "int4", true),
            column("code", "text", "text", false),
            column("at", "timestamp with time zone", "timestamptz", true),
            column("wide", "numeric(400,300)", "numeric", true),
            column("x", "integer", "int4", false),
            column("y", "integer", "int4", false),
            column("clock", "time without time zone", "time", true),
        ],
        vec![
            constraint("t_pkey", "p", &["id"], "PRIMARY KEY (id)"),
            nnd,
            constraint("t_clock_key", "u", &["clock"], "UNIQUE (clock)"),
            constraint("t_two", "c", &[], "CHECK ((num_nonnulls(x, y) >= 2))"),
            constraint("t_one", "c", &[], "CHECK ((num_nonnulls(x, y) <= 1))"),
        ],
    );
    events["indexes"] = json!([{
        "name": "t_day_idx", "unique": true, "partial": false,
        "definition": "CREATE UNIQUE INDEX t_day_idx ON public.t USING btree (date_trunc('day'::text, at))"
    }]);
    let m = model(vec![events], json!({}));
    for profile in [Profile::Realistic, Profile::EdgeCases] {
        let p = plan(
            &m,
            &Options {
                max_rows_per_table: Some(100_000),
                ..Options::new(Size::Medium, profile, 3)
            },
        )
        .unwrap();
        let report = p.report();
        let t = &report.tables[0];
        // A unique time is a second of one day.
        assert!(t.rows_planned <= 86_400, "{}", t.rows_planned);
        // An index on an expression is left to Postgres, and said so.
        assert!(
            t.fallbacks
                .iter()
                .any(|f| f.constraint.as_deref() == Some("t_day_idx")
                    && f.kind == FallbackKind::Judged),
            "{:?}",
            t.fallbacks
        );
        let all = rows(&p);
        let sample = &all["public.t"];
        // NULLS NOT DISTINCT: the column keeping the set distinct is never NULL.
        assert!(values(sample, "code").iter().all(Option::is_some));
        for w in values(sample, "wide") {
            let w = w.unwrap();
            assert!(w.parse::<f64>().is_ok() || w == "NaN", "{w}");
        }
    }
}

#[test]
fn nulls_not_distinct_keeps_every_column_of_its_set_filled() {
    let m = model(
        vec![
            table(
                "users",
                vec![column("id", "integer", "int4", true)],
                vec![constraint("users_pkey", "p", &["id"], "PRIMARY KEY (id)")],
            ),
            table(
                "t",
                vec![
                    column("id", "integer", "int4", true),
                    column("code", "text", "text", false),
                    column("kind", "text", "text", false),
                    column("owner_id", "integer", "int4", false),
                ],
                vec![
                    constraint("t_pkey", "p", &["id"], "PRIMARY KEY (id)"),
                    // `code` is made distinct here first, NULLs allowed…
                    constraint("t_code_key", "u", &["code"], "UNIQUE (code)"),
                    // …then this set finds it already distinct: still no NULL.
                    constraint(
                        "t_code_kind_key",
                        "u",
                        &["code", "kind"],
                        "UNIQUE NULLS NOT DISTINCT (code, kind)",
                    ),
                    // A one-to-one foreign key under NULLS NOT DISTINCT.
                    constraint(
                        "t_owner_key",
                        "u",
                        &["owner_id"],
                        "UNIQUE NULLS NOT DISTINCT (owner_id)",
                    ),
                    fk("t_owner_fkey", &["owner_id"], "users", &["id"]),
                ],
            ),
        ],
        json!({}),
    );
    for profile in [Profile::Realistic, Profile::EdgeCases] {
        let p = plan(&m, &Options::new(Size::Small, profile, 4)).unwrap();
        let all = rows(&p);
        let t = &all["public.t"];
        assert_ne!(t.1.len(), 0);
        for c in ["code", "kind", "owner_id"] {
            assert!(
                values(t, c).iter().all(Option::is_some),
                "{profile:?}: NULL in {c}"
            );
        }
        let owners: HashSet<Option<&str>> = values(t, "owner_id").into_iter().collect();
        assert_eq!(owners.len(), t.1.len(), "one row per owner");
    }
}
