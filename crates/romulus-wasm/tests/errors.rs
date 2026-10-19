//! Errors: what Postgres refuses, the engine refuses with Postgres's own
//! message and SQLSTATE (the sandbox compiles each case as the oracle).

mod common;

const BASE: &str =
    "CREATE TABLE customers (id uuid PRIMARY KEY, email text NOT NULL UNIQUE, n int);\n";

/// Each case runs after `BASE`.
const CASES: &[&str] = &[
    "CREATE TABLE invoices (id int PRIMARY KEY, customer_id uuid REFERENCES clients);",
    "CREATE TABLE customers (id int);",
    "CREATE TABLE t (a int PRIMARY KEY, b int, PRIMARY KEY (b));",
    "CREATE TABLE t (a int REFERENCES customers (n));",
    "CREATE TABLE t (a int REFERENCES customers);",
    "CREATE TABLE t (a mood);",
    "CREATE TABLE t (a int CHECK (b > 0));",
    "CREATE TABLE t (a int, a text);",
    "ALTER TABLE customers ADD COLUMN email text;",
    "ALTER TABLE customers DROP COLUMN missing;",
    "CREATE TABLE t (a int REFERENCES customers (email)); DROP TABLE customers;",
    "CREATE TABLE t (a uuid REFERENCES customers); ALTER TABLE customers DROP COLUMN id;",
    "CREATE TABLE t (a int CHECK (a = 'x'::text));",
    "CREATE TABLE t (a uuid DEFAULT 0);",
    "CREATE TABLE t (a int CHECK (a + 1));",
    "CREATE TYPE mood AS ENUM ('a'); ALTER TYPE mood ADD VALUE 'a';",
    "CREATE TYPE mood AS ENUM ('a'); CREATE TABLE t (m mood); DROP TYPE mood;",
    "DROP TABLE nothing;",
    "CREATE VIEW v AS SELECT nope FROM customers;",
    "CREATE INDEX ON customers (nope);",
    "CREATE INDEX customers_pkey ON customers (n);",
    "ALTER TABLE customers ALTER COLUMN id DROP NOT NULL;",
    "CREATE TABLE billing.t (a int);",
    "COMMENT ON COLUMN customers.nope IS 'x';",
    "CREATE POLICY p ON customers USING (n > 0); CREATE POLICY p ON customers USING (true);",
    "CREATE TABLE t (a int GENERATED ALWAYS AS (random()) STORED);",
    "CREATE DOMAIN pos AS int CHECK (VALUE > 0); ALTER DOMAIN pos VALIDATE CONSTRAINT nope;",
    "CREATE DOMAIN pos AS int CHECK (VALUE > 0); ALTER DOMAIN pos RENAME CONSTRAINT nope TO yes;",
    "CREATE DOMAIN pos AS int CONSTRAINT a CHECK (VALUE > 0) CONSTRAINT b CHECK (VALUE < 9); ALTER DOMAIN pos RENAME CONSTRAINT a TO b;",
];

#[test]
fn errors_read_as_postgres_writes_them() {
    common::with_sandbox(|compiler| async move {
        let mut failures = Vec::new();
        for case in CASES {
            let sql = format!("{BASE}{case}");
            let compilation = compiler.compile(&sql).await.expect("the sandbox answers");
            let expected = compilation
                .diagnostics
                .iter()
                .find(|d| d.is_error())
                .map(|d| (d.message.clone(), d.sqlstate.clone()));
            let compiled = common::compile(&sql);
            let actual = compiled
                .diagnostics
                .iter()
                .find(|d| d.is_error())
                .map(|d| (d.message.clone(), d.sqlstate.clone()));
            if expected != actual {
                failures.push(format!(
                    "{case}\n  postgres: {expected:?}\n  engine:   {actual:?}"
                ));
            }
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    });
}
