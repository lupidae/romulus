//! Migrations on tables with rows: what a hazard says is what happens. A
//! `deletes_data` statement runs and the values come out changed; a
//! `can_fail` statement stops on the rows that break it, and nothing is
//! lost. Each case builds the old schema on a scratch database, inserts a
//! few rows, and applies the plain migration statement by statement.
//!
//! Renames keep rows: a row inserted before the up migration reads the same
//! after it under the new names, and after the down under the old ones
//! (swaps included).

use romulus_migration::{Hazard, Options, down, migration};
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

struct RenameCase {
    name: &'static str,
    old: &'static str,
    rows: &'static str,
    /// The new schema; its `ALTER … RENAME` beyond `old` are known renames.
    new: &'static str,
    /// Detected renames turned on or off.
    decide: &'static [(&'static str, bool)],
    /// Read after the up migration, then what it reads.
    up: (&'static str, &'static str),
    /// Read after the down migration.
    down: (&'static str, &'static str),
}

const RENAMES: &[RenameCase] = &[
    RenameCase {
        name: "a column renamed, detected",
        old: "CREATE TABLE users (id integer PRIMARY KEY, email text NOT NULL UNIQUE);",
        rows: "INSERT INTO users VALUES (1, 'ada@example.com');",
        new: "CREATE TABLE users (id integer PRIMARY KEY, email_address text NOT NULL UNIQUE);",
        decide: &[],
        up: ("SELECT email_address FROM users", "ada@example.com"),
        down: ("SELECT email FROM users", "ada@example.com"),
    },
    RenameCase {
        name: "a table and its column renamed, known",
        old: "CREATE TABLE customers (id serial PRIMARY KEY, name text NOT NULL);",
        rows: "INSERT INTO customers (name) VALUES ('Ada');",
        new: "CREATE TABLE customers (id serial PRIMARY KEY, name text NOT NULL);\nALTER TABLE customers RENAME TO clients;\nALTER TABLE clients RENAME COLUMN name TO full_name;",
        decide: &[],
        up: ("SELECT id || ' ' || full_name FROM clients", "1 Ada"),
        down: ("SELECT id || ' ' || name FROM customers", "1 Ada"),
    },
    RenameCase {
        name: "two columns swapped",
        old: "CREATE TABLE t (id integer PRIMARY KEY, a text NOT NULL, b integer NOT NULL);",
        rows: "INSERT INTO t VALUES (1, 'was a', 7);",
        new: "CREATE TABLE t (id integer PRIMARY KEY, a text NOT NULL, b integer NOT NULL);\nALTER TABLE t RENAME COLUMN a TO tmp;\nALTER TABLE t RENAME COLUMN b TO a;\nALTER TABLE t RENAME COLUMN tmp TO b;",
        decide: &[],
        up: ("SELECT b || '/' || a FROM t", "was a/7"),
        down: ("SELECT a || '/' || b FROM t", "was a/7"),
    },
    RenameCase {
        name: "two tables swapped",
        old: "CREATE TABLE cats (id integer PRIMARY KEY, meow text);\nCREATE TABLE dogs (id integer PRIMARY KEY, woof text);",
        rows: "INSERT INTO cats VALUES (1, 'meow');\nINSERT INTO dogs VALUES (2, 'woof');",
        new: "CREATE TABLE cats (id integer PRIMARY KEY, meow text);\nCREATE TABLE dogs (id integer PRIMARY KEY, woof text);\nALTER TABLE cats RENAME TO pets;\nALTER TABLE dogs RENAME TO cats;\nALTER TABLE pets RENAME TO dogs;",
        decide: &[],
        up: (
            "SELECT (SELECT meow FROM dogs) || '/' || (SELECT woof FROM cats)",
            "meow/woof",
        ),
        down: (
            "SELECT (SELECT meow FROM cats) || '/' || (SELECT woof FROM dogs)",
            "meow/woof",
        ),
    },
    RenameCase {
        name: "an enum value renamed, decided on",
        old: "CREATE TYPE mood AS ENUM ('sad', 'ok');\nCREATE TABLE t (id integer PRIMARY KEY, m mood NOT NULL);",
        rows: "INSERT INTO t VALUES (1, 'ok');",
        new: "CREATE TYPE mood AS ENUM ('sad', 'fine');\nCREATE TABLE t (id integer PRIMARY KEY, m mood NOT NULL);",
        decide: &[("enum_value/public/mood/ok/fine", true)],
        up: ("SELECT m::text FROM t", "fine"),
        down: ("SELECT m::text FROM t", "ok"),
    },
    RenameCase {
        name: "a table moved to another schema",
        old: "CREATE SCHEMA archive;\nCREATE TABLE notes (id serial PRIMARY KEY, body text NOT NULL);",
        rows: "INSERT INTO notes (body) VALUES ('kept');",
        new: "CREATE SCHEMA archive;\nCREATE TABLE notes (id serial PRIMARY KEY, body text NOT NULL);\nALTER TABLE notes SET SCHEMA archive;",
        decide: &[],
        up: ("SELECT id || ' ' || body FROM archive.notes", "1 kept"),
        down: ("SELECT id || ' ' || body FROM public.notes", "1 kept"),
    },
];

async fn read(
    db: &mut sqlx::PgConnection,
    (query, value): (&'static str, &'static str),
) -> Result<(), String> {
    let got: String = sqlx::query(query)
        .fetch_one(db)
        .await
        .map_or_else(|e| e.to_string(), |row| row.get(0));
    if got == value {
        Ok(())
    } else {
        Err(format!("{query} is {got}, expected {value}"))
    }
}

#[test]
fn renames_keep_rows_up_and_down() {
    with_compiler(|compiler| async move {
        let admin = admin_url().expect("checked by with_compiler");
        let mut failures = Vec::new();
        for case in RENAMES {
            let old = compile(&compiler, case.old)
                .await
                .expect("the old schema compiles");
            let new = compile(&compiler, case.new)
                .await
                .expect("the new schema compiles");
            let mut options = Options::default();
            options.renames.known = romulus_sandbox::known_renames(case.old, case.new);
            options
                .renames
                .decisions
                .extend(case.decide.iter().map(|(k, on)| ((*k).to_owned(), *on)));
            let up = migration(&old, &new, &crate::options_for(&compiler, &options));
            let back = down(&old, &new, &up);
            if up.applied.is_empty() {
                failures.push(format!("{}: nothing renamed\n{}", case.name, up.script()));
                continue;
            }
            let (name, mut db) = scratch(&admin).await.expect("a scratch database");
            let outcome = async {
                script(&mut db, case.old)
                    .await
                    .map_err(|e| format!("old: {e}"))?;
                script(&mut db, case.rows)
                    .await
                    .map_err(|e| format!("rows: {e}"))?;
                statements(&mut db, &up.script())
                    .await
                    .map_err(|e| format!("up: {e}"))?;
                read(&mut db, case.up)
                    .await
                    .map_err(|e| format!("after the up: {e}"))?;
                statements(&mut db, &back.script())
                    .await
                    .map_err(|e| format!("down: {e}"))?;
                read(&mut db, case.down)
                    .await
                    .map_err(|e| format!("after the down: {e}"))
            }
            .await;
            drop_scratch(&admin, &name, db).await;
            if let Err(e) = outcome {
                failures.push(format!(
                    "{}: {e}\n{}\n--- down ---\n{}",
                    case.name,
                    up.script(),
                    back.script()
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    });
}

/// A range of versions composed as the API composes them: v2 swaps two
/// columns, v3 renames one of them. The rows follow their column through
/// both, there and back.
#[test]
fn a_range_of_versions_keeps_each_column_s_rows() {
    use romulus_migration::{Rename, RenameChoices, RenameKind};
    let column = |from: &str, to: &str| Rename {
        kind: RenameKind::Column,
        schema: "public".into(),
        parent: Some("t".into()),
        from: from.into(),
        to: to.into(),
        to_schema: None,
    };
    let v1 = "CREATE TABLE t (id integer PRIMARY KEY, a integer, b text);";
    let v3 = "CREATE TABLE t (id integer PRIMARY KEY, c integer, a text);";
    let range = RenameChoices::compose([
        RenameChoices::exactly(vec![column("a", "b"), column("b", "a")]),
        RenameChoices::exactly(vec![column("b", "c")]),
    ]);
    with_compiler(|compiler| async move {
        let admin = admin_url().expect("checked by with_compiler");
        let old = compile(&compiler, v1).await.expect("v1 compiles");
        let new = compile(&compiler, v3).await.expect("v3 compiles");
        let options = |renames: RenameChoices| {
            crate::options_for(
                &compiler,
                &Options {
                    renames,
                    ..Options::default()
                },
            )
        };
        let up = migration(&old, &new, &options(range.clone()));
        let back = migration(&new, &old, &options(range.reversed()));
        assert!(
            up.statements
                .iter()
                .chain(&back.statements)
                .all(|s| !s.destructive),
            "{}\n--- back ---\n{}",
            up.script(),
            back.script()
        );
        let (name, mut db) = scratch(&admin).await.expect("a scratch database");
        let outcome = async {
            script(&mut db, v1).await.map_err(|e| format!("v1: {e}"))?;
            script(&mut db, "INSERT INTO t VALUES (1, 7, 'seven');")
                .await
                .map_err(|e| format!("rows: {e}"))?;
            statements(&mut db, &up.script())
                .await
                .map_err(|e| format!("up: {e}"))?;
            read(&mut db, ("SELECT c || '/' || a FROM t", "7/seven"))
                .await
                .map_err(|e| format!("after the up: {e}"))?;
            statements(&mut db, &back.script())
                .await
                .map_err(|e| format!("back: {e}"))?;
            read(&mut db, ("SELECT a || '/' || b FROM t", "7/seven"))
                .await
                .map_err(|e| format!("after the way back: {e}"))
        }
        .await;
        drop_scratch(&admin, &name, db).await;
        if let Err(e) = outcome {
            panic!("{e}\n{}\n--- back ---\n{}", up.script(), back.script());
        }
    });
}

/// One version swapping two tables' names and renaming a column of one,
/// as a version stores it (its net renames, by level, the column under
/// its table's final name): every row stays with its table, there and
/// back.
#[test]
fn a_table_swap_with_a_column_renamed_keeps_each_table_s_rows() {
    use romulus_migration::{Rename, RenameChoices, RenameKind};
    let rename = |kind, parent: Option<&str>, from: &str, to: &str| Rename {
        kind,
        schema: "public".into(),
        parent: parent.map(str::to_owned),
        from: from.into(),
        to: to.into(),
        to_schema: None,
    };
    let v1 = "CREATE TABLE cats (id integer PRIMARY KEY, meow text);\nCREATE TABLE dogs (id integer PRIMARY KEY, woof text);";
    // As the renames leave it: Postgres keeps the keys' names.
    let v2 = "CREATE TABLE cats (id integer CONSTRAINT dogs_pkey PRIMARY KEY, woof text);\nCREATE TABLE dogs (id integer CONSTRAINT cats_pkey PRIMARY KEY, purr text);";
    let stored = RenameChoices::exactly(vec![
        rename(RenameKind::Table, None, "cats", "dogs"),
        rename(RenameKind::Table, None, "dogs", "cats"),
        rename(RenameKind::Column, Some("dogs"), "meow", "purr"),
    ]);
    with_compiler(|compiler| async move {
        let admin = admin_url().expect("checked by with_compiler");
        let old = compile(&compiler, v1).await.expect("v1 compiles");
        let new = compile(&compiler, v2).await.expect("v2 compiles");
        let options = |renames: RenameChoices| {
            crate::options_for(
                &compiler,
                &Options {
                    renames,
                    ..Options::default()
                },
            )
        };
        // As the API reads a version: composed, the keys named after the
        // tables detected along.
        let range = RenameChoices::compose([stored.clone()]);
        let up = migration(&old, &new, &options(range.clone()));
        let back = migration(&new, &old, &options(range.reversed()));
        assert!(
            up.statements
                .iter()
                .chain(&back.statements)
                .all(|s| !s.destructive),
            "{}\n--- back ---\n{}",
            up.script(),
            back.script()
        );
        let (name, mut db) = scratch(&admin).await.expect("a scratch database");
        let outcome = async {
            script(&mut db, v1).await.map_err(|e| format!("v1: {e}"))?;
            script(
                &mut db,
                "INSERT INTO cats VALUES (1, 'meow');\nINSERT INTO dogs VALUES (2, 'woof');",
            )
            .await
            .map_err(|e| format!("rows: {e}"))?;
            statements(&mut db, &up.script())
                .await
                .map_err(|e| format!("up: {e}"))?;
            read(
                &mut db,
                (
                    "SELECT (SELECT id || purr FROM dogs) || ' ' || (SELECT id || woof FROM cats)",
                    "1meow 2woof",
                ),
            )
            .await
            .map_err(|e| format!("after the up: {e}"))?;
            statements(&mut db, &back.script())
                .await
                .map_err(|e| format!("back: {e}"))?;
            read(
                &mut db,
                (
                    "SELECT (SELECT id || meow FROM cats) || ' ' || (SELECT id || woof FROM dogs)",
                    "1meow 2woof",
                ),
            )
            .await
            .map_err(|e| format!("after the way back: {e}"))
        }
        .await;
        drop_scratch(&admin, &name, db).await;
        if let Err(e) = outcome {
            panic!("{e}\n{}\n--- back ---\n{}", up.script(), back.script());
        }
    });
}
