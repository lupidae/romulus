//! The oracle: generated rows load with no surprise. Each schema is
//! compiled in the sandbox, built on a scratch database of the sandbox
//! cluster (as its superuser, with the roles its policies name and stubs
//! for the functions it calls without defining them), handed to an owner
//! role that is not a superuser, as a playground's schema is, and the plan
//! runs there as that owner, step by step: every step must succeed, every
//! table must hold the rows the report says were inserted, and every row
//! planned but missing must be one the report says was dropped.
//!
//! The schemas: the sandbox corpus and remus's showcase in both profiles
//! (`corpus.rs`), and hostile scenarios (`scenarios.rs`).
//!
//! Needs `ROMULUS_SANDBOX_URL` (`.env`, `just sandbox`): skipped, loudly,
//! without it on a laptop, and failed without it in CI (`CI` set), like the
//! migration oracle.

mod corpus;
mod medium;
mod scenarios;

use std::{
    collections::BTreeSet,
    fmt::Write as _,
    future::Future,
    sync::{
        LazyLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use remus_core::{Schema, fixtures::SHOWCASE};
use romulus_fakedata::{Options, Report, Step, copy_text, plan};
use romulus_sandbox::{Compilation, Compiler, SandboxConfig, render_sql};
use sqlx::{AssertSqlSafe, Connection, PgConnection, Row, raw_sql};
use tokio::{runtime::Runtime, sync::OnceCell};

static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a tokio runtime")
});

static COMPILER: OnceCell<Compiler> = OnceCell::const_new();
static SCRATCH: AtomicUsize = AtomicUsize::new(0);

fn admin_url() -> Option<String> {
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    std::env::var("ROMULUS_SANDBOX_URL")
        .ok()
        .filter(|url| !url.is_empty())
}

/// Runs a test body with the sandbox, or skips without one.
pub fn with_sandbox<F, Fut>(body: F)
where
    F: FnOnce(Compiler, String) -> Fut,
    Fut: Future<Output = ()>,
{
    let Some(url) = admin_url() else {
        assert!(
            std::env::var_os("CI").is_none(),
            "ROMULUS_SANDBOX_URL is not set in CI: the fake data oracle cannot run"
        );
        #[allow(clippy::print_stderr)]
        {
            eprintln!("SKIPPED: ROMULUS_SANDBOX_URL is not set, the fake data oracle did not run");
        }
        return;
    };
    RUNTIME.block_on(async {
        let compiler = COMPILER
            .get_or_init(|| async {
                Compiler::start(SandboxConfig {
                    pool_size: 2,
                    upload_concurrency: Some(2),
                    prefix: "romulus_test_fakedata".to_owned(),
                    statement_timeout: Duration::from_secs(15),
                    compile_timeout: Duration::from_secs(60),
                    ..SandboxConfig::new(url.clone())
                })
                .await
                .expect("the sandbox compiler starts (is sandbox-postgres up?)")
            })
            .await
            .clone();
        body(compiler, url).await;
    });
}

/// The corpus files and the showcase, as SQL.
pub fn corpus() -> Vec<(String, String)> {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../romulus-sandbox/tests/corpus"
    );
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .expect("the sandbox corpus")
        .map(|e| e.expect("a corpus file").path())
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .collect();
    paths.sort();
    let mut out: Vec<(String, String)> = paths
        .iter()
        .map(|p| {
            (
                p.file_stem()
                    .expect("a file name")
                    .to_string_lossy()
                    .into_owned(),
                std::fs::read_to_string(p).expect("a readable corpus file"),
            )
        })
        .collect();
    let showcase = Schema::from_json(SHOWCASE).expect("the showcase model");
    out.push(("showcase".to_owned(), render_sql(&showcase)));
    out
}

/// Functions the schema calls without defining them, as a playground has
/// them: one stub each, returning NULL.
fn stubs(compilation: &Compilation) -> String {
    let mut sql = String::new();
    let mut schemas: BTreeSet<&str> = compilation
        .created_schemas
        .iter()
        .map(String::as_str)
        .collect();
    for stub in &compilation.stubs {
        schemas.insert(&stub.schema);
    }
    for schema in schemas {
        let _ = writeln!(sql, "CREATE SCHEMA IF NOT EXISTS \"{schema}\";");
    }
    for stub in &compilation.stubs {
        let body = if stub.returns().ends_with("void") {
            "BEGIN END"
        } else {
            "BEGIN RETURN NULL; END"
        };
        let _ = writeln!(
            sql,
            "CREATE FUNCTION \"{}\".\"{}\"({}) RETURNS {} LANGUAGE plpgsql IMMUTABLE AS $f$ {body} $f$;",
            stub.schema,
            stub.name,
            stub.args().collect::<Vec<_>>().join(", "),
            stub.returns()
        );
    }
    sql
}

async fn create_roles(admin: &str, model: &Schema) -> Result<(), String> {
    let roles: BTreeSet<&String> = model
        .entities
        .iter()
        .flat_map(|e| &e.policies)
        .flat_map(|p| &p.roles)
        .filter(|r| *r != "public")
        .collect();
    let mut conn = PgConnection::connect(admin)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    for role in roles {
        // Cluster-wide: another test may create it meanwhile.
        let _ = raw_sql(AssertSqlSafe(format!(
            "DO $$ BEGIN CREATE ROLE \"{}\"; EXCEPTION WHEN duplicate_object THEN NULL; END $$",
            role.replace('"', "\"\"")
        )))
        .execute(&mut conn)
        .await;
    }
    Ok(())
}

async fn scratch(admin: &str) -> Result<(String, PgConnection), String> {
    let name = format!(
        "romulus_test_fakedata_{}_{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::Relaxed)
    );
    let mut conn = PgConnection::connect(admin)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    raw_sql(AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&mut conn)
        .await
        .map_err(|e| format!("create database: {e}"))?;
    let (base, _) = admin
        .rsplit_once('/')
        .ok_or("an admin URL with a database")?;
    let db = PgConnection::connect(&format!("{base}/{name}"))
        .await
        .map_err(|e| format!("connect to scratch: {e}"))?;
    Ok((name, db))
}

async fn drop_scratch(admin: &str, name: &str, db: PgConnection) {
    let _ = db.close().await;
    if let Ok(mut conn) = PgConnection::connect(admin).await {
        let _ = raw_sql(AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {name} WITH (FORCE)"
        )))
        .execute(&mut conn)
        .await;
    }
}

/// The role a playground's schema belongs to: not a superuser.
const OWNER: &str = "romulus_test_fakedata_owner";

fn owner_url(admin: &str, database: &str) -> Result<String, String> {
    let (scheme, rest) = admin.split_once("://").ok_or("an admin URL")?;
    let (_, host) = rest
        .rsplit_once('@')
        .ok_or("an admin URL with credentials")?;
    let (host, _) = host.split_once('/').unwrap_or((host, ""));
    Ok(format!("{scheme}://{OWNER}:{OWNER}@{host}/{database}"))
}

/// Gives the model's schemas and relations to [`OWNER`].
async fn hand_over(
    admin_db: &mut PgConnection,
    model: &Schema,
    database: &str,
) -> Result<(), String> {
    let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let mut statements = vec![format!("GRANT CREATE ON DATABASE {database} TO {OWNER}")];
    let schemas: BTreeSet<&str> = model.entities.iter().map(|e| e.schema.as_str()).collect();
    for s in schemas {
        statements.push(format!("ALTER SCHEMA {} OWNER TO {OWNER}", quote(s)));
    }
    for e in &model.entities {
        let kind = match e.kind {
            remus_core::model::EntityKind::View => "VIEW",
            remus_core::model::EntityKind::MaterializedView => "MATERIALIZED VIEW",
            remus_core::model::EntityKind::ForeignTable => "FOREIGN TABLE",
            _ => "TABLE",
        };
        statements.push(format!(
            "ALTER {kind} {}.{} OWNER TO {OWNER}",
            quote(&e.schema),
            quote(&e.name)
        ));
    }
    for p in &model.partitions {
        statements.push(format!(
            "ALTER TABLE {}.{} OWNER TO {OWNER}",
            quote(&p.child_schema),
            quote(&p.child)
        ));
    }
    // Schemas the model does not list (the stubs', an extension's) are
    // usable by the owner, as in a playground.
    statements.push(format!(
        "DO $$ DECLARE s text; BEGIN FOR s IN SELECT nspname FROM pg_namespace WHERE nspname NOT LIKE 'pg\\_%' AND nspname <> 'information_schema' LOOP EXECUTE format('GRANT USAGE ON SCHEMA %I TO {OWNER}', s); END LOOP; END $$"
    ));
    for s in statements {
        // An extension's objects stay the superuser's: that is fine.
        let _ = raw_sql(AssertSqlSafe(s)).execute(&mut *admin_db).await;
    }
    Ok(())
}

/// Runs every step of the plan; `Err` names the step that failed.
pub async fn load(
    db: &mut PgConnection,
    model: &Schema,
    options: &Options,
) -> Result<Report, String> {
    let plan = plan(model, options).map_err(|e| e.to_string())?;
    let mut report = plan.report();
    let mut outcome = None;
    for step in plan.steps() {
        let label = step.label();
        match step {
            Step::Sql(sql) => {
                raw_sql(AssertSqlSafe(sql.clone()))
                    .execute(&mut *db)
                    .await
                    .map_err(|e| format!("step `{label}` failed: {e}\n{sql}"))?;
            }
            Step::CopyTable(copy) => {
                let statement = copy.statement();
                let mut sink = db
                    .copy_in_raw(&statement)
                    .await
                    .map_err(|e| format!("`{statement}`: {e}"))?;
                let mut buffer = Vec::with_capacity(1 << 16);
                for row in copy.rows {
                    copy_text(&row, &mut buffer);
                    if buffer.len() > 1 << 16 {
                        sink.send(std::mem::take(&mut buffer))
                            .await
                            .map_err(|e| format!("`{statement}`: {e}"))?;
                    }
                }
                if !buffer.is_empty() {
                    sink.send(buffer)
                        .await
                        .map_err(|e| format!("`{statement}`: {e}"))?;
                }
                sink.finish()
                    .await
                    .map_err(|e| format!("`{statement}`: {e}"))?;
            }
            Step::Outcome(query) => {
                let json: String = raw_sql(AssertSqlSafe(query))
                    .fetch_one(&mut *db)
                    .await
                    .map_err(|e| format!("the outcome: {e}"))?
                    .get(0);
                outcome = Some(json);
            }
        }
    }
    let json = outcome.ok_or("no outcome step")?;
    report.record_outcome(&json).map_err(|e| e.to_string())?;
    Ok(report)
}

/// Every table holds what the report says, and every missing row is
/// explained.
pub async fn verify(db: &mut PgConnection, model: &Schema, report: &Report) -> Vec<String> {
    let mut problems = Vec::new();
    // Every constraint is still there (a cycle's foreign keys are dropped
    // and added back while it loads).
    for e in &model.entities {
        if !e.kind.is_table() || e.constraints.is_empty() {
            continue;
        }
        let sql = format!(
            "SELECT count(*) FROM pg_constraint c JOIN pg_class r ON r.oid = c.conrelid JOIN pg_namespace n ON n.oid = r.relnamespace \
             WHERE n.nspname = '{}' AND r.relname = '{}' AND c.contype IN ('p', 'u', 'f', 'c', 'x')",
            e.schema.replace('\'', "''"),
            e.name.replace('\'', "''")
        );
        let row = match raw_sql(AssertSqlSafe(sql)).fetch_one(&mut *db).await {
            Ok(row) => row,
            Err(err) => {
                problems.push(format!(
                    "{}.{}: cannot count its constraints: {err}",
                    e.schema, e.name
                ));
                continue;
            }
        };
        let count: i64 = row.get(0);
        if usize::try_from(count).unwrap_or(0) != e.constraints.len() {
            problems.push(format!(
                "{}.{}: {count} constraints after the load, the model has {}",
                e.schema,
                e.name,
                e.constraints.len()
            ));
        }
    }
    for t in &report.tables {
        let name = format!("{}.{}", t.schema, t.table);
        let sql = format!(
            "SELECT count(*) FROM \"{}\".\"{}\"",
            t.schema.replace('"', "\"\""),
            t.table.replace('"', "\"\"")
        );
        let count: i64 = match raw_sql(AssertSqlSafe(sql)).fetch_one(&mut *db).await {
            Ok(row) => row.get(0),
            Err(e) => {
                problems.push(format!("{name}: cannot count: {e}"));
                continue;
            }
        };
        let inserted = t.rows_inserted.unwrap_or(0);
        if u64::try_from(count).unwrap_or(0) != inserted {
            problems.push(format!(
                "{name}: holds {count} rows, the report says {inserted} were inserted"
            ));
        }
        let missing = t.rows_planned.saturating_sub(inserted);
        let explained: u64 = t.dropped.iter().map(|d| d.rows).sum();
        if missing > explained {
            problems.push(format!(
                "{name}: {missing} of {} planned rows are missing, the report explains {explained}: {:?}",
                t.rows_planned, t.dropped
            ));
        }
    }
    problems
}

/// Builds `sql` on a scratch database, loads its plan as the owner, checks
/// the result. `Ok` holds the report and what does not add up.
pub async fn run(
    compiler: &Compiler,
    admin: &str,
    name: &str,
    sql: &str,
    options: &Options,
) -> Result<(Report, Vec<String>), String> {
    let compilation = compiler
        .compile(sql)
        .await
        .map_err(|e| format!("the sandbox failed: {e}"))?;
    let model = compilation
        .model
        .clone()
        .ok_or_else(|| format!("does not compile: {:?}", compilation.diagnostics))?;
    create_roles(admin, &model).await?;
    let mut conn = PgConnection::connect(admin)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    let _ = raw_sql(AssertSqlSafe(format!(
        "DO $$ BEGIN CREATE ROLE {OWNER} LOGIN PASSWORD '{OWNER}'; EXCEPTION WHEN duplicate_object THEN NULL; END $$"
    )))
    .execute(&mut conn)
    .await;
    let _ = conn.close().await;
    let (db_name, mut db) = scratch(admin).await?;
    let started = Instant::now();
    let result = async {
        raw_sql(AssertSqlSafe(stubs(&compilation)))
            .execute(&mut db)
            .await
            .map_err(|e| format!("stubs: {e}"))?;
        // The stubs' schemas exist already: the text creating them again
        // must not fail (a playground's build does the same).
        let schema_sql = sql
            .lines()
            .map(|l| match l.strip_prefix("CREATE SCHEMA ") {
                Some(rest) if !rest.starts_with("IF NOT EXISTS") => {
                    format!("CREATE SCHEMA IF NOT EXISTS {rest}")
                }
                _ => l.to_owned(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        raw_sql(AssertSqlSafe(schema_sql))
            .execute(&mut db)
            .await
            .map_err(|e| format!("the schema does not build: {e}"))?;
        hand_over(&mut db, &model, &db_name).await?;
        let mut owner = PgConnection::connect(&owner_url(admin, &db_name)?)
            .await
            .map_err(|e| format!("connect as the owner: {e}"))?;
        let report = load(&mut owner, &model, options).await;
        let _ = owner.close().await;
        let report = report?;
        let problems = verify(&mut db, &model, &report).await;
        Ok::<_, String>((report, problems))
    }
    .await;
    drop_scratch(admin, &db_name, db).await;
    #[allow(clippy::print_stderr)]
    if let Ok((report, _)) = &result {
        eprintln!(
            "{name}: {} rows planned, {} inserted, in {:?}",
            report.rows_planned,
            report.rows_inserted.unwrap_or(0),
            started.elapsed()
        );
        for line in report.lines() {
            eprintln!("    {line}");
        }
    }
    result
}
