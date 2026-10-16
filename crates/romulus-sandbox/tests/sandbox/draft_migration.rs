//! A draft written as a migration: its base version's SQL, then the
//! migration, through the drafts' compile (`romulus_sandbox::draft_migration`).

use std::time::{Duration, Instant};

use romulus_sandbox::{Code, Diagnostic, draft_migration::script};

use crate::with_compiler;

const BASE: &str = "CREATE TABLE users (id bigint PRIMARY KEY, email text NOT NULL);\n\
                    COMMENT ON TABLE users IS 'People';\n";

/// Compiles `migration` on `base` as a draft does: the model and the
/// diagnostics located in the migration.
async fn compile(
    compiler: &romulus_sandbox::Compiler,
    base: &str,
    migration: &str,
) -> (Option<remus_core::Schema>, Vec<Diagnostic>) {
    let script = script(base, migration);
    let compilation = compiler
        .compile(&script.text)
        .await
        .expect("the sandbox runs");
    let diagnostics = script.relocate(migration, compilation.diagnostics);
    (compilation.model, diagnostics)
}

#[test]
fn a_migration_with_a_backfill_and_an_index_built_concurrently_compiles() {
    with_compiler(|compiler| async move {
        let migration = "BEGIN;\n\
                         ALTER TABLE users ADD COLUMN slug text;\n\
                         UPDATE users SET slug = lower(email);\n\
                         ALTER TABLE users ALTER COLUMN slug SET NOT NULL;\n\
                         COMMIT;\n\
                         CREATE INDEX CONCURRENTLY users_slug_idx ON users (slug);\n";
        let (model, diagnostics) = compile(compiler, BASE, migration).await;
        let model = model.unwrap_or_else(|| panic!("{diagnostics:#?}"));
        let users = &model.entities[0];
        let slug = users
            .columns
            .iter()
            .find(|c| c.name == "slug")
            .expect("slug");
        assert!(slug.not_null);
        assert!(users.indexes.iter().any(|i| i.name == "users_slug_idx"));
        assert_eq!(users.comment.as_deref(), Some("People"));
        let skipped: Vec<&str> = diagnostics
            .iter()
            .filter(|d| d.code == Code::NotRun)
            .map(|d| &migration[d.span.unwrap().range()])
            .collect();
        assert_eq!(
            skipped,
            ["BEGIN", "UPDATE users SET slug = lower(email)", "COMMIT"]
        );
    });
}

#[test]
fn the_migration_starts_with_the_default_search_path() {
    with_compiler(|compiler| async move {
        let base = "CREATE SCHEMA app;\nSET search_path = app;\nCREATE TABLE things (id int);\n";
        let (model, diagnostics) = compile(compiler, base, "CREATE TABLE more (id int);").await;
        let model = model.unwrap_or_else(|| panic!("{diagnostics:#?}"));
        let mut names: Vec<String> = model
            .entities
            .iter()
            .map(|e| format!("{}.{}", e.schema, e.name))
            .collect();
        names.sort();
        assert_eq!(names, ["app.things", "public.more"]);
    });
}

#[test]
fn postgres_errors_are_located_in_the_migration() {
    with_compiler(|compiler| async move {
        let migration = "-- é, then a mistake\nALTER TABLE users ADD COLUMN plan text;\nALTER TABLE nope ADD COLUMN x int;\n";
        let (model, diagnostics) = compile(compiler, BASE, migration).await;
        assert!(model.is_none());
        let error = diagnostics.iter().find(|d| d.is_error()).expect("an error");
        assert_eq!(error.code, Code::Sql);
        assert!(error.message.contains("\"nope\""), "{error:?}");
        let span = error.span.expect("located");
        assert_eq!(span.from.line, 3, "{error:?}");
        assert!(migration[span.range()].contains("nope"), "{error:?}");
        assert!(
            error
                .detail
                .as_deref()
                .is_none_or(|d| !d.starts_with("In the base"))
        );
    });
}

/// The drafts' allowlist, located in the migration: nothing of these runs.
#[test]
fn hostile_migrations_are_refused_like_drafts() {
    with_compiler(|compiler| async move {
        for hostile in [
            "DO $$ BEGIN PERFORM pg_sleep(30); END $$;",
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'SELECT 1';",
            "CREATE TRIGGER t BEFORE INSERT ON users EXECUTE FUNCTION f();",
            "COPY users FROM PROGRAM 'id';",
            "GRANT ALL ON users TO PUBLIC;",
            "ALTER SYSTEM SET work_mem = '1TB';",
            "SET statement_timeout = 0;",
            "SET ROLE sandbox_admin;",
            "ROLLBACK;",
            "CREATE TEMP TABLE t (id int);",
            "CREATE TABLE t AS SELECT pg_sleep(30);",
            "ALTER TABLE users ADD COLUMN x text DEFAULT repeat('x', 500000000);",
            "ALTER TABLE users ADD CONSTRAINT c CHECK (pg_advisory_lock(1) IS NULL);",
            "CREATE EXTENSION dblink;",
            "LOCK TABLE users IN ACCESS EXCLUSIVE MODE;",
        ] {
            let started = Instant::now();
            let (model, diagnostics) = compile(compiler, BASE, hostile).await;
            assert!(model.is_none(), "{hostile} compiled");
            let error = diagnostics.iter().find(|d| d.is_error()).expect("an error");
            assert!(
                matches!(
                    error.code,
                    Code::NotAllowed | Code::FunctionNotAllowed | Code::ExtensionNotAvailable
                ),
                "{hostile}: {error:?}"
            );
            // Located in the migration, never in the base.
            let span = error.span.expect("located");
            assert!(span.end <= hostile.len(), "{hostile}: {error:?}");
            assert!(started.elapsed() < Duration::from_secs(10), "{hostile} ran");
        }
    });
}

/// Data statements are not run at all: not even a sleep.
#[test]
fn data_statements_never_run() {
    with_compiler(|compiler| async move {
        let migration = "SELECT pg_sleep(30);\n\
                         INSERT INTO users SELECT g, 'x' FROM generate_series(1, 100000000) g;\n\
                         DELETE FROM users;\n\
                         SELECT pg_terminate_backend(pg_backend_pid());\n\
                         ALTER TABLE users ADD COLUMN plan text;\n";
        let started = Instant::now();
        let (model, diagnostics) = compile(compiler, BASE, migration).await;
        assert!(started.elapsed() < Duration::from_secs(10));
        let model = model.unwrap_or_else(|| panic!("{diagnostics:#?}"));
        assert!(model.entities[0].columns.iter().any(|c| c.name == "plan"));
        assert_eq!(
            diagnostics
                .iter()
                .filter(|d| d.code == Code::NotRun)
                .count(),
            4
        );
    });
}

/// What a version keeps when canonical SQL would lose part of its schema
/// (a `NOT VALID` check comes back validated from remus's SQL): the base,
/// then what of the migration runs, which compiles to the same model.
#[test]
fn the_faithful_sql_compiles_to_what_the_migration_made() {
    with_compiler(|compiler| async move {
        let migration = "BEGIN;\n\
                         ALTER TABLE users ADD COLUMN age int;\n\
                         UPDATE users SET age = 1;\n\
                         ALTER TABLE users ADD CONSTRAINT age_positive CHECK (age > 0) NOT VALID;\n\
                         CREATE INDEX CONCURRENTLY users_age_idx ON users (age);\n\
                         COMMIT;\n";
        let (model, diagnostics) = compile(compiler, BASE, migration).await;
        let model = model.unwrap_or_else(|| panic!("{diagnostics:#?}"));
        let faithful = romulus_sandbox::draft_migration::faithful_sql(BASE, migration);
        let again = compiler.compile(&faithful).await.expect("the sandbox runs");
        assert_eq!(
            again.model.as_ref(),
            Some(&model),
            "{faithful}\n{:#?}",
            again.diagnostics
        );
        assert!(
            !faithful.contains("UPDATE") && !faithful.contains("CONCURRENTLY"),
            "{faithful}"
        );
    });
}
