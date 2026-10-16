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

    #[must_use]
    pub fn reversed(&self) -> Self {
        Self {
            label: format!("{} (reversed)", self.label),
            old: self.new.clone(),
            new: self.old.clone(),
            options: self.options.clone(),
        }
    }

    #[must_use]
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
/// How a pair went, when it did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    /// The sandbox cannot run something the migration rightly does.
    Skipped,
}

#[allow(clippy::too_many_lines)]
pub async fn check(compiler: &Compiler, case: &Case) -> Result<Outcome, String> {
    let m = migration(&case.old, &case.new, &case.options);
    if romulus_diff::diff(&case.old, &case.new).is_empty() != m.is_empty() && m.warnings.is_empty()
    {
        return Err(format!(
            "{}: the diff and the migration disagree on emptiness\n{}",
            case.label,
            m.script()
        ));
    }
    // The sandbox's compile role does not own extensions: it cannot move
    // one to another schema, which a real owner can.
    if m.statements
        .iter()
        .any(|s| s.sql.starts_with("ALTER EXTENSION ") && s.sql.contains(" SET SCHEMA "))
    {
        return Ok(Outcome::Skipped);
    }
    let script = m.script().replace(" INDEX CONCURRENTLY ", " INDEX ");
    let old_sql = render_sql(&case.old);
    // A schema the old side uses for functions alone exists in a real
    // database, but the sandbox creates it only when the text does not.
    let mut prelude = String::new();
    for s in &m.statements {
        if let Some(schema) = s
            .sql
            .strip_prefix("CREATE SCHEMA IF NOT EXISTS ")
            .and_then(|r| r.strip_suffix(';'))
            && old_sql.contains(&format!("{schema}."))
        {
            let _ = writeln!(prelude, "CREATE SCHEMA IF NOT EXISTS {schema};");
        }
    }
    let sql = format!("{prelude}{old_sql}\n-- ===== migration =====\n{script}");
    let compilation = compiler
        .compile(&sql)
        .await
        .map_err(|e| format!("{}: the sandbox failed: {e}", case.label))?;
    // Functions the schemas call without defining them are stubs whose
    // types the sandbox guesses from the calls: an old schema and a
    // migration calling one with different types make it guess wrong for
    // one of them. A real database has one function; skip those.
    let stubbed = |text: &str| {
        compilation
            .stubs
            .iter()
            .any(|stub| text.contains(&format!("{}(", stub.name)))
    };
    let Some(mut result) = compilation.model.clone() else {
        let errors = explain(&sql, &compilation);
        let in_old_part = compilation.errors().any(|d| {
            d.span.is_some_and(|span| {
                span.from.line as usize <= old_sql.lines().count() + prelude.lines().count()
            })
        });
        let statement_stubbed = compilation.errors().any(|d| {
            d.span.is_some_and(|span| {
                let line = sql
                    .lines()
                    .nth((span.from.line as usize).saturating_sub(1))
                    .unwrap_or("");
                let statement = sql[span.range().start..].split(';').next().unwrap_or("");
                stubbed(line) || stubbed(statement)
            })
        });
        if !compilation.stubs.is_empty() && (in_old_part || statement_stubbed || stubbed(&errors)) {
            return Ok(Outcome::Skipped);
        }
        return Err(format!(
            "{}: the migration does not run\n{errors}\n--- migration ---\n{script}",
            case.label
        ));
    };
    // Columns Postgres put last, as the migration warned.
    for warning in &m.warnings {
        if warning.code != WarningCode::ColumnOrder {
            continue;
        }
        let Some(object) = &warning.object else {
            continue;
        };
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
        if let Some(composite) = result
            .composites
            .iter_mut()
            .find(|c| format!("{}.{}", c.schema, c.name) == *object)
            && let Some(wanted) = case
                .new
                .composites
                .iter()
                .find(|c| format!("{}.{}", c.schema, c.name) == *object)
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
        return Ok(Outcome::Passed);
    }
    if diff.lines().all(stubbed) {
        return Ok(Outcome::Skipped);
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
    let mut skipped = 0;
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
        match result.expect("a check task") {
            Ok(Outcome::Passed) => {}
            Ok(Outcome::Skipped) => skipped += 1,
            Err(failure) => failures.push(failure),
        }
    }
    #[allow(clippy::print_stderr)]
    {
        eprintln!(
            "{total} pairs: {} passed, {skipped} skipped (the sandbox cannot run them), {} failed",
            total - skipped - failures.len(),
            failures.len()
        );
    }
    assert!(
        skipped * 10 <= total,
        "{skipped} of {total} pairs skipped: too many to call this tested"
    );
    if !failures.is_empty() {
        #[allow(clippy::print_stderr)]
        for failure in &failures {
            let mut lines = failure.lines();
            eprintln!(
                "FAILED {} | {}",
                lines.next().unwrap_or(""),
                lines.next().unwrap_or("")
            );
        }
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
