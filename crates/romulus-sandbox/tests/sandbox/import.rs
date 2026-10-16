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

/// Hostile migrations: whatever they define, none of their own code runs
/// in the sandbox, and what the compile allowlist refuses outside
/// functions is still refused.
mod hostile {
    use std::time::{Duration, Instant};

    use romulus_sandbox::Code;

    use super::replay;
    use crate::with_compiler;

    /// Postgres evaluates constant calls of immutable and stable functions
    /// while it creates defaults, checks and indexes: with the real body,
    /// each of these raises (or sleeps). The replay gives every function a
    /// fixed body, so they all succeed, at once.
    #[test]
    fn function_bodies_never_run_even_where_postgres_folds_calls() {
        with_compiler(|compiler| async move {
            let started = Instant::now();
            let (replay, models) = replay(
                compiler,
                &[
                    "CREATE FUNCTION boom() RETURNS int IMMUTABLE LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'user code ran'; END $$;\n\
                     CREATE FUNCTION nap() RETURNS int STABLE LANGUAGE sql AS $$ SELECT pg_sleep(30)::text::int $$;\n\
                     CREATE FUNCTION loop() RETURNS int IMMUTABLE LANGUAGE plpgsql AS $$ BEGIN LOOP END LOOP; END $$;\n\
                     CREATE FUNCTION grow() RETURNS text IMMUTABLE LANGUAGE sql BEGIN ATOMIC SELECT repeat('x', 500000000); END;\n\
                     CREATE FUNCTION native() RETURNS int IMMUTABLE LANGUAGE c AS '$libdir/plpgsql', 'plpgsql_call_handler';\n\
                     CREATE FUNCTION py() RETURNS int IMMUTABLE LANGUAGE plpython3u AS 'import os; os.system(\"id\")';",
                    "CREATE TABLE t (x int);\n\
                     ALTER TABLE t ADD COLUMN a int DEFAULT boom();\n\
                     ALTER TABLE t ADD COLUMN b int DEFAULT nap();\n\
                     ALTER TABLE t ADD COLUMN c int DEFAULT loop();\n\
                     ALTER TABLE t ADD COLUMN d text DEFAULT grow();\n\
                     ALTER TABLE t ADD COLUMN e int DEFAULT native() + py();\n\
                     ALTER TABLE t ADD CONSTRAINT ok CHECK (x > boom());\n\
                     CREATE INDEX t_folded ON t ((x + boom() + loop()));\n\
                     CREATE INDEX t_partial ON t (x) WHERE grow() IS NOT NULL;",
                ],
                0,
            )
            .await;
            assert!(replay.failure.is_none(), "{:?}", replay.failure);
            assert_eq!(models.len(), 2);
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "took {:?}",
                started.elapsed()
            );
        });
    }

    /// Triggers keep firing nothing (no rows are ever written), and their
    /// conditions and rules' actions are dropped before they reach the
    /// sandbox: denied calls there no longer matter.
    #[test]
    fn trigger_conditions_rules_and_queries_never_run() {
        with_compiler(|compiler| async move {
            let started = Instant::now();
            let (replay, _) = replay(
                compiler,
                &[
                    "CREATE TABLE t (x int);\n\
                     CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;\n\
                     CREATE TRIGGER slow BEFORE INSERT ON t FOR EACH ROW WHEN (pg_sleep(30) IS NULL) EXECUTE FUNCTION f();\n\
                     CREATE RULE r AS ON INSERT TO t DO ALSO SELECT pg_sleep(30), pg_terminate_backend(1);\n\
                     CREATE TABLE copy AS SELECT repeat('x', 500000000) AS big, generate_series(1, 100000000) AS n;\n\
                     SELECT generate_series(1, 100000000) AS n INTO numbers;\n\
                     INSERT INTO t SELECT generate_series(1, 100000000);\n\
                     DO $$ BEGIN PERFORM pg_sleep(30); END $$;\n\
                     WITH gone AS (DELETE FROM t RETURNING *) SELECT count(*) FROM gone;\n\
                     COPY t FROM PROGRAM 'id';\n\
                     CALL nothing();",
                ],
                0,
            )
            .await;
            assert!(replay.failure.is_none(), "{:?}", replay.failure);
            assert!(started.elapsed() < Duration::from_secs(10));
            let kinds: Vec<&str> = replay.skipped[0].iter().map(|s| s.kind.as_str()).collect();
            assert_eq!(kinds, ["INSERT", "DO", "WITH", "COPY", "CALL"]);
        });
    }

    /// What the compile allowlist refuses outside function bodies is still
    /// refused in an import (before anything runs), and the server-wide
    /// statements are skipped, never run.
    #[test]
    fn the_allowlist_still_holds_outside_function_bodies() {
        with_compiler(|compiler| async move {
            for sql in [
                // Arguments' defaults are evaluated by callers: still checked.
                "CREATE FUNCTION f(x text DEFAULT repeat('x', 500000000)) RETURNS text LANGUAGE sql AS 'SELECT x'",
                "CREATE TABLE t (x int DEFAULT pg_terminate_backend(1)::int)",
                "CREATE TABLE t (x text); CREATE INDEX ON t ((lpad('', 400000000, 'x')))",
                "CREATE TABLE copy AS SELECT pg_sleep(30)",
                "CREATE FUNCTION pg_temp.f() RETURNS int LANGUAGE sql AS 'SELECT 1'",
                "CREATE TEMP TABLE t (id int)",
                "CREATE EXTENSION dblink; CREATE EXTENSION IF NOT EXISTS dblink; CREATE TABLE t (x int DEFAULT dblink_exec('x')::int)",
            ] {
                let (replay, _) = replay(compiler, &[sql], 0).await;
                let failure = replay.failure.unwrap_or_else(|| panic!("{sql} replayed"));
                assert!(
                    failure.diagnostics.iter().all(|d| matches!(
                        d.code,
                        Code::NotAllowed | Code::FunctionNotAllowed | Code::Sql
                    )),
                    "{sql}: {:?}",
                    failure.diagnostics
                );
            }
            let (replay, models) = replay(
                compiler,
                &["CREATE ROLE intruder SUPERUSER;\n\
                   ALTER SYSTEM SET shared_buffers = '8GB';\n\
                   SET ROLE sandbox_admin;\n\
                   SET SESSION AUTHORIZATION sandbox_admin;\n\
                   RESET ALL;\n\
                   SET statement_timeout = 0;\n\
                   CREATE EVENT TRIGGER e ON ddl_command_start EXECUTE FUNCTION f();\n\
                   CREATE LANGUAGE plperlu;\n\
                   CREATE CAST (int AS text) WITH FUNCTION f(int) AS IMPLICIT;\n\
                   LOCK TABLE pg_class IN ACCESS EXCLUSIVE MODE;\n\
                   LISTEN x; NOTIFY x;\n\
                   VACUUM FULL;\n\
                   CREATE DATABASE escape;\n\
                   CREATE TABLESPACE t LOCATION '/tmp';\n\
                   GRANT ALL ON SCHEMA public TO PUBLIC;\n\
                   CREATE TABLE kept (id int);"],
                0,
            )
            .await;
            assert!(replay.failure.is_none(), "{:?}", replay.failure);
            assert_eq!(super::tables(&models[0].1), ["public.kept"]);
            assert_eq!(replay.skipped[0].len(), 16, "{:?}", replay.skipped[0]);
        });
    }

    /// A function the import defines may share a name with one an
    /// extension's install script calls (it runs as the bootstrap
    /// superuser): its body is a stub, so capturing the call runs nothing
    /// of the import's.
    #[test]
    fn functions_shadowing_extension_calls_run_nothing() {
        with_compiler(|compiler| async move {
            let started = Instant::now();
            let (replay, _) = replay(
                compiler,
                &[
                    "CREATE FUNCTION public.format(text, VARIADIC text[]) RETURNS text LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(30); RETURN ''; END $$;\n\
                     CREATE FUNCTION public.lower(anyelement) RETURNS text LANGUAGE plpgsql AS $$ BEGIN ALTER ROLE CURRENT_USER SUPERUSER; RETURN ''; END $$;\n\
                     CREATE EXTENSION IF NOT EXISTS pgcrypto;\n\
                     CREATE EXTENSION IF NOT EXISTS citext;\n\
                     CREATE TABLE t (id uuid DEFAULT gen_random_uuid(), name citext);",
                ],
                0,
            )
            .await;
            assert!(started.elapsed() < Duration::from_secs(10));
            assert!(replay.failure.is_none(), "{:?}", replay.failure);
        });
    }
}
