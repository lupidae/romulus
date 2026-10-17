//! Plans Medium rows for a 30-table schema and streams them all as `COPY`
//! text, without a database: the generator's own time and memory.
//!
//! ```text
//! /usr/bin/time -l cargo run --release -p romulus-fakedata --example medium   # macOS
//! /usr/bin/time -v cargo run --release -p romulus-fakedata --example medium   # Linux
//! ```
//!
//! The rows are counted and dropped as they come, as a runner sending them
//! to Postgres in chunks would. The resident set is sampled as it goes and
//! must stay under 50 MB.

#![allow(clippy::print_stdout, clippy::cast_precision_loss)]

use std::time::Instant;

use remus_core::Schema;
use romulus_fakedata::{Options, Profile, Size, Step, copy_text, plan};
use serde_json::{Value, json};

fn column(name: &str, ty: &str, catalog: &str, not_null: bool) -> Value {
    json!({
        "name": name, "type": ty, "type_schema": "pg_catalog", "type_name": catalog,
        "type_kind": "b", "is_array": ty.ends_with("[]"), "not_null": not_null,
        "default": null, "identity": null, "generated": null, "comment": null
    })
}

fn id(name: &str) -> Value {
    let mut c = column(name, "bigint", "int8", true);
    c["identity"] = json!("a");
    c
}

fn constraint(name: &str, kind: &str, columns: &[&str], definition: &str) -> Value {
    json!({
        "name": name, "kind": kind, "columns": columns, "ref_schema": null, "ref_table": null,
        "ref_columns": [], "on_update": null, "on_delete": null, "deferrable": false,
        "definition": definition, "comment": null
    })
}

fn fk(table: &str, column: &str, parent: &str) -> Value {
    json!({
        "name": format!("{table}_{column}_fkey"), "kind": "f", "columns": [column],
        "ref_schema": "public", "ref_table": parent, "ref_columns": ["id"],
        "on_update": null, "on_delete": null, "deferrable": false,
        "definition": format!("FOREIGN KEY ({column}) REFERENCES public.{parent}(id)"), "comment": null
    })
}

#[allow(clippy::needless_pass_by_value)]
fn table(name: &str, columns: Vec<Value>, constraints: Vec<Value>) -> Value {
    json!({
        "schema": "public", "name": name, "kind": "r", "comment": null, "partition_key": null,
        "view_definition": null, "columns": columns, "constraints": constraints
    })
}

/// An entity: an identity key, a name, an email or code, timestamps.
fn entity(name: &str, parents: &[&str]) -> Value {
    let mut columns = vec![
        id("id"),
        column("name", "text", "text", true),
        column("email", "character varying(255)", "varchar", false),
        column("description", "text", "text", false),
        column("active", "boolean", "bool", true),
        column("settings", "jsonb", "jsonb", false),
        column(
            "created_at",
            "timestamp with time zone",
            "timestamptz",
            true,
        ),
        column(
            "updated_at",
            "timestamp with time zone",
            "timestamptz",
            false,
        ),
    ];
    let mut constraints = vec![
        constraint(&format!("{name}_pkey"), "p", &["id"], "PRIMARY KEY (id)"),
        constraint(
            &format!("{name}_email_key"),
            "u",
            &["email"],
            "UNIQUE (email)",
        ),
    ];
    for p in parents {
        let c = format!("{}_id", p.trim_end_matches('s'));
        columns.push(column(&c, "bigint", "int8", true));
        constraints.push(fk(name, &c, p));
    }
    table(name, columns, constraints)
}

/// An event: a key, foreign keys, an amount with a CHECK, a status, a time.
fn event(name: &str, parents: &[&str]) -> Value {
    let mut columns = vec![
        id("id"),
        column("amount", "numeric(12,2)", "numeric", true),
        column("quantity", "integer", "int4", true),
        column("status", "text", "text", true),
        column("note", "text", "text", false),
        column("ip", "inet", "inet", false),
        column(
            "occurred_at",
            "timestamp with time zone",
            "timestamptz",
            true,
        ),
    ];
    let mut constraints = vec![
        constraint(&format!("{name}_pkey"), "p", &["id"], "PRIMARY KEY (id)"),
        constraint(
            &format!("{name}_amount_check"),
            "c",
            &[],
            "CHECK ((amount >= (0)::numeric))",
        ),
        constraint(
            &format!("{name}_status_check"),
            "c",
            &[],
            "CHECK ((status = ANY (ARRAY['new'::text, 'paid'::text, 'shipped'::text, 'refunded'::text])))",
        ),
    ];
    for p in parents {
        let c = format!("{}_id", p.trim_end_matches('s'));
        columns.push(column(&c, "bigint", "int8", false));
        constraints.push(fk(name, &c, p));
    }
    table(name, columns, constraints)
}

fn lookup(name: &str) -> Value {
    table(
        name,
        vec![
            id("id"),
            column("code", "character(3)", "bpchar", true),
            column("label", "text", "text", true),
        ],
        vec![
            constraint(&format!("{name}_pkey"), "p", &["id"], "PRIMARY KEY (id)"),
            constraint(&format!("{name}_code_key"), "u", &["code"], "UNIQUE (code)"),
        ],
    )
}

fn schema() -> Schema {
    let mut entities = Vec::new();
    for l in ["countries", "currencies", "statuses", "plans", "categories"] {
        entities.push(lookup(l));
    }
    let entity_tables: [(&str, &[&str]); 10] = [
        ("organizations", &["plans", "countries"]),
        ("users", &["organizations"]),
        ("customers", &["countries"]),
        ("suppliers", &["countries"]),
        ("products", &["categories", "suppliers"]),
        ("warehouses", &["countries"]),
        ("stores", &["organizations"]),
        ("projects", &["organizations"]),
        ("teams", &["organizations"]),
        ("documents", &["projects"]),
    ];
    for (name, parents) in entity_tables {
        entities.push(entity(name, parents));
    }
    let event_tables: [(&str, &[&str]); 15] = [
        ("orders", &["customers", "stores", "currencies"]),
        ("order_lines", &["products", "warehouses"]),
        ("payments", &["customers", "currencies"]),
        ("shipments", &["warehouses", "customers"]),
        ("refunds", &["customers"]),
        ("page_views", &["users"]),
        ("sessions", &["users"]),
        ("reviews", &["products", "customers"]),
        ("comments", &["documents", "users"]),
        ("invoices", &["organizations", "currencies"]),
        ("stock_moves", &["products", "warehouses"]),
        ("notifications", &["users"]),
        ("audit_log", &["users", "organizations"]),
        ("time_entries", &["projects", "users"]),
        ("api_calls", &["organizations"]),
    ];
    for (name, parents) in event_tables {
        entities.push(event(name, parents));
    }
    let model = json!({
        "remus_version": "0.2", "database": "medium", "server_version": "17",
        "entities": entities
    });
    Schema::from_json(&model.to_string()).expect("the synthetic model")
}

fn main() {
    let model = schema();
    let started = Instant::now();
    let plan = plan(&model, &Options::new(Size::Medium, Profile::Realistic, 7)).expect("a plan");
    let planned = started.elapsed();
    let mut rows = 0u64;
    let mut peak = rss_kb();
    let mut bytes = 0usize;
    let mut buffer = Vec::with_capacity(1 << 16);
    for step in plan.steps() {
        if let Step::CopyTable(copy) = step {
            for row in copy.rows {
                copy_text(&row, &mut buffer);
                rows += 1;
                if rows.is_multiple_of(50_000) {
                    peak = peak.max(rss_kb());
                }
                if buffer.len() >= 1 << 16 {
                    bytes += buffer.len();
                    buffer.clear();
                }
            }
        }
    }
    bytes += buffer.len();
    let elapsed = started.elapsed();
    println!(
        "{} tables, {rows} rows, {:.1} MB of COPY text; planned in {planned:?}, streamed in {elapsed:?} ({:.0} rows/s)",
        model.entities.len(),
        bytes as f64 / 1e6,
        rows as f64 / elapsed.as_secs_f64()
    );
    let peak = peak.max(rss_kb());
    println!(
        "peak resident set (sampled every 50 000 rows): {:.1} MB",
        peak as f64 / 1024.0
    );
    // The generator streams: memory must not grow with the rows.
    assert!(
        peak < 50 * 1024,
        "the generator held {peak} kB: it should stream"
    );
}

/// This process's resident set, in kB (`ps`, on macOS and Linux).
fn rss_kb() -> u64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}
