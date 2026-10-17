//! The sandbox corpus and remus's showcase, in both profiles: every row
//! planned is inserted, or nearly.

use romulus_fakedata::{Options, Profile, Report, Size};

use crate::{corpus, run, with_sandbox};

/// Loads every corpus schema; (planned, dropped) over all of them, or
/// `None` without a sandbox.
fn load_corpus(options: &Options) -> Option<(u64, u64)> {
    let totals = std::sync::Mutex::new(None);
    let totals_ref = &totals;
    with_sandbox(|compiler, admin| async move {
        let (mut planned, mut dropped) = (0, 0);
        let mut failures = Vec::new();
        for (name, sql) in corpus() {
            match run(&compiler, &admin, &name, &sql, options).await {
                Ok((report, problems)) => {
                    planned += report.rows_planned;
                    dropped += report.rows_dropped();
                    failures.extend(problems.into_iter().map(|p| format!("{name}: {p}")));
                    failures.extend(
                        strict(&report, options.profile == Profile::Realistic)
                            .into_iter()
                            .map(|p| format!("{name}: {p}")),
                    );
                }
                Err(e) => failures.push(format!("{name}: {e}")),
            }
        }
        assert!(
            failures.is_empty(),
            "{} problems:\n\n{}",
            failures.len(),
            failures.join("\n\n")
        );
        *totals_ref.lock().expect("the totals") = Some((planned, dropped));
    });
    totals.into_inner().expect("the totals")
}

/// What a corpus schema must also show: rows for every table, nothing
/// skipped but an extension's own tables, and (realistic rows) no table
/// that fell back to one row at a time.
fn strict(report: &Report, realistic: bool) -> Vec<String> {
    let mut problems = Vec::new();
    if report.rows_planned == 0 {
        problems.push("no rows planned".to_owned());
    }
    for t in &report.tables {
        if let Some(why) = &t.skipped {
            problems.push(format!("{}.{} skipped: {why}", t.schema, t.table));
        }
        if t.rows_planned == 0 {
            problems.push(format!("{}.{}: no rows planned", t.schema, t.table));
        }
        if realistic && t.row_by_row {
            problems.push(format!(
                "{}.{}: loaded one row at a time: {:?}",
                t.schema, t.table, t.notes
            ));
        }
    }
    for s in &report.skipped {
        if s.reason != "provided by an extension" {
            problems.push(format!("{} not loaded: {}", s.relation, s.reason));
        }
    }
    problems
}

#[test]
fn every_corpus_schema_loads_its_realistic_rows() {
    let Some((planned, dropped)) = load_corpus(&Options::new(Size::Small, Profile::Realistic, 7))
    else {
        return;
    };
    // Realistic rows are built to pass: a drop is a generator bug.
    assert_eq!(dropped, 0, "{dropped} of {planned} rows dropped");
}

#[test]
fn every_corpus_schema_loads_its_edge_case_rows() {
    let Some((planned, dropped)) = load_corpus(&Options::new(Size::Small, Profile::EdgeCases, 11))
    else {
        return;
    };
    assert!(
        dropped * 100 <= planned,
        "{dropped} of {planned} rows dropped"
    );
}
