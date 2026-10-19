//! The engine never panics: in the browser a panic aborts the wasm
//! instance. Every text a person passes through while typing (cut short, a
//! line gone, a statement twice) compiles to a model or to errors, and so
//! do parse trees that do not match their text. No sandbox needed.

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};

/// The hero's seed (web/src/hero/seed.ts `SEED_SQL`).
const HERO_SEED: &str = "CREATE TYPE invoice_status AS ENUM ('draft', 'open', 'paid', 'void');

CREATE TABLE customers (
  id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  email      text NOT NULL UNIQUE,
  name       text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE invoices (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  customer_id uuid NOT NULL REFERENCES customers ON DELETE CASCADE,
  status      invoice_status NOT NULL DEFAULT 'draft',
  total_cents bigint NOT NULL DEFAULT 0 CHECK (total_cents >= 0),
  issued_at   timestamptz
);
CREATE INDEX invoices_customer_id_idx ON invoices (customer_id);
COMMENT ON COLUMN invoices.total_cents IS 'Sum of the lines, in cents';

CREATE TABLE invoice_lines (
  id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  invoice_id  uuid NOT NULL REFERENCES invoices ON DELETE CASCADE,
  description text NOT NULL,
  quantity    integer NOT NULL DEFAULT 1 CHECK (quantity > 0),
  unit_cents  bigint NOT NULL
);

CREATE TABLE payments (
  id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  invoice_id uuid NOT NULL REFERENCES invoices,
  amount     numeric(12, 2) NOT NULL CHECK (amount > 0),
  paid_at    timestamptz NOT NULL DEFAULT now()
);
";

fn texts() -> Vec<(String, String)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/oracle");
    let mut out: Vec<(String, String)> = std::fs::read_dir(dir)
        .expect("tests/oracle")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .map(|p| {
            let sql = std::fs::read_to_string(&p).expect("the file");
            (p.display().to_string(), sql)
        })
        .collect();
    out.sort();
    out.push(("hero seed".to_owned(), HERO_SEED.to_owned()));
    out
}

/// Every text met on the way: each prefix cut at a line's end, the text
/// with one line deleted, the text with one statement written twice.
fn variants(sql: &str) -> Vec<String> {
    let lines: Vec<&str> = sql.split_inclusive('\n').collect();
    let mut out = Vec::new();
    for n in 0..=lines.len() {
        out.push(lines[..n].concat());
    }
    for skip in 0..lines.len() {
        out.push(
            lines
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != skip)
                .map(|(_, l)| *l)
                .collect(),
        );
    }
    if let Ok(statements) = pg_query::split_with_parser(sql) {
        for twice in 0..statements.len() {
            let mut text = String::new();
            for (i, s) in statements.iter().enumerate() {
                text.push_str(s);
                text.push_str(";\n");
                if i == twice {
                    text.push_str(s);
                    text.push_str(";\n");
                }
            }
            out.push(text);
        }
    }
    out
}

#[test]
fn texts_being_typed_never_panic() {
    let mut panicked = Vec::new();
    let mut compiled = 0;
    for (name, sql) in texts() {
        for text in variants(&sql) {
            compiled += 1;
            if catch_unwind(AssertUnwindSafe(|| common::compile(&text))).is_err() {
                panicked.push(format!("{name}:\n{text}"));
            }
        }
    }
    assert!(compiled > 500, "only {compiled} texts compiled");
    assert!(
        panicked.is_empty(),
        "panicked on:\n{}",
        panicked.join("\n---\n")
    );
}

#[test]
fn a_tree_that_is_not_the_text_never_panics() {
    let texts = texts();
    let short = "CREATE TABLE é (a int);";
    let short_tree = common::tree(short).expect("parses");
    let mut panicked = Vec::new();
    for (name, sql) in &texts {
        let tree = common::tree(sql).expect("parses");
        // The text and the tree of another: offsets past the end, inside
        // a character, statements that are not there.
        for (text, tree) in [(short, tree.as_str()), (sql.as_str(), short_tree.as_str())] {
            if catch_unwind(|| romulus_wasm::compile(text, tree)).is_err() {
                panicked.push(name.clone());
            }
        }
    }
    for tree in [
        "",
        "null",
        "[]",
        "{}",
        "{\"stmts\": 5}",
        "{\"stmts\": [{}]}",
        "not json",
    ] {
        if catch_unwind(|| romulus_wasm::compile(HERO_SEED, tree)).is_err() {
            panicked.push(format!("tree {tree}"));
        }
    }
    for cursor in [0, 1, 5, 10_000, u32::MAX] {
        if catch_unwind(|| romulus_wasm::syntax_error("SELEC é", "syntax error", cursor)).is_err()
        {
            panicked.push(format!("cursor {cursor}"));
        }
    }
    assert!(panicked.is_empty(), "panicked on: {panicked:?}");
}

#[test]
fn huge_exponents_are_kept_as_written() {
    let model =
        common::model("CREATE TABLE t (a numeric DEFAULT 1e999999999, b numeric DEFAULT 1.5e3);");
    let entity = model.entities.first().expect("the table");
    let defaults: Vec<_> = entity.columns.iter().map(|c| c.default.clone()).collect();
    assert_eq!(defaults[1].as_deref(), Some("'1500'::numeric"));
    assert!(defaults[0].is_some());
}
