//! Schemas built to be awkward: the load never fails a step, and what
//! cannot be loaded is in the report.

use romulus_fakedata::{DropReason, FallbackKind, Options, Profile, Report, Size};

use crate::{run, with_sandbox};

struct Scenario {
    name: &'static str,
    sql: &'static str,
    /// What the report must show.
    expect: fn(&Report) -> Result<(), String>,
}

fn table<'r>(report: &'r Report, name: &str) -> Result<&'r romulus_fakedata::TableReport, String> {
    report
        .tables
        .iter()
        .find(|t| t.table == name)
        .ok_or_else(|| format!("no table {name}"))
}

/// Every planned row of every table inserted.
fn all_in(report: &Report) -> Result<(), String> {
    for t in &report.tables {
        if t.rows_inserted != Some(t.rows_planned) {
            return Err(format!(
                "{}: {:?} of {} inserted: {:?}",
                t.table, t.rows_inserted, t.rows_planned, t.dropped
            ));
        }
    }
    Ok(())
}

fn some_rows(report: &Report, name: &str) -> Result<(), String> {
    let t = table(report, name)?;
    if t.rows_inserted.unwrap_or(0) == 0 {
        return Err(format!("{name}: no rows ({:?})", t.dropped));
    }
    Ok(())
}

/// Rows Postgres dropped from `b` take the `a` rows pointing at them, and
/// so on around the cycle; what is left still loads, and says why.
fn cycle_with_drops(r: &Report) -> Result<(), String> {
    some_rows(r, "a")?;
    some_rows(r, "b")?;
    let b = table(r, "b")?;
    if !b.dropped.iter().any(|d| d.reason == DropReason::Check) {
        return Err(format!("expected b's CHECK drops: {b:?}"));
    }
    let a = table(r, "a")?;
    if !a.dropped.iter().any(|d| d.reason == DropReason::ForeignKey) {
        return Err(format!("expected a's rows left without a parent: {a:?}"));
    }
    Ok(())
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "a cycle NOT NULL both ways",
        sql: "CREATE TABLE a (id int PRIMARY KEY, b_id int NOT NULL, label text NOT NULL);
              CREATE TABLE b (id int PRIMARY KEY, a_id int NOT NULL REFERENCES a (id));
              ALTER TABLE a ADD CONSTRAINT a_b_fkey FOREIGN KEY (b_id) REFERENCES b (id);
              COMMENT ON CONSTRAINT a_b_fkey ON a IS 'kept across the load';",
        expect: |r| {
            some_rows(r, "a")?;
            some_rows(r, "b")?;
            all_in(r)
        },
    },
    Scenario {
        name: "a cycle NOT NULL both ways, deferrable",
        sql: "CREATE TABLE a (id int PRIMARY KEY, b_id int NOT NULL);
              CREATE TABLE b (id int PRIMARY KEY, a_id int NOT NULL REFERENCES a (id));
              ALTER TABLE a ADD CONSTRAINT a_b_fkey FOREIGN KEY (b_id) REFERENCES b (id) DEFERRABLE INITIALLY DEFERRED;",
        expect: |r| {
            some_rows(r, "a")?;
            all_in(r)
        },
    },
    Scenario {
        name: "a cycle NOT NULL both ways, rows dropped on one side",
        sql: "CREATE TABLE a (id int PRIMARY KEY, b_id int NOT NULL, label text NOT NULL);
              CREATE TABLE b (id int PRIMARY KEY, a_id int NOT NULL REFERENCES a (id),
                  CONSTRAINT b_not_sevens CHECK (id % 7 <> 0));
              ALTER TABLE a ADD CONSTRAINT a_b_fkey FOREIGN KEY (b_id) REFERENCES b (id);",
        expect: cycle_with_drops,
    },
    Scenario {
        name: "a cycle NOT NULL both ways, deferrable, rows dropped on one side",
        sql: "CREATE TABLE a (id int PRIMARY KEY, b_id int NOT NULL);
              CREATE TABLE b (id int PRIMARY KEY, a_id int NOT NULL REFERENCES a (id) DEFERRABLE,
                  CONSTRAINT b_not_sevens CHECK (id % 7 <> 0));
              ALTER TABLE a ADD CONSTRAINT a_b_fkey FOREIGN KEY (b_id) REFERENCES b (id) DEFERRABLE INITIALLY DEFERRED;",
        expect: cycle_with_drops,
    },
    Scenario {
        name: "a deferrable unique (no ON CONFLICT arbiter)",
        sql: "CREATE TABLE t (id int PRIMARY KEY, code text NOT NULL UNIQUE DEFERRABLE INITIALLY DEFERRED, n int);",
        expect: all_in,
    },
    Scenario {
        name: "row level security forced on the owner",
        sql: "CREATE TABLE secrets (id int PRIMARY KEY, owner_id int NOT NULL, body text);
              ALTER TABLE secrets ENABLE ROW LEVEL SECURITY;
              ALTER TABLE secrets FORCE ROW LEVEL SECURITY;
              CREATE POLICY mine ON secrets USING (owner_id = 42);",
        expect: |r| {
            some_rows(r, "secrets")?;
            all_in(r)
        },
    },
    Scenario {
        name: "a CHECK that raises on every row",
        sql: "CREATE TABLE t (id int PRIMARY KEY, payload jsonb NOT NULL,
                  CONSTRAINT t_n CHECK ((1 / (length(payload::text) - length(payload::text))) = 0));",
        expect: |r| {
            let t = table(r, "t")?;
            if t.rows_inserted != Some(0) || !t.dropped.iter().any(|d| d.reason == DropReason::Error) {
                return Err(format!("expected every row refused, with the error: {t:?}"));
            }
            Ok(())
        },
    },
    Scenario {
        name: "a table with nothing to write",
        sql: "CREATE TABLE ticks (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, at timestamptz DEFAULT now());
              CREATE TABLE nothing ();",
        expect: |r| {
            some_rows(r, "nothing")?;
            all_in(r)
        },
    },
    Scenario {
        name: "an exclusive arc and a CHECK over two columns",
        sql: "CREATE TABLE posts (id int PRIMARY KEY, title text NOT NULL);
              CREATE TABLE photos (id int PRIMARY KEY, url text NOT NULL);
              CREATE TABLE comments (
                  id int PRIMARY KEY,
                  post_id int REFERENCES posts (id),
                  photo_id int REFERENCES photos (id),
                  starts_at timestamptz NOT NULL, ends_at timestamptz NOT NULL,
                  CHECK (num_nonnulls(post_id, photo_id) = 1),
                  CHECK (ends_at > starts_at));",
        expect: all_in,
    },
    Scenario {
        name: "domains over domains, arrays of composites and enums",
        sql: "CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');
              CREATE DOMAIN positive AS integer CHECK (VALUE > 0);
              CREATE DOMAIN small_positive AS positive CHECK (VALUE < 100) NOT NULL;
              CREATE DOMAIN code AS varchar(8) CHECK (VALUE ~ '^[A-Z]{2}[0-9]{2,6}$');
              CREATE TYPE pair AS (label text, score small_positive, feeling mood);
              CREATE TABLE t (
                  id int PRIMARY KEY,
                  level small_positive,
                  ref code UNIQUE,
                  pairs pair[],
                  moods mood[] NOT NULL DEFAULT '{}',
                  codes code[]);",
        expect: all_in,
    },
    Scenario {
        name: "partitions awkward to fill",
        sql: "CREATE TABLE half (id uuid NOT NULL, v int) PARTITION BY HASH (id);
              CREATE TABLE half_0 PARTITION OF half FOR VALUES WITH (MODULUS 4, REMAINDER 0);
              CREATE TABLE half_1 PARTITION OF half FOR VALUES WITH (MODULUS 4, REMAINDER 1);
              CREATE TABLE by_kind (kind text, n int NOT NULL) PARTITION BY LIST (kind);
              CREATE TABLE by_kind_a PARTITION OF by_kind FOR VALUES IN ('a', NULL);
              CREATE TABLE by_kind_b PARTITION OF by_kind FOR VALUES IN ('b');
              CREATE TABLE grid (x int NOT NULL, y int NOT NULL) PARTITION BY RANGE (x, y);
              CREATE TABLE grid_low PARTITION OF grid FOR VALUES FROM (MINVALUE, MINVALUE) TO (100, 0);
              CREATE TABLE grid_high PARTITION OF grid FOR VALUES FROM (100, 0) TO (MAXVALUE, MAXVALUE);
              CREATE TABLE words (w text NOT NULL) PARTITION BY RANGE (w);
              CREATE TABLE words_am PARTITION OF words FOR VALUES FROM ('a') TO ('m');",
        expect: |r| {
            let half = table(r, "half")?;
            if half.rows_planned != 0 || !half.fallbacks.iter().any(|f| f.kind == FallbackKind::NoRows) {
                return Err(format!("half covers 2 hash classes of 4: no rows, said so: {half:?}"));
            }
            some_rows(r, "by_kind")?;
            some_rows(r, "grid")?;
            some_rows(r, "words")?;
            all_in(r)
        },
    },
    Scenario {
        name: "a self-reference without a key to fill it by",
        sql: "CREATE TABLE nodes (name text UNIQUE, parent text REFERENCES nodes (name));",
        expect: |r| {
            let t = table(r, "nodes")?;
            if !t.fallbacks.iter().any(|f| f.kind == FallbackKind::NotBuilt) {
                return Err(format!("expected the fallback in the report: {t:?}"));
            }
            all_in(r)
        },
    },
    Scenario {
        name: "keys that hold few values",
        sql: "CREATE TABLE flags (on_off boolean UNIQUE NOT NULL);
              CREATE TABLE codes (c char(1) PRIMARY KEY);
              CREATE TABLE tiny (n smallint PRIMARY KEY CHECK (n BETWEEN 1 AND 5));",
        expect: |r| {
            let flags = table(r, "flags")?;
            if flags.rows_planned > 2 {
                return Err(format!("a unique boolean holds 2 rows: {flags:?}"));
            }
            let tiny = table(r, "tiny")?;
            if tiny.rows_planned > 5 {
                return Err(format!("1 to 5: {tiny:?}"));
            }
            all_in(r)
        },
    },
    Scenario {
        name: "a generated column a CHECK reads, and a stub default",
        sql: "CREATE TABLE items (
                  id bigint PRIMARY KEY DEFAULT util.next_id(),
                  price numeric(8, 2) NOT NULL CHECK (price > 0),
                  qty int NOT NULL CHECK (qty BETWEEN 1 AND 50),
                  total numeric(10, 2) GENERATED ALWAYS AS (price * qty) STORED,
                  CONSTRAINT items_total CHECK (total < 400000));",
        expect: all_in,
    },
];

#[test]
fn hostile_schemas_load_without_a_failed_step() {
    with_sandbox(|compiler, admin| async move {
        let mut failures = Vec::new();
        for profile in [Profile::Realistic, Profile::EdgeCases] {
            for s in SCENARIOS {
                let options = Options::new(Size::Small, profile, 3);
                let label = format!("{} ({profile:?})", s.name);
                match run(&compiler, &admin, &label, s.sql, &options).await {
                    Ok((report, problems)) => {
                        failures.extend(problems.into_iter().map(|p| format!("{label}: {p}")));
                        if let Err(e) = (s.expect)(&report) {
                            failures.push(format!("{label}: {e}"));
                        }
                    }
                    Err(e) => failures.push(format!("{label}: {e}")),
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} problems:\n\n{}",
            failures.len(),
            failures.join("\n\n")
        );
    });
}
