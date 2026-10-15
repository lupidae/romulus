//! Many compiles at once: the pool isolates them, per-key coalescing keeps
//! only the newest, and every model belongs to the text it was asked for.

use std::time::{Duration, Instant};

use remus_core::Schema;
use romulus_sandbox::{Compilation, Compiler, Rejected, SandboxConfig, SandboxError};

use crate::{config, explain, with_compiler, with_own_compiler};

/// The same table names in every text (so two compiles in one database
/// would collide), with a column that says which text it was.
fn draft(key: usize, version: usize) -> String {
    format!(
        "CREATE TABLE users (id bigint PRIMARY KEY, k{key}_v{version} int);\n\
         CREATE TABLE orders (id bigint PRIMARY KEY, user_id bigint REFERENCES users (id));\n\
         CREATE INDEX orders_user_idx ON orders (user_id);\n\
         CREATE POLICY own ON orders TO app_user USING (user_id = current_setting('app.user')::bigint);"
    )
}

/// The marker column of a compiled draft.
fn marker(model: &Schema) -> String {
    let users = model
        .entities
        .iter()
        .find(|e| e.name == "users")
        .expect("users");
    users.columns[1].name.clone()
}

#[test]
fn fifty_parallel_compiles_share_the_pool_and_never_mix_results() {
    with_own_compiler(
        |url| config(url, "romulus_test_parallel"),
        |compiler| async move {
            let started = Instant::now();
            let tasks: Vec<_> = (0..50)
                .map(|i| {
                    let compiler = compiler.clone();
                    tokio::spawn(async move {
                        // A distinct version per call: no cache hits.
                        let sql = draft(1000 + i, i);
                        let compilation = compiler.compile(&sql).await.expect("the sandbox runs");
                        (i, sql, compilation)
                    })
                })
                .collect();
            for task in tasks {
                let (i, sql, compilation) = task.await.expect("the task");
                assert!(compilation.is_ok(), "{}", explain(&sql, &compilation));
                let model = compilation.model.unwrap();
                assert_eq!(marker(&model), format!("k{}_v{i}", 1000 + i));
                let orders = model.entities.iter().find(|e| e.name == "orders").unwrap();
                assert_eq!(orders.policies[0].roles, ["app_user"]);
            }
            let elapsed = started.elapsed();
            assert!(elapsed < Duration::from_secs(60), "{elapsed:?}");
            let metrics = compiler.metrics();
            assert_eq!(metrics.pool_free, metrics.pool_size);
            assert_eq!(metrics.sandbox_runs, 50);
            assert_eq!(metrics.succeeded, 50);
        },
    );
}

#[test]
fn fifty_draft_compiles_across_twenty_keys_coalesce_to_the_newest() {
    with_compiler(|compiler| async move {
        // 20 drafts; 10 send three versions at once, 10 send two: 50 requests.
        let tasks: Vec<_> = (0..20)
            .map(|key| {
                let compiler = compiler.clone();
                tokio::spawn(async move {
                    let versions = if key < 10 { 3 } else { 2 };
                    let name = format!("draft-{key}");
                    let mut requests = Vec::new();
                    for version in 0..versions {
                        requests.push(start(&compiler, &name, key, version).await);
                    }
                    let mut results = Vec::new();
                    for request in requests {
                        results.push(request.await.expect("the task"));
                    }
                    (key, results)
                })
            })
            .collect();

        let mut answered = 0;
        let mut superseded = 0;
        for task in tasks {
            let (key, results) = task.await.expect("the task");
            let newest = results.len() - 1;
            for (version, result) in results.into_iter().enumerate() {
                match result {
                    Ok(compilation) => {
                        answered += 1;
                        let model = compilation.model.expect("a model");
                        // A model always matches the text it answers.
                        assert_eq!(marker(&model), format!("k{key}_v{version}"));
                    }
                    Err(Rejected::Superseded) => {
                        superseded += 1;
                        assert_ne!(version, newest, "the newest request of {key} was dropped");
                    }
                    Err(Rejected::Sandbox(err)) => panic!("{err}"),
                }
            }
        }
        assert_eq!(answered + superseded, 50);
        // Every draft's newest version ran.
        assert!(answered >= 20);
    });
}

/// Send a draft's version, and give it a moment to register before the
/// next one, so versions arrive in order.
async fn start(
    compiler: &Compiler,
    key: &str,
    draft_key: usize,
    version: usize,
) -> tokio::task::JoinHandle<Result<Compilation, Rejected>> {
    let compiler = compiler.clone();
    let key = key.to_owned();
    let sql = draft(draft_key, version);
    let handle = tokio::spawn(async move { compiler.compile_for(&key, sql).await });
    tokio::time::sleep(Duration::from_millis(2)).await;
    handle
}

#[test]
fn compiles_of_one_key_are_spaced_by_the_minimum_interval() {
    with_own_compiler(
        |url| SandboxConfig {
            pool_size: 2,
            min_interval: Duration::from_millis(400),
            ..config(url, "romulus_test_rate")
        },
        |compiler| async move {
            let started = Instant::now();
            for version in 0..3 {
                let compilation = compiler
                    .compile_for("rated", draft(7, version))
                    .await
                    .expect("compiled");
                assert!(compilation.is_ok());
            }
            // Three starts, two intervals between them.
            assert!(
                started.elapsed() >= Duration::from_millis(800),
                "{:?}",
                started.elapsed()
            );

            // Another key is not slowed down by this one.
            let other = Instant::now();
            compiler
                .compile_for("other", draft(8, 0))
                .await
                .expect("compiled");
            assert!(other.elapsed() < Duration::from_millis(400));
        },
    );
}

#[test]
fn a_second_process_cannot_share_a_prefix() {
    with_own_compiler(
        |url| SandboxConfig {
            pool_size: 1,
            ..config(url, "romulus_test_exclusive")
        },
        |_first| async move {
            let url = crate::admin_url().expect("the sandbox URL");
            // Advisory locks are per database: the lock lives in one fixed
            // database whatever database the admin URL names.
            let elsewhere = match url.rsplit_once('/') {
                Some((base, _)) => format!("{base}/template1"),
                None => url.clone(),
            };
            for admin_url in [url, elsewhere] {
                let second = Compiler::start(SandboxConfig {
                    pool_size: 1,
                    ..config(admin_url.clone(), "romulus_test_exclusive")
                })
                .await;
                assert!(
                    matches!(second, Err(SandboxError::PrefixInUse(_))),
                    "{admin_url}: {:?}",
                    second.err()
                );
            }
        },
    );
}

async fn admin() -> sqlx::PgConnection {
    use sqlx::Connection;
    let url = crate::admin_url().expect("the sandbox URL");
    sqlx::PgConnection::connect(&url).await.expect("admin")
}

/// Terminate the admin session (the prefix lock's holder) of a compiler.
async fn kill_admin(prefix: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE application_name = $1",
    )
    .bind(format!("romulus-sandbox-admin:{prefix}"))
    .fetch_one(&mut admin().await)
    .await
    .expect("terminate")
}

async fn admin_sessions(prefix: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE application_name = $1")
        .bind(format!("romulus-sandbox-admin:{prefix}"))
        .fetch_one(&mut admin().await)
        .await
        .expect("count")
}

#[test]
fn a_lost_admin_connection_takes_the_prefix_back() {
    with_own_compiler(
        |url| SandboxConfig {
            pool_size: 1,
            admin_check_interval: Duration::from_millis(100),
            ..config(url, "romulus_test_watch")
        },
        |first| async move {
            assert_eq!(kill_admin("romulus_test_watch").await, 1);
            let mut back = false;
            for _ in 0..100 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if admin_sessions("romulus_test_watch").await == 1 {
                    back = true;
                    break;
                }
            }
            assert!(back, "the watchdog never reconnected");
            let url = crate::admin_url().expect("the sandbox URL");
            let second = Compiler::start(SandboxConfig {
                pool_size: 1,
                ..config(url, "romulus_test_watch")
            })
            .await;
            assert!(matches!(second, Err(SandboxError::PrefixInUse(_))));
            assert!(first.check_admin().await.is_ok());
            let compilation = first.compile("CREATE TABLE still_mine (id int)").await;
            assert!(compilation.expect("compiles").is_ok());
        },
    );
}

#[test]
fn a_prefix_taken_while_the_admin_was_away_stops_the_first_compiler() {
    with_own_compiler(
        |url| SandboxConfig {
            pool_size: 1,
            // No check before the second process starts.
            admin_check_interval: Duration::from_secs(3600),
            ..config(url, "romulus_test_taken")
        },
        |first| async move {
            assert_eq!(kill_admin("romulus_test_taken").await, 1);
            let url = crate::admin_url().expect("the sandbox URL");
            let second = Compiler::start(SandboxConfig {
                pool_size: 1,
                ..config(url, "romulus_test_taken")
            })
            .await
            .expect("the prefix was free: the second process takes it");
            assert!(matches!(
                first.check_admin().await,
                Err(SandboxError::PrefixInUse(_))
            ));
            // From then on the first one refuses to compile in databases
            // that are no longer its own, for good.
            let refused = first.compile("CREATE TABLE not_mine (id int)").await;
            assert!(matches!(refused, Err(SandboxError::PrefixInUse(_))));
            let refused = first
                .compile_for("draft", "CREATE TABLE x (id int)".to_owned())
                .await;
            assert!(matches!(
                refused,
                Err(Rejected::Sandbox(SandboxError::PrefixInUse(_)))
            ));
            let compilation = second.compile("CREATE TABLE mine (id int)").await;
            assert!(compilation.expect("compiles").is_ok());
            second.close().await;
        },
    );
}

/// The catalog probe installs extensions as superuser: in a database of its
/// own that no compile role can reach, never in a pool database.
#[test]
fn extensions_are_probed_where_no_compile_role_reaches() {
    with_compiler(|_compiler| async move {
        let mut admin = admin().await;
        let privilege = |role: &'static str, database: &'static str| {
            sqlx::query_scalar::<_, bool>("SELECT has_database_privilege($1, $2, 'CONNECT')")
                .bind(role)
                .bind(database)
        };
        assert!(
            !privilege("romulus_test_0", "romulus_test_probe")
                .fetch_one(&mut admin)
                .await
                .unwrap()
        );
        assert!(
            !privilege("romulus_test_1", "romulus_test_probe")
                .fetch_one(&mut admin)
                .await
                .unwrap()
        );
        assert!(
            !privilege("romulus_test_0", "romulus_test_1")
                .fetch_one(&mut admin)
                .await
                .unwrap()
        );
        assert!(
            privilege("romulus_test_0", "romulus_test_0")
                .fetch_one(&mut admin)
                .await
                .unwrap()
        );
    });
}
