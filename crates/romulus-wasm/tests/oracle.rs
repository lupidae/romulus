//! The oracle: every file of `tests/oracle/` compiled by the sandbox
//! Postgres (the app's compile) and by this engine must give the same
//! model. Skipped without `ROMULUS_SANDBOX_URL`, except in CI.

mod common;

use std::fmt::Write;

use remus_core::Schema;

/// Both models as the comparison sees them: canonical order, the same
/// header (the sandbox names its exact server version).
fn comparable(model: &Schema) -> String {
    let mut model = romulus_diff::normalize(model);
    model.server_version = String::new();
    model.database = String::new();
    model.remus_version = String::new();
    serde_json::to_string_pretty(&model).expect("serializes")
}

fn line_diff(expected: &str, actual: &str) -> String {
    let mut out = String::new();
    for change in similar::TextDiff::from_lines(expected, actual).iter_all_changes() {
        let sign = match change.tag() {
            similar::ChangeTag::Delete => "-",
            similar::ChangeTag::Insert => "+",
            similar::ChangeTag::Equal => continue,
        };
        let _ = write!(out, "{sign} {change}");
    }
    out
}

#[test]
fn the_engine_reads_sql_as_postgres_does() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/oracle");
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("tests/oracle")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    files.sort();
    common::with_sandbox(|compiler| async move {
        let mut failures = Vec::new();
        for file in &files {
            let sql = std::fs::read_to_string(file).expect("the file");
            let expected = comparable(&common::sandbox_model(compiler, &sql).await);
            let compiled = common::compile(&sql);
            let Some(model) = compiled.model else {
                failures.push(format!("{}: {:?}", file.display(), compiled.diagnostics));
                continue;
            };
            let actual = comparable(&model);
            if expected != actual {
                failures.push(format!(
                    "{}:\n{}",
                    file.display(),
                    line_diff(&expected, &actual)
                ));
            }
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
    });
}
