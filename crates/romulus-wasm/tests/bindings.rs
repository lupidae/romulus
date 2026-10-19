//! The functions the hero calls, called as the browser calls them: JSON
//! strings in, JSON strings out.

mod common;

use romulus_wasm::bindings::{js_compile, js_diff, js_migration, js_syntax_error, js_texts};
use serde_json::{Value, json};

fn compiled(sql: &str) -> Value {
    let tree = common::tree(sql).expect("parses");
    serde_json::from_str(&js_compile(sql, &tree)).expect("JSON")
}

const V1: &str = "CREATE TABLE customers (id uuid PRIMARY KEY, email text NOT NULL);";
const V2: &str =
    "CREATE TABLE customers (id uuid PRIMARY KEY, email text NOT NULL, phone text NOT NULL);";

#[test]
fn compile_answers_a_model_or_located_errors() {
    let ok = compiled(V1);
    assert_eq!(ok["model"]["entities"][0]["name"], "customers");
    assert_eq!(ok["diagnostics"], json!([]));

    let sql = "CREATE TABLE t (a int REFERENCES nowhere);";
    let failed = compiled(sql);
    assert_eq!(failed["model"], Value::Null);
    let error = &failed["diagnostics"][0];
    assert_eq!(error["severity"], "error");
    assert_eq!(error["code"], "sql");
    assert_eq!(error["sqlstate"], "42P01");
    assert_eq!(error["message"], "relation \"nowhere\" does not exist");
    let start = usize::try_from(error["span"]["utf16_start"].as_u64().unwrap()).unwrap();
    assert_eq!(&sql[start..start + "nowhere".len()], "nowhere");
}

#[test]
fn a_syntax_error_is_located_at_libpg_querys_cursor() {
    let sql = "CREATE TABLE t (\n  id int,\n  oops oops oops\n);";
    // libpg_query's cursorpos: 1-based, in characters.
    let cursor = u32::try_from(sql.find("oops oops").unwrap() + "oops ".len() + 1).unwrap();
    let out: Value = serde_json::from_str(&js_syntax_error(
        sql,
        "syntax error at or near \"oops\"",
        cursor,
    ))
    .unwrap();
    let error = &out["diagnostics"][0];
    assert_eq!(error["code"], "syntax_error");
    assert_eq!(error["span"]["from"], json!({"line": 3, "column": 8}));
    assert_eq!(out["model"], Value::Null);
}

#[test]
fn diff_and_migration_answer_what_the_api_serves() {
    let before = compiled(V1)["model"].to_string();
    let after = compiled(V2)["model"].to_string();

    let diff: Value = serde_json::from_str(&js_diff(&before, &after)).unwrap();
    assert_eq!(diff["diff"]["summary"]["text"], "1 table changed");
    assert!(
        diff["sql"]["after"]
            .as_str()
            .unwrap()
            .contains("phone text NOT NULL")
    );
    assert!(!diff["sql"]["before"].as_str().unwrap().contains("phone"));

    let request = json!({
        "format": "dbmate", "concurrently": false, "from": 1, "to": 2,
        "from_label": "v1", "to_label": "draft", "database": "acme/billing",
        "date": "2026-10-19", "timestamp": "20261019120000",
    })
    .to_string();
    let migration: Value = serde_json::from_str(&js_migration(&before, &after, &request)).unwrap();
    assert_eq!(migration["format"], "dbmate");
    assert_eq!(migration["pg_major"], 17);
    let file = migration["files"][0]["content"].as_str().unwrap();
    assert!(file.contains("-- migrate:up"), "{file}");
    assert!(file.contains("-- migrate:down"), "{file}");
    assert!(file.contains("ADD COLUMN phone text"), "{file}");
    assert_eq!(
        migration["destructive"], 1,
        "adding a NOT NULL column can fail on existing rows"
    );
}

#[test]
fn texts_are_remus_formats() {
    let model = compiled(V1)["model"].to_string();
    let texts: Value = serde_json::from_str(&js_texts(&model)).unwrap();
    assert!(texts["dbml"].as_str().unwrap().contains("Table"));
    assert!(texts["mermaid"].as_str().unwrap().contains("erDiagram"));
    assert!(texts["json"].as_str().unwrap().contains("\"customers\""));
}

#[test]
fn unreadable_arguments_answer_an_error() {
    let error: Value = serde_json::from_str(&js_diff("{", "{}")).unwrap();
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .starts_with("not a remus model")
    );
    let model = compiled(V1)["model"].to_string();
    let error: Value = serde_json::from_str(&js_migration(&model, &model, "{}")).unwrap();
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .starts_with("not a migration request")
    );
    let out: Value = serde_json::from_str(&js_compile(V1, "not json")).unwrap();
    assert_eq!(out["diagnostics"][0]["code"], "syntax_error");
}
