//! Medium on a 30-table schema, loaded: how the time splits between
//! generating rows and Postgres judging them. Ignored by default (a
//! million rows); run it in release:
//!
//! ```text
//! cargo test --release -p romulus-fakedata --test oracle medium -- --ignored --nocapture
//! ```

use std::{fmt::Write as _, time::Instant};

use romulus_fakedata::{Options, Profile, Size, Step, copy_text, plan};
use sqlx::{AssertSqlSafe, Row, raw_sql};

use crate::{drop_scratch, scratch, verify, with_sandbox};

/// Lookups, entities referencing them, events referencing entities.
fn schema() -> String {
    let mut sql = String::new();
    for l in ["countries", "currencies", "statuses", "plans", "categories"] {
        let _ = writeln!(
            sql,
            "CREATE TABLE {l} (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, code char(3) NOT NULL UNIQUE, label text NOT NULL);"
        );
    }
    let entities: [(&str, &[&str]); 10] = [
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
    for (name, parents) in entities {
        let _ = write!(
            sql,
            "CREATE TABLE {name} (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, name text NOT NULL, \
             email varchar(255) UNIQUE, description text, active boolean NOT NULL DEFAULT true, settings jsonb, \
             created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz"
        );
        for p in parents {
            let _ = write!(
                sql,
                ", {}_id bigint NOT NULL REFERENCES {p} (id)",
                p.trim_end_matches('s')
            );
        }
        sql.push_str(");\n");
    }
    let events: [(&str, &[&str]); 15] = [
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
    for (name, parents) in events {
        let _ = write!(
            sql,
            "CREATE TABLE {name} (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
             amount numeric(12, 2) NOT NULL CHECK (amount >= 0), quantity integer NOT NULL, \
             status text NOT NULL CHECK (status IN ('new', 'paid', 'shipped', 'refunded')), note text, ip inet, \
             occurred_at timestamptz NOT NULL"
        );
        for p in parents {
            let _ = write!(
                sql,
                ", {}_id bigint REFERENCES {p} (id)",
                p.trim_end_matches('s')
            );
        }
        sql.push_str(");\n");
    }
    sql
}

#[test]
#[ignore = "a million rows: run it in release, by hand"]
fn a_medium_load_of_thirty_tables() {
    medium(Profile::Realistic);
}

#[test]
#[ignore = "a million rows: run it in release, by hand"]
fn a_medium_load_of_thirty_tables_edge_cases() {
    medium(Profile::EdgeCases);
}

fn medium(profile: Profile) {
    with_sandbox(|compiler, admin| async move {
        let sql = schema();
        let model = compiler
            .compile(&sql)
            .await
            .expect("the sandbox")
            .model
            .expect("the schema compiles");
        let (name, mut db) = scratch(&admin).await.expect("a scratch database");
        raw_sql(AssertSqlSafe(sql))
            .execute(&mut db)
            .await
            .expect("the schema builds");
        let plan = plan(&model, &Options::new(Size::Medium, profile, 7)).expect("a plan");
        let mut report = plan.report();
        let started = Instant::now();
        let (mut generating, mut copying, mut judging) = (0.0, 0.0, 0.0);
        for step in plan.steps() {
            match step {
                Step::CopyTable(copy) => {
                    let mut sink = db.copy_in_raw(&copy.statement()).await.expect("COPY");
                    let mut buffer = Vec::with_capacity(1 << 20);
                    let mut rows = copy.rows;
                    loop {
                        let t = Instant::now();
                        let Some(row) = rows.next() else { break };
                        copy_text(&row, &mut buffer);
                        generating += t.elapsed().as_secs_f64();
                        if buffer.len() >= 1 << 20 {
                            let t = Instant::now();
                            sink.send(std::mem::take(&mut buffer)).await.expect("send");
                            copying += t.elapsed().as_secs_f64();
                        }
                    }
                    let t = Instant::now();
                    sink.send(buffer).await.expect("send");
                    sink.finish().await.expect("COPY done");
                    copying += t.elapsed().as_secs_f64();
                }
                Step::Sql(s) => {
                    let t = Instant::now();
                    raw_sql(AssertSqlSafe(s))
                        .execute(&mut db)
                        .await
                        .expect("a step");
                    judging += t.elapsed().as_secs_f64();
                }
                Step::Outcome(q) => {
                    let json: String = raw_sql(AssertSqlSafe(q))
                        .fetch_one(&mut db)
                        .await
                        .expect("the outcome")
                        .get(0);
                    report.record_outcome(&json).expect("an outcome");
                }
            }
        }
        #[allow(clippy::print_stderr)]
        {
            eprintln!(
                "{} rows planned, {} inserted in {:?}: generating {generating:.1} s (this thread), COPY {copying:.1} s, SQL {judging:.1} s",
                report.rows_planned,
                report.rows_inserted.unwrap_or(0),
                started.elapsed()
            );
            for line in report.lines() {
                eprintln!("    {line}");
            }
        }
        let problems = verify(&mut db, &model, &report).await;
        drop_scratch(&admin, &name, db).await;
        assert!(problems.is_empty(), "{problems:#?}");
        assert!(
            report.tables.iter().all(|t| !t.row_by_row),
            "a table fell back to one row at a time"
        );
        match profile {
            Profile::Realistic => assert_eq!(report.rows_dropped(), 0),
            _ => assert!(
                report.rows_dropped() * 100 <= report.rows_planned,
                "{} dropped",
                report.rows_dropped()
            ),
        }
    });
}
