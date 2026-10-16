//! The sandbox corpus (realistic schemas) and remus's showcase: each built
//! from nothing and dropped to nothing, and each migrated into the others.

use remus_core::{Schema, fixtures::SHOWCASE};
use romulus_sandbox::{Compiler, render_sql};
use tokio::sync::OnceCell;

use crate::{Case, check_all, compile, empty, with_compiler};

static MODELS: OnceCell<Vec<(String, Schema)>> = OnceCell::const_new();

/// The corpus files and the showcase, compiled: canonical models.
pub async fn models(compiler: &Compiler) -> &'static [(String, Schema)] {
    MODELS
        .get_or_init(|| async {
            let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../romulus-sandbox/tests/corpus");
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
        check_all(&compiler, cases).await;
    });
}

#[test]
fn every_corpus_schema_migrates_into_every_other() {
    with_compiler(|compiler| async move {
        let models = models(&compiler).await;
        let mut cases = Vec::new();
        for (a, old) in models {
            for (b, new) in models {
                if a != b {
                    cases.push(Case::new(format!("{a} → {b}"), old, new));
                }
            }
        }
        check_all(&compiler, cases).await;
    });
}
