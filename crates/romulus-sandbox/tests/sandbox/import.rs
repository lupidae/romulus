//! Migration imports: replaying migrations one after the other, a model
//! after each.

use std::sync::{Arc, Mutex};

use remus_core::Schema;
use romulus_sandbox::{Code, Compiler, Replay, ReplayEvent};

use crate::with_compiler;

/// Replay `migrations`, models read from `from` on: the replay and the
/// models it sent, by migration index.
pub async fn replay(
    compiler: &Compiler,
    migrations: &[&str],
    from: usize,
) -> (Replay, Vec<(usize, Schema)>) {
    let models = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&models);
    let replay = compiler
        .replay(
            migrations.iter().map(|m| (*m).to_owned()).collect(),
            from,
            move |event| {
                if let ReplayEvent::Replayed { index, model } = event {
                    sink.lock().unwrap().push((index, *model));
                }
            },
        )
        .await
        .expect("the sandbox runs");
    let models = std::mem::take(&mut *models.lock().unwrap());
    (replay, models)
}

pub fn tables(model: &Schema) -> Vec<String> {
    let mut names: Vec<String> = model
        .entities
        .iter()
        .map(|e| format!("{}.{}", e.schema, e.name))
        .collect();
    names.sort();
    names
}

fn columns(model: &Schema, table: &str) -> Vec<String> {
    model
        .entities
        .iter()
        .find(|e| e.name == table)
        .map(|e| e.columns.iter().map(|c| c.name.clone()).collect())
        .unwrap_or_default()
}

#[test]
fn each_migration_gets_the_model_after_it() {
    with_compiler(|compiler| async move {
        let (replay, models) = replay(
            compiler,
            &[
                "CREATE TABLE users (id bigint PRIMARY KEY, email text NOT NULL);",
                "BEGIN;\nALTER TABLE users ADD COLUMN name text;\nINSERT INTO users VALUES (1, 'a@b.c', 'A');\nCOMMIT;",
                "ALTER TABLE users RENAME TO accounts;\nCREATE TABLE posts (id bigint PRIMARY KEY, author bigint REFERENCES accounts);",
            ],
            0,
        )
        .await;
        assert!(replay.failure.is_none(), "{:?}", replay.failure);
        assert_eq!(models.len(), 3);
        assert_eq!(tables(&models[0].1), ["public.users"]);
        assert_eq!(columns(&models[1].1, "users"), ["id", "email", "name"]);
        assert_eq!(tables(&models[2].1), ["public.accounts", "public.posts"]);
        let skipped: Vec<_> = replay.skipped[1].iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(skipped, ["BEGIN", "INSERT", "COMMIT"]);
    });
}

#[test]
fn earlier_migrations_run_without_a_model_and_empty_ones_share_one() {
    with_compiler(|compiler| async move {
        let (replay, models) = replay(
            compiler,
            &[
                "CREATE TABLE a (id int);",
                "CREATE TABLE b (id int);",
                "INSERT INTO a VALUES (1);",
                "CREATE TABLE c (id int);",
            ],
            1,
        )
        .await;
        assert!(replay.failure.is_none(), "{:?}", replay.failure);
        let indexes: Vec<usize> = models.iter().map(|(i, _)| *i).collect();
        assert_eq!(indexes, [1, 2, 3]);
        assert_eq!(tables(&models[0].1), ["public.a", "public.b"]);
        assert_eq!(tables(&models[1].1), ["public.a", "public.b"]);
        assert_eq!(tables(&models[2].1), ["public.a", "public.b", "public.c"]);
    });
}

#[test]
fn a_failure_names_its_migration_and_position_in_it() {
    with_compiler(|compiler| async move {
        let (replay, models) = replay(
            compiler,
            &[
                "CREATE TABLE users (id int);",
                "-- add the column\nALTER TABLE users\n  ADD COLUMN team_id int REFERENCES teams (id);",
                "CREATE TABLE never (id int);",
            ],
            0,
        )
        .await;
        assert!(models.is_empty(), "no model before the replay succeeds");
        let failure = replay.failure.expect("it fails");
        assert_eq!(failure.index, 1);
        let error = &failure.diagnostics[0];
        assert_eq!(error.code, Code::Sql);
        assert_eq!(error.sqlstate.as_deref(), Some("42P01"));
        let span = error.span.expect("located");
        assert_eq!(span.from.line, 3, "{error:?}");
    });
}

#[test]
fn search_path_resets_between_migrations() {
    with_compiler(|compiler| async move {
        let (replay, models) = replay(
            compiler,
            &[
                "CREATE SCHEMA app;\nSET search_path TO app;\nCREATE TABLE inside (id int);",
                "CREATE TABLE outside (id int);",
            ],
            0,
        )
        .await;
        assert!(replay.failure.is_none(), "{:?}", replay.failure);
        assert_eq!(tables(&models[1].1), ["app.inside", "public.outside"]);
    });
}

#[test]
fn functions_and_triggers_replay_without_their_code() {
    with_compiler(|compiler| async move {
        let (replay, models) = replay(
            compiler,
            &[
                "CREATE TABLE posts (id int PRIMARY KEY, title text, updated_at timestamptz);",
                "CREATE FUNCTION touch() RETURNS trigger LANGUAGE plpgsql AS $$\nBEGIN\n  NEW.updated_at := now();\n  RETURN NEW;\nEND\n$$;\nCREATE TRIGGER posts_touch BEFORE UPDATE ON posts FOR EACH ROW WHEN (OLD.* IS DISTINCT FROM NEW.*) EXECUTE FUNCTION touch();",
                "CREATE FUNCTION slugify(t text) RETURNS text IMMUTABLE LANGUAGE sql AS $$ SELECT lower(t) $$;\nCREATE UNIQUE INDEX CONCURRENTLY posts_slug ON posts (slugify(title));\nALTER TABLE posts ADD COLUMN slug text DEFAULT slugify('x');",
                "DROP TRIGGER posts_touch ON posts;\nDROP FUNCTION touch();",
            ],
            0,
        )
        .await;
        assert!(replay.failure.is_none(), "{:?}", replay.failure);
        assert_eq!(models.len(), 4);
        let posts = &models[2].1.entities[0];
        assert!(
            posts.indexes.iter().any(|i| i.name == "posts_slug"),
            "{:?}",
            posts.indexes
        );
        assert!(posts.columns.iter().any(|c| c.name == "slug"));
        // No stub for a function the migrations define.
        assert!(
            replay.stubs.iter().all(|s| s.name != "slugify"),
            "{:?}",
            replay.stubs
        );
    });
}

#[test]
fn a_skipped_do_block_is_named_when_what_it_created_is_missing() {
    with_compiler(|compiler| async move {
        let (replay, _) = replay(
            compiler,
            &[
                "DO $$ BEGIN\n  IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'mood') THEN\n    CREATE TYPE mood AS ENUM ('happy', 'sad');\n  END IF;\nEND $$;",
                "CREATE TABLE people (id int, mood mood);",
            ],
            0,
        )
        .await;
        let failure = replay.failure.expect("the type is missing");
        assert_eq!(failure.index, 1);
        let hint = failure.diagnostics[0].hint.as_deref().unwrap_or_default();
        assert!(hint.contains("DO"), "{hint}");
    });
}

#[test]
fn a_repeated_create_extension_if_not_exists_is_not_run_again() {
    with_compiler(|compiler| async move {
        let (replay, models) = replay(
            compiler,
            &[
                "CREATE EXTENSION IF NOT EXISTS pgcrypto;\nCREATE TABLE t (id uuid DEFAULT gen_random_uuid());",
                "CREATE EXTENSION IF NOT EXISTS pgcrypto;\nCREATE EXTENSION IF NOT EXISTS pg_stat_statements;\nCREATE TABLE u (id int);",
            ],
            0,
        )
        .await;
        assert!(replay.failure.is_none(), "{:?}", replay.failure);
        assert_eq!(tables(&models[1].1), ["public.t", "public.u"]);
        assert_eq!(replay.skipped[1].len(), 1, "{:?}", replay.skipped[1]);
    });
}
