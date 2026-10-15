//! Statement keys name unnamed indexes and constraints exactly as Postgres
//! does: checked against what the sandbox actually created.

use std::collections::HashSet;

use romulus_sandbox::{Action, ObjectKind, parse};

use crate::{compile_ok, with_compiler};

/// Every index, unique, primary-key, exclusion, check and foreign-key name
/// the keys predict exists in the compiled model.
#[test]
fn predicted_names_are_the_names_postgres_chose() {
    with_compiler(|compiler| async move {
        let long_table = "a".repeat(60);
        let long_column = "b".repeat(30);
        let texts = [
            "CREATE TABLE u (id int PRIMARY KEY); CREATE TABLE v (id int PRIMARY KEY);
             CREATE TABLE t (x int CHECK (x > 0), y int, z int, UNIQUE (x), UNIQUE (x));
             ALTER TABLE t ADD CHECK (x < 9);
             ALTER TABLE t ADD CHECK (x <> 5);
             ALTER TABLE t ADD CHECK (x > y);
             ALTER TABLE t ADD CHECK (y > z);
             CREATE INDEX ON t (x);
             CREATE INDEX ON t USING hash (x);
             CREATE UNIQUE INDEX ON t (lower(x::text), lower(y::text));
             CREATE INDEX ON t ((x + y));
             CREATE INDEX ON t (x) INCLUDE (y, z);
             CREATE INDEX t_y_key ON t (y);
             ALTER TABLE t ADD UNIQUE (y);
             ALTER TABLE t ADD FOREIGN KEY (x) REFERENCES u;
             ALTER TABLE t ADD FOREIGN KEY (x) REFERENCES v;
             ALTER TABLE t ADD CONSTRAINT keep CHECK (z > 0), ADD CHECK (z < 100), ADD UNIQUE (z);
             ALTER TABLE t DROP CONSTRAINT t_x_check1;
             ALTER TABLE t ADD CHECK (x < 100);
             CREATE TABLE w (id serial PRIMARY KEY, k int GENERATED ALWAYS AS IDENTITY, EXCLUDE USING btree (id WITH =));",
            "CREATE TABLE p (x int, y text) PARTITION BY RANGE (x);
             CREATE TABLE p1 PARTITION OF p FOR VALUES FROM (1) TO (10);
             CREATE INDEX ON p (x);
             CREATE INDEX ON p1 (x);
             CREATE INDEX ON p (lower(y));
             CREATE TABLE p2 PARTITION OF p FOR VALUES FROM (10) TO (20);
             CREATE INDEX ON p2 (x);
             ALTER TABLE p DETACH PARTITION p1;
             ALTER TABLE p DETACH PARTITION p2;",
            &format!(
                "CREATE TABLE {long_table} ({long_column} int, c int);
                 CREATE INDEX ON {long_table} ({long_column});
                 CREATE INDEX ON {long_table} ({long_column});
                 ALTER TABLE {long_table} ADD UNIQUE ({long_column}, c);
                 ALTER TABLE {long_table} ADD CHECK ({long_column} > 0);"
            ),
        ];
        for sql in texts {
            let parsed = parse(sql);
            let compilation = compile_ok(compiler, sql).await;
            let model = compilation.model.expect("a model");
            let names: HashSet<(String, String)> = model
                .entities
                .iter()
                .flat_map(|e| {
                    e.indexes
                        .iter()
                        .map(|i| i.name.clone())
                        .chain(e.constraints.iter().map(|c| c.name.clone()))
                        .map(|n| (e.schema.clone(), n))
                })
                .collect();
            let mut checked = 0;
            for statement in &parsed.statements {
                for key in &statement.keys {
                    let has_name = matches!(
                        key.kind(),
                        ObjectKind::Index | ObjectKind::Constraint | ObjectKind::ForeignKey
                    );
                    if key.action() != Action::Create || !has_name {
                        continue;
                    }
                    let (schema, name) = (&key.path()[0], key.path().last().unwrap());
                    assert!(
                        names.contains(&(schema.clone(), name.clone())),
                        "predicted {key}, Postgres has {names:?}"
                    );
                    checked += 1;
                }
            }
            assert!(checked > 0);
        }
    });
}
