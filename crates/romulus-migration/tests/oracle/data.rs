//! Migrations on tables with rows: what a hazard says is what happens. A
//! `deletes_data` statement runs and the values come out changed; a
//! `can_fail` statement stops on the rows that break it, and nothing is
//! lost. Each case builds the old schema on a scratch database, inserts a
//! few rows, and applies the plain migration statement by statement.

use romulus_migration::{Hazard, Options, migration};
use sqlx::Row;

use crate::{
    admin_url, compile,
    tools::{drop_scratch, scratch, script, statements},
    with_compiler,
};

enum Expect {
    /// The migration runs; afterwards `query` returns `value`.
    Runs {
        query: &'static str,
        value: &'static str,
    },
    /// The migration stops with an error containing this.
    Fails(&'static str),
}

struct DataCase {
    name: &'static str,
    old: &'static str,
    rows: &'static str,
    new: &'static str,
    /// Part of the statement whose hazard is checked.
    statement: &'static str,
    hazard: Hazard,
    expect: Expect,
}

const CASES: &[DataCase] = &[
    DataCase {
        name: "text shortened to varchar(10)",
        old: "CREATE TABLE t (id integer PRIMARY KEY, code text);",
        rows: "INSERT INTO t VALUES (1, 'longer than ten');",
        new: "CREATE TABLE t (id integer PRIMARY KEY, code varchar(10));",
        statement: "ALTER COLUMN code TYPE",
        hazard: Hazard::CanFail,
        expect: Expect::Fails("value too long"),
    },
    DataCase {
        name: "varchar(20) shortened to varchar(10)",
        old: "CREATE TABLE t (id integer PRIMARY KEY, code varchar(20));",
        rows: "INSERT INTO t VALUES (1, 'longer than ten');",
        new: "CREATE TABLE t (id integer PRIMARY KEY, code varchar(10));",
        statement: "ALTER COLUMN code TYPE",
        hazard: Hazard::CanFail,
        expect: Expect::Fails("value too long"),
    },
    DataCase {
        name: "bit(8) shortened to bit(4)",
        old: "CREATE TABLE t (id integer PRIMARY KEY, flags bit(8));",
        rows: "INSERT INTO t VALUES (1, B'10101010');",
        new: "CREATE TABLE t (id integer PRIMARY KEY, flags bit(4));",
        statement: "ALTER COLUMN flags TYPE",
        hazard: Hazard::CanFail,
        expect: Expect::Fails("bit string length"),
    },
    DataCase {
        name: "text to name cuts at 63 bytes",
        old: "CREATE TABLE t (id integer PRIMARY KEY, label text);",
        rows: "INSERT INTO t VALUES (1, repeat('x', 70));",
        new: "CREATE TABLE t (id integer PRIMARY KEY, label name);",
        statement: "ALTER COLUMN label TYPE",
        hazard: Hazard::DeletesData,
        expect: Expect::Runs {
            query: "SELECT octet_length(label)::text FROM t",
            value: "63",
        },
    },
    DataCase {
        name: "integer to bit(4) keeps the low bits",
        old: "CREATE TABLE t (id integer PRIMARY KEY, n integer);",
        rows: "INSERT INTO t VALUES (1, 255);",
        new: "CREATE TABLE t (id integer PRIMARY KEY, n bit(4));",
        statement: "ALTER COLUMN n TYPE",
        hazard: Hazard::DeletesData,
        expect: Expect::Runs {
            query: "SELECT n::text FROM t",
            value: "1111",
        },
    },
    DataCase {
        name: "numeric to integer rounds",
        old: "CREATE TABLE t (id integer PRIMARY KEY, price numeric(10,2));",
        rows: "INSERT INTO t VALUES (1, 1.60);",
        new: "CREATE TABLE t (id integer PRIMARY KEY, price integer);",
        statement: "ALTER COLUMN price TYPE",
        hazard: Hazard::DeletesData,
        expect: Expect::Runs {
            query: "SELECT price::text FROM t",
            value: "2",
        },
    },
    DataCase {
        name: "SET NOT NULL on a NULL",
        old: "CREATE TABLE t (id integer PRIMARY KEY, note text);",
        rows: "INSERT INTO t VALUES (1, NULL);",
        new: "CREATE TABLE t (id integer PRIMARY KEY, note text NOT NULL);",
        statement: "SET NOT NULL",
        hazard: Hazard::CanFail,
        expect: Expect::Fails("contains null values"),
    },
    DataCase {
        name: "a NOT NULL column without a default added to rows",
        old: "CREATE TABLE t (id integer PRIMARY KEY);",
        rows: "INSERT INTO t VALUES (1);",
        new: "CREATE TABLE t (id integer PRIMARY KEY, score integer NOT NULL);",
        statement: "ADD COLUMN score",
        hazard: Hazard::CanFail,
        expect: Expect::Fails("contains null values"),
    },
    DataCase {
        name: "UNIQUE added over duplicates",
        old: "CREATE TABLE t (id integer PRIMARY KEY, email text);",
        rows: "INSERT INTO t VALUES (1, 'a@x'), (2, 'a@x');",
        new: "CREATE TABLE t (id integer PRIMARY KEY, email text CONSTRAINT t_email_key UNIQUE);",
        statement: "ADD CONSTRAINT t_email_key",
        hazard: Hazard::CanFail,
        expect: Expect::Fails("could not create unique index"),
    },
    DataCase {
        name: "a column dropped",
        old: "CREATE TABLE t (id integer PRIMARY KEY, legacy text);",
        rows: "INSERT INTO t VALUES (1, 'kept until now');",
        new: "CREATE TABLE t (id integer PRIMARY KEY);",
        statement: "DROP COLUMN legacy",
        hazard: Hazard::DeletesData,
        expect: Expect::Runs {
            query: "SELECT count(*)::text FROM information_schema.columns WHERE table_name = 't' AND column_name = 'legacy'",
            value: "0",
        },
    },
];

#[test]
fn hazards_are_what_happens_to_rows() {
    with_compiler(|compiler| async move {
        let admin = admin_url().expect("checked by with_compiler");
        let mut failures = Vec::new();
        for case in CASES {
            let old = compile(&compiler, case.old)
                .await
                .expect("the old schema compiles");
            let new = compile(&compiler, case.new)
                .await
                .expect("the new schema compiles");
            let m = migration(
                &old,
                &new,
                &crate::options_for(&compiler, &Options::default()),
            );
            let Some(flagged) = m.statements.iter().find(|s| s.sql.contains(case.statement)) else {
                failures.push(format!(
                    "{}: no statement with {:?} in\n{}",
                    case.name,
                    case.statement,
                    m.script()
                ));
                continue;
            };
            if flagged.hazard != Some(case.hazard) {
                failures.push(format!(
                    "{}: {} is {:?}, expected {:?}",
                    case.name, flagged.sql, flagged.hazard, case.hazard
                ));
            }
            let (name, mut db) = scratch(&admin).await.expect("a scratch database");
            let outcome = async {
                script(&mut db, case.old)
                    .await
                    .map_err(|e| format!("old: {e}"))?;
                script(&mut db, case.rows)
                    .await
                    .map_err(|e| format!("rows: {e}"))?;
                Ok::<_, String>(statements(&mut db, &m.script()).await)
            }
            .await;
            let result = match (outcome, &case.expect) {
                (Err(setup), _) => Err(setup),
                (Ok(Ok(())), Expect::Runs { query, value }) => {
                    let got: String = sqlx::query(*query)
                        .fetch_one(&mut db)
                        .await
                        .map_or_else(|e| e.to_string(), |row| row.get(0));
                    if got == *value {
                        Ok(())
                    } else {
                        Err(format!("ran, but {query} is {got}, expected {value}"))
                    }
                }
                (Ok(Err(error)), Expect::Fails(expected)) if error.contains(expected) => Ok(()),
                (Ok(Err(error)), Expect::Fails(expected)) => Err(format!(
                    "failed with {error}, expected an error with {expected:?}"
                )),
                (Ok(Ok(())), Expect::Fails(expected)) => Err(format!(
                    "ran over rows that break it (expected an error with {expected:?}): the data was changed silently"
                )),
                (Ok(Err(error)), Expect::Runs { .. }) => Err(format!("failed: {error}")),
            };
            drop_scratch(&admin, &name, db).await;
            if let Err(e) = result {
                failures.push(format!("{}: {e}\n{}", case.name, m.script()));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    });
}
