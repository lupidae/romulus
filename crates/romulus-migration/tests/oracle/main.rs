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
//! **Renames.** Each case is planned with renames detected (and those its
//! new text runs, `known_renames`, as known), as a draft's migration is;
//! scenarios run again with every rename left off (drops and adds). Then the
//! down migration (`romulus_migration::down`, the up's renames reversed) runs
//! after the up in the same text, and must give back the old schema.
//!
//! The compile checks the order of the statements, in one transaction. The
//! tool runs (`tools.rs`) check the files: each tool's layout applied as the
//! tool applies it (its transactions, `CONCURRENTLY` for real, down
//! migrations), on a scratch database as the cluster's superuser, with real
//! roles and functions. What the compile cannot run is skipped with a
//! named reason ([`Outcome::Skipped`]) and covered there.
//!
//! Needs `ROMULUS_SANDBOX_URL` (`.env`, `just sandbox`): skipped, loudly,
//! without it on a laptop, and failed without it in CI (`CI` set).
//!
//! **Per Postgres major.** Migrations are planned for the sandbox's major
//! (`Options::major`), so pointing `ROMULUS_SANDBOX_URL` at another major's
//! sandbox (`ROMULUS_SANDBOX_URL_PG15`…; CI runs a job per major) proves
//! them there. Off the default major (17) the corpus pairs and random edits
//! are a smoke subset ([`smoke`]; `ROMULUS_ORACLE_FULL=1` runs them all);
//! the scenarios, one per kind of change, always run whole.

mod corpus;
mod data;
mod mutate;
mod scenarios;
mod tools;

use std::{fmt::Write, future::Future, sync::LazyLock, time::Duration};

use remus_core::{
    Schema,
    emit::ident::{qualified, quote_ident},
};
use romulus_migration::{
    Kind, Migration, Options, RenameChoices, RenameKind, Stage, WarningCode, migration,
};
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

pub fn admin_url() -> Option<String> {
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    std::env::var("ROMULUS_SANDBOX_URL")
        .ok()
        .filter(|url| !url.is_empty())
}

/// The major the product defaults to, where the oracle runs whole.
pub const DEFAULT_MAJOR: u32 = 17;

/// A case's options, for the sandbox's major.
pub fn options_for(compiler: &Compiler, options: &Options) -> Options {
    Options {
        major: Some(compiler.major()),
        ..options.clone()
    }
}

/// Off the default major, a smoke subset of the corpus pairs and random
/// edits, unless `ROMULUS_ORACLE_FULL` is set.
pub fn smoke(compiler: &Compiler) -> bool {
    compiler.major() != DEFAULT_MAJOR && std::env::var_os("ROMULUS_ORACLE_FULL").is_none()
}

/// Run a test body on the shared compiler, or skip without a sandbox.
pub fn with_compiler<F, Fut>(body: F)
where
    F: FnOnce(Compiler) -> Fut,
    Fut: Future<Output = ()>,
{
    let Some(url) = admin_url() else {
        // In CI a missing sandbox is a failure: a skipped oracle passes
        // silently, and it is what proves the migrations.
        assert!(
            std::env::var_os("CI").is_none(),
            "ROMULUS_SANDBOX_URL is not set in CI: the migration oracle cannot run"
        );
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

    /// The other way, renames detected afresh (decisions and known renames
    /// are about the forward way).
    #[must_use]
    pub fn reversed(&self) -> Self {
        Self {
            label: format!("{} (reversed)", self.label),
            old: self.new.clone(),
            new: self.old.clone(),
            options: Options {
                renames: if self.options.renames.detect {
                    RenameChoices::default()
                } else {
                    RenameChoices::exactly(Vec::new())
                },
                ..self.options.clone()
            },
        }
    }

    #[must_use]
    pub fn concurrently(&self) -> Self {
        Self {
            label: format!("{} (concurrently)", self.label),
            options: Options {
                concurrently: true,
                ..self.options.clone()
            },
            ..self.clone()
        }
    }

    /// Every detected rename left off: what dropping and adding does.
    #[must_use]
    pub fn without_renames(&self) -> Self {
        Self {
            label: format!("{} (renames off)", self.label),
            options: Options {
                renames: RenameChoices::exactly(Vec::new()),
                ..self.options.clone()
            },
            ..self.clone()
        }
    }

    /// With the renames a text runs known (its `ALTER … RENAME`s).
    #[must_use]
    pub fn knowing(mut self, known: Vec<romulus_migration::Rename>) -> Self {
        self.options.renames.known = known;
        self
    }

    /// With these renames turned on or off.
    #[must_use]
    pub fn deciding(mut self, decisions: &[(String, bool)]) -> Self {
        self.options
            .renames
            .decisions
            .extend(decisions.iter().cloned());
        self
    }
}

/// Migrate `case.old` to `case.new` in the sandbox; `Err` explains how it
/// went wrong.
/// How a pair went, when it did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    /// The sandbox's compile cannot run something the migration rightly
    /// does; the tool runs (`tools.rs`, a real database as superuser, real
    /// functions) cover these. One of [`MOVED_EXTENSION`], [`GUESSED_FUNCTION`].
    Skipped(&'static str),
}

/// `ALTER EXTENSION … SET SCHEMA`: the compile role does not own extensions.
pub const MOVED_EXTENSION: &str = "moves an extension";
/// A function the schemas call without defining it, whose type the
/// sandbox guesses differently for the old text and the migration.
pub const GUESSED_FUNCTION: &str = "a guessed function signature";
/// Before 17, an enum value cannot be used in the transaction adding it,
/// even one that created the type: the compile runs the old schema and the
/// migration in one; the migration commits its values first, as the tool
/// runs (`tools.rs`) check.
pub const ENUM_VALUES_COMMITTED: &str = "uses enum values it commits first (before 17)";

/// The statements of `m` as the sandbox's compile can run them: `setval`
/// is data (the sandbox runs DDL only; the tool runs execute it), and
/// `CONCURRENTLY` cannot run in its transaction.
fn compile_script(m: &Migration) -> String {
    let mut script = String::new();
    for statement in m.statements.iter().filter(|s| s.kind != Kind::SetSequence) {
        script.push_str(&statement.sql.replace(" INDEX CONCURRENTLY ", " INDEX "));
        script.push('\n');
    }
    script
}

/// Columns Postgres put last, as the migration warned: in `wanted`'s order
/// in `result`, for the objects warned about (every one when `all`).
fn reorder(result: &mut Schema, wanted: &Schema, warned: &[&Migration], all: bool) {
    let objects: Vec<String> = warned
        .iter()
        .flat_map(|m| &m.warnings)
        .filter(|w| w.code == WarningCode::ColumnOrder)
        .filter_map(|w| w.object.clone())
        .collect();
    let named = |schema: &str, name: &str| all || objects.contains(&format!("{schema}.{name}"));
    for entity in &mut result.entities {
        if !named(&entity.schema, &entity.name) {
            continue;
        }
        if let Some(target) = wanted
            .entities
            .iter()
            .find(|e| e.schema == entity.schema && e.name == entity.name)
        {
            entity.columns.sort_by_key(|c| {
                target
                    .columns
                    .iter()
                    .position(|w| w.name == c.name)
                    .unwrap_or(usize::MAX)
            });
        }
    }
    for composite in &mut result.composites {
        if !named(&composite.schema, &composite.name) {
            continue;
        }
        if let Some(target) = wanted
            .composites
            .iter()
            .find(|c| c.schema == composite.schema && c.name == composite.name)
        {
            composite.attributes.sort_by_key(|a| {
                target
                    .attributes
                    .iter()
                    .position(|w| w.name == a.name)
                    .unwrap_or(usize::MAX)
            });
        }
    }
}

/// Compiles `prelude`, the old schema's SQL and `scripts`, and compares the
/// model with `target`; `Err` explains how it went wrong.
#[allow(clippy::too_many_arguments)]
async fn run_scripts(
    compiler: &Compiler,
    label: &str,
    what: &str,
    prelude: &str,
    old_sql: &str,
    scripts: &[(&str, &Migration)],
    target: &Schema,
    reorder_all: bool,
) -> Result<Outcome, String> {
    let mut sql = format!("{prelude}{old_sql}");
    let mut shown = String::new();
    for (name, m) in scripts {
        let script = compile_script(m);
        let _ = write!(sql, "\n-- ===== {name} =====\n{script}");
        let _ = write!(shown, "--- {name} ---\n{script}");
    }
    let compilation = compiler
        .compile(&sql)
        .await
        .map_err(|e| format!("{label}: the sandbox failed: {e}"))?;
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
        if compiler.major() < 17
            && scripts
                .iter()
                .any(|(_, m)| m.statements.iter().any(|s| s.stage == Stage::Commit))
            && compilation
                .errors()
                .any(|d| d.sqlstate.as_deref() == Some("55P04"))
        {
            return Ok(Outcome::Skipped(ENUM_VALUES_COMMITTED));
        }
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
            return Ok(Outcome::Skipped(GUESSED_FUNCTION));
        }
        return Err(format!(
            "{label}: the {what} does not run\n{errors}\n{shown}"
        ));
    };
    let warned: Vec<&Migration> = scripts.iter().map(|(_, m)| *m).collect();
    reorder(&mut result, target, &warned, reorder_all);
    let diff = romulus_diff::diff(&result, target);
    if diff.is_empty() {
        return Ok(Outcome::Passed);
    }
    if diff.lines().all(stubbed) {
        return Ok(Outcome::Skipped(GUESSED_FUNCTION));
    }
    let mut out =
        format!("{label}: after the {what}, the database is not the schema it should be:\n");
    for line in diff.lines() {
        let _ = writeln!(out, "  {line}");
    }
    let _ = write!(out, "{shown}");
    Err(out)
}

pub async fn check(compiler: &Compiler, case: &Case) -> Result<Outcome, String> {
    let m = migration(&case.old, &case.new, &options_for(compiler, &case.options));
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
        return Ok(Outcome::Skipped(MOVED_EXTENSION));
    }
    if let Some(drop) = drops_a_renamed_name(&m) {
        return Err(format!(
            "{}: the migration renames, then drops the old name: {drop}\n{}",
            case.label,
            m.script()
        ));
    }
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
    let up = run_scripts(
        compiler,
        &case.label,
        "migration",
        &prelude,
        &old_sql,
        &[("migration", &m)],
        &case.new,
        false,
    )
    .await?;
    if up != Outcome::Passed {
        return Ok(up);
    }
    // The way back, from where the up migration left the database: its
    // renames reversed, nothing else taken for one. Column order aside
    // (columns added again go last).
    let down = romulus_migration::down(&case.old, &case.new, &m);
    if let Some(drop) = drops_a_renamed_name(&down) {
        return Err(format!(
            "{}: the down migration renames, then drops the old name: {drop}\n{}",
            case.label,
            down.script()
        ));
    }
    // The compile refuses an extension created twice in one text, and
    // cannot move one: the tool runs prove those downs on a real database.
    if down
        .statements
        .iter()
        .any(|s| s.kind == Kind::CreateExtension || s.sql.starts_with("ALTER EXTENSION "))
    {
        return Ok(Outcome::Passed);
    }
    run_scripts(
        compiler,
        &case.label,
        "down migration",
        &prelude,
        &old_sql,
        &[("migration", &m), ("down", &down)],
        &case.old,
        true,
    )
    .await
}

/// A statement dropping what a rename that ran was called before (unless
/// another rename took the name again, as in a swap): the rename was for
/// nothing, the object dropped and created all the same.
pub fn drops_a_renamed_name(m: &Migration) -> Option<String> {
    m.applied
        .iter()
        .filter(|r| r.from != r.to || r.final_schema() != r.schema)
        .find_map(|r| {
            let reused = m.applied.iter().any(|o| {
                o.kind == r.kind
                    && o.parent == r.parent
                    && o.final_schema() == r.schema
                    && o.to == r.from
            });
            if reused {
                return None;
            }
            // The parent as it is called once the renames ran.
            let parent = r.parent.as_deref().map(|p| {
                let now = m
                    .applied
                    .iter()
                    .find(|t| {
                        matches!(
                            t.kind,
                            RenameKind::Table
                                | RenameKind::Partition
                                | RenameKind::View
                                | RenameKind::MaterializedView
                        ) && t.schema == r.schema
                            && t.from == p
                    })
                    .map_or((r.schema.as_str(), p), |t| {
                        (t.final_schema(), t.to.as_str())
                    });
                qualified(now.0, now.1)
            });
            let old = qualified(&r.schema, &r.from);
            let name = quote_ident(&r.from);
            let target = match r.kind {
                RenameKind::Table | RenameKind::Partition => {
                    vec![(Kind::DropTable, format!("DROP TABLE {old};"))]
                }
                RenameKind::View => vec![(Kind::DropView, format!("DROP VIEW {old};"))],
                RenameKind::MaterializedView => {
                    vec![(Kind::DropView, format!("DROP MATERIALIZED VIEW {old};"))]
                }
                RenameKind::Enum | RenameKind::Composite => {
                    vec![(Kind::DropType, format!("DROP TYPE {old};"))]
                }
                RenameKind::Domain => vec![(Kind::DropDomain, format!("DROP DOMAIN {old};"))],
                RenameKind::Sequence => vec![(Kind::DropSequence, format!("DROP SEQUENCE {old};"))],
                RenameKind::Index => vec![
                    (Kind::DropIndex, format!("DROP INDEX {old};")),
                    (Kind::DropIndex, format!("DROP INDEX CONCURRENTLY {old};")),
                ],
                RenameKind::Column => vec![(
                    Kind::DropColumn,
                    format!("ALTER TABLE {} DROP COLUMN {name};", parent.as_deref()?),
                )],
                RenameKind::Constraint => vec![(
                    Kind::DropConstraint,
                    format!("ALTER TABLE {} DROP CONSTRAINT {name};", parent.as_deref()?),
                )],
                RenameKind::Policy => vec![(
                    Kind::DropPolicy,
                    format!("DROP POLICY {name} ON {};", parent.as_deref()?),
                )],
                RenameKind::EnumValue | RenameKind::Attribute => return None,
            };
            m.statements
                .iter()
                .find(|s| {
                    target
                        .iter()
                        .any(|(kind, sql)| s.kind == *kind && s.sql == *sql)
                })
                .map(|s| s.sql.clone())
        })
}

/// Check every case, a few at a time, and fail with every failure. Returns
/// the cases skipped, with why.
pub async fn check_all(compiler: &Compiler, cases: Vec<Case>) -> Vec<(String, &'static str)> {
    let total = cases.len();
    let mut set = JoinSet::new();
    let mut failures = Vec::new();
    let mut skipped: Vec<(String, &'static str)> = Vec::new();
    let mut pending = cases.into_iter();
    let mut running = 0;
    loop {
        while running < 8 {
            let Some(case) = pending.next() else { break };
            let compiler = compiler.clone();
            set.spawn(async move { (case.label.clone(), check(&compiler, &case).await) });
            running += 1;
        }
        let Some(result) = set.join_next().await else {
            break;
        };
        running -= 1;
        let (label, result) = result.expect("a check task");
        match result {
            Ok(Outcome::Passed) => {}
            Ok(Outcome::Skipped(reason)) => skipped.push((label, reason)),
            Err(failure) => failures.push(failure),
        }
    }
    #[allow(clippy::print_stderr)]
    {
        eprintln!(
            "{total} pairs: {} passed, {} skipped (the sandbox cannot run them: {:?}), {} failed",
            total - skipped.len() - failures.len(),
            skipped.len(),
            skipped
                .iter()
                .map(|(_, why)| *why)
                .collect::<std::collections::BTreeSet<_>>(),
            failures.len()
        );
    }
    // Enum values committed first are proven by the tool runs, which
    // commit as the tools do: they do not count against the compile.
    let unproven = skipped
        .iter()
        .filter(|(_, why)| *why != ENUM_VALUES_COMMITTED)
        .count();
    assert!(
        unproven * 10 <= total,
        "{unproven} of {total} pairs skipped: too many to call this tested"
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
            .take(if std::env::var_os("ROMULUS_ORACLE_ALL").is_some() {
                usize::MAX
            } else {
                6
            })
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n\n=====================================\n\n");
        panic!(
            "{} of {total} pairs failed. The first ones:\n\n{shown}",
            failures.len()
        );
    }
    skipped
}
