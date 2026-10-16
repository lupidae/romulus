//! The correctness oracle: for pairs of schemas `(old, new)`, a database
//! built from `render_sql(old)` and then migrated must introspect as `new`.
//!
//! Each pair runs in the sandbox Postgres as one text: `render_sql(old)`,
//! then the migration's statements (`CONCURRENTLY` removed: the sandbox
//! compiles in a transaction, and the order is what is being checked). The
//! model it compiles to is compared with `new` by `romulus_diff::diff`,
//! except for column order where the migration warned about it (Postgres
//! adds columns last).
//!
//! The pairs (see each module): hand-written scenarios, one per kind of
//! change (`scenarios/*.sql`); the sandbox corpus and remus's showcase, from
//! and to nothing and into one another; and random mutations of those.
//!
//! Needs `ROMULUS_SANDBOX_URL` (`.env`, `just sandbox`); skipped, loudly,
//! without it, like romulus-sandbox's own tests.

mod corpus;
mod mutate;
mod scenarios;

use std::{fmt::Write, future::Future, sync::LazyLock, time::Duration};

use remus_core::Schema;
use romulus_migration::{Options, WarningCode, migration};
use romulus_sandbox::{Compilation, Compiler, SandboxConfig, render_sql};
use tokio::{runtime::Runtime, sync::OnceCell, task::JoinSet};

static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("a tokio runtime")
});

static COMPILER: OnceCell<Compiler> = OnceCell::const_new();

fn admin_url() -> Option<String> {
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    std::env::var("ROMULUS_SANDBOX_URL").ok()
}

/// Run a test body on the shared compiler, or skip without a sandbox.
pub fn with_compiler<F, Fut>(body: F)
where
    F: FnOnce(Compiler) -> Fut,
    Fut: Future<Output = ()>,
{
    let Some(url) = admin_url() else {
        #[allow(clippy::print_stderr)]
        {
            eprintln!("SKIPPED: ROMULUS_SANDBOX_URL is not set, the migration oracle did not run");
        }
        return;
    };
    RUNTIME.block_on(async {
        let compiler = COMPILER
            .get_or_init(|| async {
                Compiler::start(SandboxConfig {
                    pool_size: 4,
                    upload_concurrency: Some(4),
                    prefix: "romulus_test_migration".to_owned(),
                    statement_timeout: Duration::from_secs(15),
                    compile_timeout: Duration::from_secs(60),
                    cache_entries: 2048,
                    ..SandboxConfig::new(url)
                })
                .await
                .expect("the sandbox compiler starts (is sandbox-postgres up?)")
            })
            .await
            .clone();
        body(compiler).await;
    });
}

/// Diagnostics, one per line, located in `sql`.
pub fn explain(sql: &str, compilation: &Compilation) -> String {
    compilation
        .diagnostics
        .iter()
        .map(|d| match d.span {
            Some(span) => format!(
                "{:?} {}:{} {} [{}]",
                d.code,
                span.from.line,
                span.from.column,
                d.message,
                &sql[span.range()]
            ),
            None => format!("{:?} {}", d.code, d.message),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The model `sql` compiles to, or the reason it does not.
pub async fn compile(compiler: &Compiler, sql: &str) -> Result<Schema, String> {
    let compilation = compiler
        .compile(sql)
        .await
        .map_err(|e| format!("the sandbox failed: {e}"))?;
    compilation
        .model
        .clone()
        .ok_or_else(|| explain(sql, &compilation))
}

/// A schema holding nothing.
pub fn empty() -> Schema {
    Schema::from_json(r#"{"remus_version": "0.2", "database": "sandbox", "server_version": "17"}"#)
        .expect("an empty model")
}

/// One pair to check.
#[derive(Clone)]
pub struct Case {
    pub label: String,
    pub old: Schema,
    pub new: Schema,
    pub options: Options,
}

impl Case {
    pub fn new(label: impl Into<String>, old: &Schema, new: &Schema) -> Self {
        Self {
            label: label.into(),
            old: old.clone(),
            new: new.clone(),
            options: Options::default(),
        }
    }

    pub fn reversed(&self) -> Self {
        Self {
            label: format!("{} (reversed)", self.label),
            old: self.new.clone(),
            new: self.old.clone(),
            options: self.options.clone(),
        }
    }

    pub fn concurrently(&self) -> Self {
        Self {
            label: format!("{} (concurrently)", self.label),
            options: Options { concurrently: true },
            ..self.clone()
        }
    }
}

/// Migrate `case.old` to `case.new` in the sandbox; `Err` explains how it
/// went wrong.
pub async fn check(compiler: &Compiler, case: &Case) -> Result<(), String> {
    let m = migration(&case.old, &case.new, &case.options);
    let back = migration(&case.new, &case.old, &case.options);
    if romulus_diff::diff(&case.old, &case.new).is_empty() != m.is_empty() && m.warnings.is_empty() {
        return Err(format!(
            "{}: the diff and the migration disagree on emptiness\n{}",
            case.label,
            m.script()
        ));
    }
    let _ = back;
    let script = m
        .script()
        .replace(" INDEX CONCURRENTLY ", " INDEX ");
    let sql = format!("{}\n-- ===== migration =====\n{script}", render_sql(&case.old));
    let mut result = compile(compiler, &sql).await.map_err(|errors| {
        format!(
            "{}: the migration does not run\n{errors}\n--- migration ---\n{script}",
            case.label
        )
    })?;
    // Columns Postgres put last, as the migration warned.
    for warning in &m.warnings {
        if warning.code != WarningCode::ColumnOrder {
            continue;
        }
        let Some(object) = &warning.object else { continue };
        let Some(wanted) = case
            .new
            .entities
            .iter()
            .find(|e| format!("{}.{}", e.schema, e.name) == *object)
        else {
            continue;
        };
        if let Some(entity) = result
            .entities
            .iter_mut()
            .find(|e| e.schema == wanted.schema && e.name == wanted.name)
        {
            entity.columns.sort_by_key(|c| {
                wanted
                    .columns
                    .iter()
                    .position(|w| w.name == c.name)
                    .unwrap_or(usize::MAX)
            });
        }
        if let Some(composite) = result.composites.iter_mut().find(|c| format!("{}.{}", c.schema, c.name) == *object)
            && let Some(wanted) = case.new.composites.iter().find(|c| format!("{}.{}", c.schema, c.name) == *object)
        {
            composite.attributes.sort_by_key(|a| {
                wanted
                    .attributes
                    .iter()
                    .position(|w| w.name == a.name)
                    .unwrap_or(usize::MAX)
            });
        }
    }
    let diff = romulus_diff::diff(&result, &case.new);
    if diff.is_empty() {
        return Ok(());
    }
    let mut out = format!(
        "{}: the migrated database is not the new schema:\n",
        case.label
    );
    for line in diff.lines() {
        let _ = writeln!(out, "  {line}");
    }
    let _ = write!(out, "--- migration ---\n{script}");
    Err(out)
}

/// Check every case, a few at a time, and fail with every failure.
pub async fn check_all(compiler: &Compiler, cases: Vec<Case>) {
    let total = cases.len();
    let mut set = JoinSet::new();
    let mut failures = Vec::new();
    let mut pending = cases.into_iter();
    let mut running = 0;
    loop {
        while running < 8 {
            let Some(case) = pending.next() else { break };
            let compiler = compiler.clone();
            set.spawn(async move { check(&compiler, &case).await });
            running += 1;
        }
        let Some(result) = set.join_next().await else {
            break;
        };
        running -= 1;
        if let Err(failure) = result.expect("a check task") {
            failures.push(failure);
        }
    }
    if !failures.is_empty() {
        let shown = failures
            .iter()
            .take(6)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n\n=====================================\n\n");
        panic!(
            "{} of {total} pairs failed. The first ones:\n\n{shown}",
            failures.len()
        );
    }
}
