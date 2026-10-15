//! Hostile input: refused before the sandbox sees it, or contained by it.

use std::time::{Duration, Instant};

use romulus_sandbox::{Code, Compilation, SandboxConfig};

use crate::{config, explain, with_compiler, with_own_compiler};

fn codes(compilation: &Compilation) -> Vec<Code> {
    compilation.errors().map(|d| d.code).collect()
}

/// Each statement is refused by the allowlist, with nothing run.
#[test]
fn statements_outside_the_allowlist_never_reach_the_sandbox() {
    with_compiler(|compiler| async move {
        for sql in [
            "DO $$ BEGIN PERFORM pg_sleep(30); END $$",
            "COPY (SELECT 1) TO PROGRAM 'id'",
            "COPY users FROM '/etc/passwd'",
            "INSERT INTO users VALUES (1)",
            "CREATE TABLE t (id int); INSERT INTO t SELECT generate_series(1, 100000000)",
            "SELECT pg_sleep(30)",
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'SELECT 1'",
            "CREATE TABLE t (id int); CREATE TRIGGER x BEFORE INSERT ON t EXECUTE FUNCTION f()",
            "ALTER SYSTEM SET shared_buffers = '8GB'",
            "CREATE ROLE intruder SUPERUSER",
            "SET ROLE sandbox_admin",
            "SET statement_timeout = 0",
            "SET SESSION AUTHORIZATION sandbox_admin",
            "BEGIN; CREATE TABLE t (id int); COMMIT",
            "LOCK TABLE pg_class IN ACCESS EXCLUSIVE MODE",
            "CREATE EXTENSION dblink",
            "CREATE EXTENSION postgres_fdw",
            "LISTEN romulus; NOTIFY romulus",
            "VACUUM FULL",
            "CREATE DATABASE escape",
            "CREATE TABLESPACE t LOCATION '/tmp'",
            "CREATE TEMP TABLE t (id int)",
            "CREATE TABLE t (id int DEFAULT pg_terminate_backend(pg_backend_pid())::int)",
            "CREATE TABLE t (id int) PARTITION BY LIST (id); CREATE TABLE t1 PARTITION OF t FOR VALUES IN (pg_cancel_backend(1)::int)",
            "CREATE TABLE t (x xml DEFAULT query_to_xml('DELETE FROM users', true, false, ''))",
            "ALTER TABLE t ADD COLUMN x text DEFAULT repeat('x', 500000000)",
            "CREATE INDEX ON t ((lpad('', 400000000, 'x')))",
            "CREATE TABLE t (id int CHECK (pg_advisory_lock(1) IS NULL))",
            "CREATE TABLE t (x oid DEFAULT lo_import('/etc/passwd'))",
            "CREATE VIEW v AS SELECT pg_read_file('/etc/passwd')",
            "DROP SEQUENCE pg_temp.romulus_sandbox_progress",
            "CREATE TABLE romulus_sandbox_policy_roles (id int)",
        ] {
            let compilation = compiler.compile(sql).await.expect("the sandbox runs");
            assert!(compilation.model.is_none(), "{sql} compiled");
            // Allowlist errors come before the sandbox: none of these ran.
            assert_eq!(compilation.stubs, []);
            let codes = codes(&compilation);
            assert!(
                codes.iter().all(|c| matches!(
                    c,
                    Code::NotAllowed | Code::FunctionNotAllowed | Code::ExtensionNotAvailable
                )),
                "{sql}:\n{}",
                explain(sql, &compilation)
            );
            assert!(
                compilation
                    .errors()
                    .all(|d| d.span.is_some() && d.hint.is_some())
            );
        }
    });
}

/// What the allowlist lets through still cannot do anything outside the
/// compile: the role has no privileges beyond its own database.
#[test]
fn allowed_statements_cannot_reach_beyond_their_database() {
    with_compiler(|compiler| async move {
        for (sql, sqlstate) in [
            // The server's own objects.
            ("COMMENT ON EXTENSION plpgsql IS 'mine'", "42501"),
            ("CREATE SCHEMA pg_mine", "42939"),
            ("ALTER TABLE pg_catalog.pg_class ADD COLUMN x int", "42501"),
            ("CREATE TABLE pg_catalog.mine (id int)", "42501"),
            ("CREATE INDEX ON pg_catalog.pg_class (relname)", "42501"),
            (
                "DROP SCHEMA public CASCADE; CREATE TABLE t (id int)",
                "3F000",
            ),
        ] {
            let compilation = compiler.compile(sql).await.expect("the sandbox runs");
            assert!(compilation.model.is_none(), "{sql} compiled");
            let error = compilation.errors().next().expect("an error");
            assert_eq!(
                error.code,
                Code::Sql,
                "{sql}: {}",
                explain(sql, &compilation)
            );
            assert_eq!(
                error.sqlstate.as_deref(),
                Some(sqlstate),
                "{sql}: {}",
                explain(sql, &compilation)
            );
        }
        // And nothing stuck: a normal compile still works on every slot.
        for _ in 0..8 {
            let sql = format!("CREATE TABLE after_{} (id int)", rand_suffix());
            let compilation = compiler.compile(&sql).await.expect("the sandbox runs");
            assert!(compilation.is_ok(), "{}", explain(&sql, &compilation));
        }
    });
}

/// Materialized views are created WITH NO DATA: their query never runs.
#[test]
fn a_materialized_view_query_never_runs() {
    with_compiler(|compiler| async move {
        let sql = "CREATE MATERIALIZED VIEW forever AS \
                   SELECT count(*) FROM generate_series(1, 10000000000) a, generate_series(1, 10000000000) b";
        let started = Instant::now();
        let compilation = compiler.compile(sql).await.expect("the sandbox runs");
        assert!(compilation.is_ok(), "{}", explain(sql, &compilation));
        assert!(started.elapsed() < Duration::from_secs(5));
    });
}

#[test]
fn text_over_the_size_limit_is_refused_before_parsing() {
    with_compiler(|compiler| async move {
        let limit = compiler.config().max_sql_bytes;
        let sql = format!(
            "CREATE TABLE big (id int);\nCOMMENT ON TABLE big IS '{}';",
            "x".repeat(limit)
        );
        let compilation = compiler.compile(&sql).await.expect("the sandbox runs");
        assert_eq!(codes(&compilation), [Code::TooLarge]);
        assert!(compilation.diagnostics[0].span.is_none());
    });
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    with_compiler(|compiler| async move {
        for depth in [200, 5_000, 50_000] {
            // Each level an operator node: a tree, not just parentheses.
            let sql = format!(
                "CREATE TABLE t (x int DEFAULT {}1{})",
                "(1 + ".repeat(depth),
                ")".repeat(depth)
            );
            let compilation = compiler.compile(&sql).await.expect("the sandbox runs");
            assert!(compilation.model.is_none(), "depth {depth} compiled");
            let error = compilation.errors().next().expect("an error");
            assert_eq!(error.code, Code::SyntaxError, "{}", error.message);
        }
        let sql = "CREATE TABLE after_nesting (id int)";
        assert!(
            compiler
                .compile(sql)
                .await
                .expect("the sandbox runs")
                .is_ok()
        );
    });
}

/// `statement_timeout` stops a slow statement, and the error says which.
#[test]
fn a_slow_statement_hits_the_statement_timeout() {
    with_own_compiler(
        |url| SandboxConfig {
            pool_size: 1,
            statement_timeout: Duration::from_millis(100),
            ..config(url, "romulus_test_timeout")
        },
        |compiler| async move {
            // Installing PostGIS and its topology takes far longer than
            // 100 ms; a CREATE TABLE far less.
            let sql = "CREATE TABLE a (id int);\nCREATE EXTENSION postgis_topology CASCADE;\nCREATE TABLE b (id int);";
            let compilation = compiler.compile(sql).await.expect("the sandbox runs");
            let error = compilation.errors().next().expect("a timeout");
            assert_eq!(error.code, Code::Timeout, "{}", explain(sql, &compilation));
            assert_eq!(error.sqlstate.as_deref(), Some("57014"));
            assert_eq!(error.statement, Some(1));
            assert_eq!(error.span.unwrap().from.line, 2);
            assert_eq!(compiler.metrics().timeouts, 1);
        },
    );
}

/// Past the compile deadline the backend is terminated, the slot
/// reconnects, and the next compile works.
#[test]
fn the_compile_deadline_terminates_the_backend_and_the_slot_recovers() {
    with_own_compiler(
        |url| SandboxConfig {
            pool_size: 1,
            compile_timeout: Duration::from_secs(1),
            ..config(url, "romulus_test_deadline")
        },
        |compiler| async move {
            // Thousands of statements: each under statement_timeout,
            // together far over the deadline.
            let sql = (0..2_000).fold(String::new(), |mut sql, i| {
                use std::fmt::Write;
                let _ = writeln!(sql, "CREATE TABLE t{i} (id int PRIMARY KEY, label text);");
                sql
            });
            let started = Instant::now();
            let compilation = compiler.compile(&sql).await.expect("the sandbox runs");
            assert_eq!(
                codes(&compilation),
                [Code::Timeout],
                "{}",
                explain(&sql, &compilation)
            );
            assert!(
                // Parsing and checking 2 000 statements (a debug build) comes
                // first; then the 1 s deadline and the 2 s grace to stop.
                started.elapsed() < Duration::from_secs(15),
                "{:?}",
                started.elapsed()
            );
            assert_eq!(
                compilation.diagnostics[0].message,
                "the compile took longer than 1 s and was stopped"
            );

            let compilation = compiler
                .compile("CREATE TABLE recovered (id int)")
                .await
                .expect("the slot reconnects");
            assert!(compilation.is_ok(), "{}", explain("", &compilation));
            // Timeouts are not cached: the same text runs again.
            let again = compiler.compile(&sql).await.expect("the sandbox runs");
            assert!(!again.cached);
        },
    );
}

/// Unique enough for table names across runs of one test.
fn rand_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

/// Defence in depth, below the allowlist: every sandbox process may
/// allocate at most 128 MB of private memory (docker-compose.yml and CI), so
/// a value too large fails as an ordinary error, the server stays up, and
/// compiles go on. Even a superuser query cannot get past it.
#[test]
fn the_server_caps_each_backends_memory() {
    with_compiler(|compiler| async move {
        use sqlx::Connection;
        let url = crate::admin_url().expect("the sandbox URL");
        let mut admin = sqlx::PgConnection::connect(&url).await.expect("admin");
        let error = sqlx::query("SELECT length(repeat('x', 300000000))")
            .execute(&mut admin)
            .await
            .expect_err("a 300 MB value");
        assert!(error.to_string().contains("out of memory"), "{error}");
        let still: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&mut admin)
            .await
            .expect("up");
        assert_eq!(still, 1);
        let sql = "CREATE TABLE after_the_bomb (id int)";
        assert!(
            compiler
                .compile(sql)
                .await
                .expect("the sandbox runs")
                .is_ok()
        );
    });
}

/// A backend killed in the middle of a compile (by an operator, or by the
/// OOM killer, which spares the postmaster) costs a retry on a fresh
/// connection, not the compile; the pool carries on.
#[test]
fn a_backend_killed_mid_compile_is_retried_and_the_pool_recovers() {
    with_own_compiler(
        |url| SandboxConfig {
            pool_size: 1,
            ..config(url, "romulus_test_kill")
        },
        |compiler| async move {
            use sqlx::Connection;
            let sql = "CREATE EXTENSION postgis;\nCREATE EXTENSION postgis_topology;\n\
                       CREATE TABLE survived (id int);"
                .to_owned();
            let running = {
                let compiler = compiler.clone();
                let sql = sql.clone();
                tokio::spawn(async move { compiler.compile(&sql).await })
            };
            let url = crate::admin_url().expect("the sandbox URL");
            let mut admin = sqlx::PgConnection::connect(&url).await.expect("admin");
            let mut killed = false;
            for _ in 0..500 {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let n: i64 = sqlx::query_scalar(
                    "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
                     WHERE usename = 'romulus_test_kill_0' AND state = 'active'",
                )
                .fetch_one(&mut admin)
                .await
                .expect("terminate");
                if n > 0 {
                    killed = true;
                    break;
                }
            }
            assert!(killed, "the compile never showed up as active");
            let compilation = running
                .await
                .expect("the task")
                .expect("retried, not unavailable");
            assert!(compilation.is_ok(), "{}", explain(&sql, &compilation));
            let after = compiler
                .compile("CREATE TABLE next_one (id int)")
                .await
                .expect("the pool recovered");
            assert!(after.is_ok());
        },
    );
}
