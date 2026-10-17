//! The sandbox corpus (realistic schemas) and remus's showcase: each built
//! from nothing and dropped to nothing, and each migrated into the others.

use remus_core::{Schema, fixtures::SHOWCASE};
use romulus_migration::{Format, Options};
use romulus_sandbox::{Compiler, render_sql};
use tokio::sync::OnceCell;

use crate::{
    Case, check_all, compile, corpus, empty, smoke,
    tools::{ToolCase, check_tools},
    with_compiler,
};

static MODELS: OnceCell<Vec<(String, Schema)>> = OnceCell::const_new();

/// The corpus files and the showcase, compiled: canonical models.
pub async fn models(compiler: &Compiler) -> &'static [(String, Schema)] {
    MODELS
        .get_or_init(|| async {
            let dir = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../romulus-sandbox/tests/corpus"
            );
            let mut paths: Vec<_> = std::fs::read_dir(dir)
                .expect("the sandbox corpus")
                .map(|entry| entry.expect("a corpus file").path())
                .filter(|p| p.extension().is_some_and(|x| x == "sql"))
                .collect();
            paths.sort();
            let mut models = Vec::new();
            for path in paths {
                let name = path
                    .file_stem()
                    .expect("a file name")
                    .to_string_lossy()
                    .into_owned();
                // Grows its enums and uses the values in one transaction:
                // 17 and later only (romulus-sandbox's `majors` tests).
                if name == "14_enum_evolution" && compiler.major() < 17 {
                    continue;
                }
                let sql = std::fs::read_to_string(&path).expect("a readable corpus file");
                let model = compile(compiler, &sql)
                    .await
                    .unwrap_or_else(|e| panic!("{name} compiles:\n{e}"));
                models.push((name, model));
            }
            let showcase = Schema::from_json(SHOWCASE).expect("the showcase model");
            let model = compile(compiler, &render_sql(&showcase))
                .await
                .unwrap_or_else(|e| panic!("the showcase compiles:\n{e}"));
            models.push(("showcase".to_owned(), model));
            models
        })
        .await
}

#[test]
fn every_corpus_schema_is_built_from_nothing_and_dropped_to_nothing() {
    with_compiler(|compiler| async move {
        let models = models(&compiler).await;
        let nothing = empty();
        let mut cases = Vec::new();
        for (name, model) in models {
            let case = Case::new(format!("nothing → {name}"), &nothing, model);
            cases.push(case.reversed());
            cases.push(case);
        }
        let _skipped = check_all(&compiler, cases).await;
    });
}

#[test]
fn every_corpus_schema_migrates_into_every_other() {
    with_compiler(|compiler| async move {
        let models = models(&compiler).await;
        let mut cases = Vec::new();
        for (i, (a, old)) in models.iter().enumerate() {
            for (j, (b, new)) in models.iter().enumerate() {
                // The smoke subset: each into the next one only.
                let wanted = !smoke(&compiler) || j == (i + 1) % models.len();
                if a != b && wanted {
                    cases.push(Case::new(format!("{a} → {b}"), old, new));
                }
            }
        }
        let _skipped = check_all(&compiler, cases).await;
    });
}

#[test]
fn the_extension_objects_list_matches_the_sandbox() {
    with_compiler(|compiler| async move {
        let mut missing = Vec::new();
        for (name, extension) in compiler.catalog().extensions() {
            for object in &extension.objects {
                let known = romulus_migration::EXTENSION_OBJECTS
                    .iter()
                    .any(|(x, o)| *x == name && *o == object.name);
                if !known {
                    missing.push(format!("(\"{name}\", \"{}\")", object.name));
                }
            }
        }
        for (name, extension) in compiler.catalog().extensions() {
            for required in &extension.requires {
                let known = romulus_migration::EXTENSION_REQUIREMENTS
                    .iter()
                    .any(|(x, r)| *x == name && r == required);
                if !known {
                    missing.push(format!("REQUIRES (\"{name}\", \"{required}\")"));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "add to romulus_migration's extensions::PROVIDED or REQUIRES: {}",
            missing.join(", ")
        );
    });
}

#[test]
fn every_corpus_schema_is_built_and_dropped_by_each_tool() {
    with_compiler(|compiler| async move {
        let models = corpus::models(&compiler).await;
        let nothing = empty();
        let mut cases = Vec::new();
        // Up builds it, down drops it; and one into the next.
        for (i, (name, model)) in models.iter().enumerate() {
            let format = Format::ALL[i % Format::ALL.len()];
            for format in [format, Format::GolangMigrate, Format::Dbmate] {
                cases.push(ToolCase {
                    label: format!("nothing → {name}"),
                    old: nothing.clone(),
                    new: model.clone(),
                    options: Options::default(),
                    format,
                    sources: None,
                });
            }
            let (next, next_model) = &models[(i + 1) % models.len()];
            cases.push(ToolCase {
                label: format!("{name} → {next}"),
                old: model.clone(),
                new: next_model.clone(),
                options: Options {
                    concurrently: i % 2 == 0,
                    ..Options::default()
                },
                format: Format::ALL[(i + 3) % Format::ALL.len()],
                sources: None,
            });
        }
        check_tools(&compiler, cases).await;
    });
}
