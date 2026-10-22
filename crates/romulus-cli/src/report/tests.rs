use serde_json::json;

use super::*;

const ORDERS: &str = "-- add orders\nCREATE TABLE orders (id bigint PRIMARY KEY, note text);\nCREATE INDEX ON customers (name);\n";
const NOT_NULL: &str = "ALTER TABLE customers ADD COLUMN email text;\n\nALTER TABLE customers ALTER COLUMN email SET NOT NULL;\n";
const LATER: &str = "CREATE TABLE refunds (id bigint);\n";

fn files() -> BTreeMap<String, String> {
    [
        ("1_orders.sql", ORDERS),
        ("2_not_null.sql", NOT_NULL),
        ("3_later.sql", LATER),
    ]
    .into_iter()
    .map(|(p, c)| (p.to_owned(), c.to_owned()))
    .collect()
}

fn reporting(files: &BTreeMap<String, String>, allow_locks: bool) -> Reporting<'_> {
    Reporting {
        database: "acme/shop".into(),
        label: Some("PR #42: add-orders".into()),
        commit: Some("0123456789abcdef".into()),
        dir: "db/migrations".into(),
        files,
        allow_locks,
        skipped_files: Vec::new(),
    }
}

fn statement(index: usize, sql: &str, status: &str) -> Value {
    json!({
        "index": index, "sql": sql, "status": status, "duration_ms": 3,
        "locks": [], "rewrote": [], "error": null, "offending": null,
    })
}

/// The three migrations, five statements: `1_orders` (0, 1), `2_not_null`
/// (2, 3), `3_later` (4).
fn migrations() -> Value {
    json!([
        { "path": "1_orders.sql", "line": 1, "sql_line": 2, "lines": 3, "first_statement": 0, "statements": 2 },
        { "path": "2_not_null.sql", "line": 1, "sql_line": 7, "lines": 3, "first_statement": 2, "statements": 2 },
        { "path": "3_later.sql", "line": 1, "sql_line": 12, "lines": 1, "first_statement": 4, "statements": 1 },
    ])
}

fn statements() -> Vec<Value> {
    vec![
        statement(
            0,
            "CREATE TABLE orders (id bigint PRIMARY KEY, note text);",
            "passed",
        ),
        statement(1, "CREATE INDEX ON customers (name);", "passed"),
        statement(2, "ALTER TABLE customers ADD COLUMN email text;", "passed"),
        statement(
            3,
            "ALTER TABLE customers ALTER COLUMN email SET NOT NULL;",
            "passed",
        ),
        statement(4, "CREATE TABLE refunds (id bigint);", "passed"),
    ]
}

/// A finished test: `status`, `verdict`, the up statements, data loss.
#[allow(clippy::needless_pass_by_value, reason = "built in place by each test")]
fn tested(status: &str, verdict: &str, statements: Vec<Value>, data_loss: Value) -> Taken {
    serde_json::from_value(json!({
        "outcome": "tested",
        "imported": 12,
        "migrations": migrations(),
        "draft": { "id": "d1", "name": "PR #42: add-orders", "base_version": 14, "reused": true, "page": "https://r.example/app/acme/shop/drafts/d1" },
        "page": "https://r.example/app/acme/shop/tests/t1",
        "test": {
            "id": "t1", "status": status, "verdict": verdict, "from": 14, "rows_checked": true,
            "acknowledged": null, "data": { "size": "small" },
            "report": {
                "verdict": verdict,
                "headline": "Up ran on 120 rows.",
                "rows": 120,
                "phases": [{
                    "phase": "before", "status": "passed", "summary": "", "checks": [{
                        "kind": "row_counts", "status": "passed", "summary": "", "details": [],
                        "tables": [{ "table": "public.customers" }],
                    }],
                }],
                "up": { "statements": statements },
                "data_loss": data_loss,
                "nothing_checked": false,
                "empty_tables": [],
            },
        },
    }))
    .unwrap()
}

fn passed() -> Taken {
    tested("passed", "passed_up_only", statements(), json!([]))
}

#[test]
fn nothing_new_is_a_success_that_says_so() {
    let files = files();
    let taken: Taken = serde_json::from_value(
        json!({ "outcome": "nothing_new", "imported": 9, "migrations": [] }),
    )
    .unwrap();
    let outcome = reporting(&files, false).taken(&taken);
    assert_eq!(outcome.result, RunResult::NothingNew);
    assert_eq!(outcome.exit_code, 0);
    assert!(
        outcome.text().contains("nothing to test"),
        "{}",
        outcome.text()
    );
}

#[test]
fn passing_migrations_exit_zero_with_each_file_and_the_report_link() {
    let files = files();
    // No lock worth a word: the index is on customers, so make it concurrent.
    let mut taken = passed();
    taken
        .test
        .as_mut()
        .unwrap()
        .report
        .as_mut()
        .unwrap()
        .up
        .statements[1]
        .sql = "CREATE INDEX CONCURRENTLY ON customers (name);".into();
    let outcome = reporting(&files, false).taken(&taken);
    assert_eq!(outcome.result, RunResult::Passed, "{:?}", outcome.findings);
    assert_eq!(outcome.exit_code, 0);
    assert!(
        outcome
            .migrations
            .iter()
            .all(|m| m.status == Status::Passed)
    );
    let text = outcome.text();
    assert!(
        text.starts_with("romulus test: acme/shop, PR #42: add-orders (0123456)\n"),
        "{text}"
    );
    assert!(
        text.contains("3 new migrations (12 imported already), on v14 with 120 generated rows"),
        "{text}"
    );
    assert!(
        text.contains("  PASS  db/migrations/1_orders.sql (2 statements, 6 ms)"),
        "{text}"
    );
    assert!(text.contains("PASSED: Up ran on 120 rows."), "{text}");
    assert!(
        text.contains("Report: https://r.example/app/acme/shop/tests/t1"),
        "{text}"
    );
    assert!(
        text.contains("Draft:  https://r.example/app/acme/shop/drafts/d1 (PR #42: add-orders)"),
        "{text}"
    );
    assert_eq!(outcome.annotations(), "");
}

#[test]
fn a_failed_statement_names_its_file_line_and_the_rows_that_break_it() {
    let files = files();
    let mut list = statements();
    list[3]["status"] = json!("failed");
    list[3]["error"] = json!({ "sqlstate": "23502", "message": "column \"email\" of relation \"customers\" contains null values" });
    list[3]["offending"] =
        json!({ "count": 120, "query": "SELECT …", "sample": ["(id)=(1)", "(id)=(2)"] });
    list[4]["status"] = json!("not_run");
    let taken = tested("failed", "failed", list, json!([]));
    let outcome = reporting(&files, false).taken(&taken);
    assert_eq!(outcome.result, RunResult::Failed);
    assert_eq!(outcome.exit_code, 1);
    let statuses: Vec<_> = outcome.migrations.iter().map(|m| m.status).collect();
    assert_eq!(statuses[1..], [Status::Failed, Status::NotRun]);
    let failure = outcome.migrations[1].failure.as_ref().unwrap();
    assert_eq!(failure.statement, 2);
    let finding = outcome
        .findings
        .iter()
        .find(|f| f.kind == "statement_failed")
        .unwrap();
    assert_eq!(
        finding.path.as_deref(),
        Some("db/migrations/2_not_null.sql")
    );
    // The statement is on line 3 of its file.
    assert_eq!(finding.line, Some(3));
    let text = outcome.text();
    assert!(
        text.contains("  FAIL  db/migrations/2_not_null.sql: statement 2 of 2 failed"),
        "{text}"
    );
    assert!(text.contains("ERROR 23502: column \"email\""), "{text}");
    assert!(
        text.contains("120 rows break it: (id)=(1), (id)=(2)"),
        "{text}"
    );
    assert!(
        text.contains("  SKIP  db/migrations/3_later.sql (not run"),
        "{text}"
    );
    assert!(text.contains("\nFAILED: "), "{text}");
    assert!(
        outcome
            .annotations()
            .contains("::error file=db/migrations/2_not_null.sql,line=3::romulus: column"),
        "{}",
        outcome.annotations()
    );
}

#[test]
fn rewriting_an_existing_table_fails_unless_locks_are_allowed() {
    let files = files();
    let mut list = statements();
    list[3]["rewrote"] = json!(["public.customers"]);
    list[3]["locks"] = json!([{ "relation": "public.customers", "mode": "AccessExclusiveLock" }]);
    // A rewrite of a table the migration made itself is nobody's wait.
    list[0]["rewrote"] = json!(["public.orders"]);
    let mut taken = tested("passed", "passed_up_only", list, json!([]));
    taken
        .test
        .as_mut()
        .unwrap()
        .report
        .as_mut()
        .unwrap()
        .up
        .statements[1]
        .sql = "CREATE INDEX CONCURRENTLY ON customers (name);".into();

    let outcome = reporting(&files, false).taken(&taken);
    assert_eq!(outcome.exit_code, 1);
    let rewrites: Vec<_> = outcome
        .findings
        .iter()
        .filter(|f| f.kind == "rewrite")
        .collect();
    assert_eq!(rewrites.len(), 1, "{rewrites:?}");
    assert!(rewrites[0].fails);
    assert_eq!(rewrites[0].line, Some(3));
    assert!(rewrites[0].message.contains("rewrites public.customers"));
    assert!(
        outcome
            .text()
            .contains("Problems:\n  - db/migrations/2_not_null.sql:3: rewrites public.customers"),
        "{}",
        outcome.text()
    );

    let allowed = reporting(&files, true).taken(&taken);
    assert_eq!(allowed.exit_code, 0);
    assert!(
        allowed
            .text()
            .contains("Warnings:\n  - db/migrations/2_not_null.sql:3: rewrites"),
        "{}",
        allowed.text()
    );
}

#[test]
fn an_index_built_without_concurrently_on_an_existing_table_blocks_writes() {
    let files = files();
    let mut list = statements();
    list[1]["locks"] = json!([{ "relation": "public.customers", "mode": "ShareLock" }]);
    let outcome = reporting(&files, false).taken(&tested(
        "passed",
        "passed_up_only",
        list.clone(),
        json!([]),
    ));
    let finding = outcome
        .findings
        .iter()
        .find(|f| f.kind == "blocking_index")
        .unwrap();
    assert!(finding.fails);
    assert_eq!(finding.path.as_deref(), Some("db/migrations/1_orders.sql"));
    assert_eq!(finding.line, Some(3));
    assert_eq!(outcome.exit_code, 1);

    // On a table the migration created (not counted before): nobody waits.
    list[1]["locks"] = json!([{ "relation": "public.orders", "mode": "ShareLock" }]);
    let outcome = reporting(&files, false).taken(&tested(
        "passed",
        "passed_up_only",
        list.clone(),
        json!([]),
    ));
    assert!(outcome.findings.is_empty(), "{:?}", outcome.findings);

    // Concurrently: no lock that blocks writes.
    list[1]["sql"] = json!("CREATE INDEX CONCURRENTLY ON customers (name);");
    list[1]["locks"] =
        json!([{ "relation": "public.customers", "mode": "ShareUpdateExclusiveLock" }]);
    let outcome =
        reporting(&files, false).taken(&tested("passed", "passed_up_only", list, json!([])));
    assert_eq!(outcome.exit_code, 0);
}

#[test]
fn data_no_statement_declares_fails_until_acknowledged() {
    let files = files();
    let loss = json!([{
        "expected": false, "status": "needs_acknowledgement",
        "message": "public.customers.email: 3 of 10 sampled values do not come back.",
        "cause": { "direction": "up", "index": 3 },
    }]);
    let mut list = statements();
    list[1]["sql"] = json!("CREATE INDEX CONCURRENTLY ON customers (name);");
    let taken = tested(
        "needs_acknowledgement",
        "needs_acknowledgement",
        list.clone(),
        loss.clone(),
    );
    let outcome = reporting(&files, false).taken(&taken);
    assert_eq!(outcome.exit_code, 1);
    let finding = outcome
        .findings
        .iter()
        .find(|f| f.kind == "undeclared_data_loss")
        .unwrap();
    assert!(finding.fails);
    assert_eq!(
        finding.path.as_deref(),
        Some("db/migrations/2_not_null.sql")
    );
    assert!(
        finding
            .message
            .contains("acknowledge it on the test's page")
    );

    // Acknowledged in romulus: the test passed, the job does too.
    let mut acknowledged = tested("passed", "needs_acknowledgement", list, loss);
    acknowledged.test.as_mut().unwrap().acknowledged =
        Some(json!({ "by": null, "at": "2026-10-22T10:00:00Z" }));
    let outcome = reporting(&files, false).taken(&acknowledged);
    assert_eq!(outcome.exit_code, 0, "{:?}", outcome.findings);
    assert!(outcome.text().contains("(acknowledged in romulus)"));
}

#[test]
fn data_a_statement_says_it_removes_only_warns() {
    let files = files();
    let mut list = statements();
    list[1]["sql"] = json!("CREATE INDEX CONCURRENTLY ON customers (name);");
    let loss = json!([{
        "expected": true, "status": "warning",
        "message": "public.customers.note: the column is dropped, its 10 sampled values with it.",
        "cause": { "direction": "up", "index": 2 },
    }]);
    let outcome = reporting(&files, false).taken(&tested("passed", "passed_up_only", list, loss));
    assert_eq!(outcome.exit_code, 0);
    let finding = outcome
        .findings
        .iter()
        .find(|f| f.kind == "data_loss")
        .unwrap();
    assert!(!finding.fails);
    assert!(
        outcome
            .annotations()
            .starts_with("::warning file=db/migrations/2_not_null.sql,line=1::"),
        "{}",
        outcome.annotations()
    );
}

#[test]
fn migrations_that_do_not_apply_fail_on_their_file_and_line() {
    let files = files();
    let taken: Taken = serde_json::from_value(json!({
        "outcome": "compile_failed",
        "imported": 12,
        "migrations": migrations(),
        "draft": { "id": "d1", "name": "PR #42", "base_version": 14, "reused": false, "page": "p" },
        "diagnostics": [{
            "path": "2_not_null.sql", "line": 3, "severity": "error",
            "message": "column \"email\" of relation \"customers\" already exists",
        }],
    }))
    .unwrap();
    let outcome = reporting(&files, false).taken(&taken);
    assert_eq!(outcome.exit_code, 1);
    assert_eq!(outcome.migrations[1].status, Status::Invalid);
    let text = outcome.text();
    assert!(
        text.contains("  FAIL  db/migrations/2_not_null.sql (does not apply"),
        "{text}"
    );
    assert!(
        text.contains("db/migrations/2_not_null.sql:3: column \"email\""),
        "{text}"
    );
}

#[test]
fn a_test_that_could_not_run_or_did_not_end_is_exit_two() {
    let files = files();
    let mut taken = passed();
    let test = taken.test.as_mut().unwrap();
    test.status = "error".into();
    test.report = None;
    test.failure = Some("The playground could not be built.".into());
    let outcome = reporting(&files, false).taken(&taken);
    assert_eq!(outcome.result, RunResult::Error);
    assert_eq!(outcome.exit_code, 2);
    assert!(
        outcome
            .text()
            .contains("ERROR: The playground could not be built.")
    );

    let mut running = passed();
    running.test.as_mut().unwrap().status = "running".into();
    let outcome = reporting(&files, false)
        .taken(&running)
        .timed_out("gave up after 60 s".into());
    assert_eq!(outcome.result, RunResult::Timeout);
    assert_eq!(outcome.exit_code, 2);
    assert!(outcome.text().contains("TIMED OUT"), "{}", outcome.text());
    assert!(
        outcome
            .text()
            .contains("Report: https://r.example/app/acme/shop/tests/t1")
    );
}

#[test]
fn files_romulus_cannot_read_as_migrations_fail_with_their_line() {
    let files = files();
    let details = json!({
        "index": 1, "id": "2_not_null", "name": "2_not_null.sql", "path": "2_not_null.sql",
        "errors": [{ "line": 3, "message": "syntax error at or near \"NUL\"" }],
    });
    let outcome = reporting(&files, false).refused(
        "import_failed",
        "2_not_null.sql:3: syntax error at or near \"NUL\". Nothing was imported.",
        Some(&details),
    );
    assert_eq!(outcome.exit_code, 1);
    assert_eq!(
        outcome.findings[0].path.as_deref(),
        Some("db/migrations/2_not_null.sql")
    );
    assert_eq!(outcome.findings[0].line, Some(3));

    let changed = reporting(&files, false).refused(
        "migration_changed",
        "1_orders.sql changed since it was imported as v3.",
        None,
    );
    assert_eq!(changed.exit_code, 1);
    assert!(
        changed
            .text()
            .contains("1_orders.sql changed since it was imported"),
        "{}",
        changed.text()
    );
}

#[test]
fn errors_say_what_happened_and_exit_two() {
    let files = files();
    let outcome =
        reporting(&files, false).error(RunResult::Error, "token", "romulus refused ROMULUS_TOKEN");
    assert_eq!(outcome.exit_code, 2);
    assert!(
        outcome
            .text()
            .ends_with("\nERROR: romulus refused ROMULUS_TOKEN\n"),
        "{}",
        outcome.text()
    );
}

#[test]
fn the_json_is_the_whole_outcome() {
    let files = files();
    let outcome = reporting(&files, false).taken(&passed());
    let json: Value = serde_json::from_str(&outcome.json()).unwrap();
    assert_eq!(json["database"], "acme/shop");
    assert_eq!(json["exit_code"], outcome.exit_code);
    assert_eq!(json["migrations"].as_array().unwrap().len(), 3);
    assert_eq!(
        json["test_page"],
        "https://r.example/app/acme/shop/tests/t1"
    );
    assert_eq!(json["verdict"], "passed_up_only");
}

#[test]
fn annotation_text_is_escaped() {
    assert_eq!(escape_data("50% of\nrows"), "50%25 of%0Arows");
    assert_eq!(escape_property("a:b,c"), "a%3Ab%2Cc");
}

#[test]
fn lines_are_found_in_the_file() {
    assert_eq!(
        line_in(
            NOT_NULL,
            "ALTER TABLE customers ALTER COLUMN email SET NOT NULL;"
        ),
        Some(3)
    );
    assert_eq!(
        line_in(
            ORDERS,
            "CREATE TABLE orders (id bigint PRIMARY KEY, note text);"
        ),
        Some(2)
    );
    assert_eq!(
        line_in(
            "--- users\nCREATE TABLE u ();\n",
            "-- - users\nCREATE TABLE u ();"
        ),
        Some(2)
    );
    assert_eq!(line_in(ORDERS, "DROP TABLE nowhere;"), None);
}
