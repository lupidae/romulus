//! Hand-written pairs, one file per kind of change (`tests/scenarios`): the
//! old schema, a `-- ==== new ====` line, the new one. Each is checked both
//! ways and with `concurrently`, and its migration is snapshotted, so a
//! change to the generated SQL shows in review.

use romulus_migration::{Format, Meta, Options, files, migration};

use crate::{
    Case, check_all, compile,
    tools::{ToolCase, check_tools},
    with_compiler,
};

fn scenarios() -> Vec<(String, String, String)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/scenarios");
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .expect("the scenarios")
        .map(|entry| entry.expect("a scenario").path())
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let name = path
                .file_stem()
                .expect("a name")
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(&path).expect("a readable scenario");
            let (old, new) = text
                .split_once("-- ==== new ====\n")
                .unwrap_or_else(|| panic!("{name}: no `-- ==== new ====` line"));
            (name, old.to_owned(), new.to_owned())
        })
        .collect()
}

#[test]
fn every_scenario_migrates_both_ways() {
    with_compiler(|compiler| async move {
        let mut cases = Vec::new();
        for (name, old, new) in scenarios() {
            let old = compile(&compiler, &old)
                .await
                .unwrap_or_else(|e| panic!("{name}: the old schema compiles:\n{e}"));
            let new = compile(&compiler, &new)
                .await
                .unwrap_or_else(|e| panic!("{name}: the new schema compiles:\n{e}"));
            let case = Case::new(name.clone(), &old, &new);
            let m = migration(&old, &new, &Options::default());
            assert!(!m.is_empty(), "{name}: the scenario changes nothing");
            let meta = Meta {
                database: "acme/app".to_owned(),
                from: "v1".to_owned(),
                to: "v2".to_owned(),
                date: "2026-10-16".to_owned(),
                version: "20261016093000".to_owned(),
                name: name.clone(),
            };
            let back = migration(&new, &old, &Options::default());
            let plain = files(&m, &back, Format::Plain, &meta);
            insta::assert_snapshot!(name.clone(), plain[0].content);
            cases.push(case.concurrently());
            cases.push(case.reversed());
            cases.push(case);
        }
        // The only pairs the compile cannot run move an extension (the
        // tool runs below run them as the cluster's superuser).
        let skipped = check_all(&compiler, cases).await;
        for (label, why) in &skipped {
            assert!(
                label.starts_with("extension_moved") && *why == crate::MOVED_EXTENSION,
                "{label} skipped: {why}"
            );
        }
        assert_eq!(
            skipped.len(),
            3,
            "extension_moved: forward, reversed, concurrently"
        );
    });
}

#[test]
fn every_scenario_runs_as_each_tool_would_run_it() {
    with_compiler(|compiler| async move {
        let mut cases = Vec::new();
        for (name, old_sql, new_sql) in scenarios() {
            let old = compile(&compiler, &old_sql)
                .await
                .expect("the old schema compiles");
            let new = compile(&compiler, &new_sql)
                .await
                .expect("the new schema compiles");
            // Built from the text itself too: what render_sql leaves out
            // (partitions' copies of their parent's indexes, under the
            // names Postgres chose) is there to migrate.
            for (format, concurrently) in [(Format::GolangMigrate, false), (Format::Plain, true)] {
                cases.push(ToolCase {
                    label: format!("{name} (as written)"),
                    old: old.clone(),
                    new: new.clone(),
                    options: Options { concurrently },
                    format,
                    sources: Some((old_sql.clone(), new_sql.clone())),
                });
            }
            for format in Format::ALL {
                for concurrently in [false, true] {
                    cases.push(ToolCase {
                        label: name.clone(),
                        old: old.clone(),
                        new: new.clone(),
                        options: Options { concurrently },
                        format,
                        sources: None,
                    });
                }
            }
            for format in [Format::Plain, Format::GolangMigrate] {
                cases.push(ToolCase {
                    label: format!("{name} (reversed)"),
                    old: new.clone(),
                    new: old.clone(),
                    options: Options::default(),
                    format,
                    sources: None,
                });
            }
        }
        check_tools(&compiler, cases).await;
    });
}
