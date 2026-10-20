//! The migration as each tool runs it, on a real database.
//!
//! The sandbox compile runs a text as one transaction, without roles,
//! with guessed functions and without superuser rights: it checks the order
//! of the statements, not how a tool applies the files. Here each case gets
//! a scratch database on the sandbox cluster (as its superuser), with the
//! roles the policies name and real functions for the ones the schemas call
//! without defining (one signature each, as a real database has). The old
//! schema is built, then the files run the way the tool runs them:
//!
//! - **plain**: like `psql -f`: statement by statement, `BEGIN`/`COMMIT` as
//!   written;
//! - **sqlx**: each file in its own transaction, unless `-- no-transaction`;
//!   the file is one multi-statement query;
//! - **flyway**: statement by statement, in a transaction per file unless
//!   its `.sql.conf` says `executeInTransaction=false`;
//! - **prisma**: each `migration.sql` as one multi-statement query (Postgres
//!   runs it as one implicit transaction);
//! - **dbmate**: each section in a transaction unless `transaction:false`,
//!   as one multi-statement query;
//! - **golang-migrate**: each file as one multi-statement query.
//!
//! Files run in the order the tool sorts them (by version). After the up
//! migration the database must introspect as the new schema; after the
//! down migration (dbmate, golang-migrate), as the old one.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    sync::atomic::{AtomicUsize, Ordering},
};

use remus_core::{
    INTROSPECT_SQL, Schema,
    emit::ident::{qualified, quote_ident, quote_literal},
    model::{Column, TypeKind},
};
use romulus_migration::{
    File as MigrationFile, FileRole as MigrationFileRole, Format, Meta, Migration, Options,
    RenameKind, WarningCode, files, migration, without_extension_objects,
};
use romulus_sandbox::{Compiler, parse, render_sql};
use sqlx::{AssertSqlSafe, Connection, PgConnection, Row, raw_sql};
use tokio::task::JoinSet;

use crate::admin_url;

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

/// Tool runs whose tables were given rows, and how many tables in all.
static SEEDED_RUNS: AtomicUsize = AtomicUsize::new(0);
static SEEDED_TABLES: AtomicUsize = AtomicUsize::new(0);

/// One pair, laid out for one tool.
#[derive(Clone)]
pub struct ToolCase {
    pub label: String,
    pub old: Schema,
    pub new: Schema,
    pub options: Options,
    pub format: Format,
    /// The old and new SQL as written, built instead of `render_sql` of the
    /// models: a partitioned table's index then has its partitions' copies,
    /// named by Postgres (`render_sql` creates it `ON ONLY`, without them).
    pub sources: Option<(String, String)>,
    /// Give every table that takes one a row first (`DEFAULT VALUES`), and
    /// check that a migration flagging nothing destructive keeps it, up
    /// (under the table's new name) and down.
    pub rows: bool,
}

/// A value of `column`'s type a row can hold, for a column that needs one
/// (`NOT NULL`, no default): 1 for numbers (what a parent's first serial
/// key is, so a foreign key finds it), `'x'` for text, an enum's first
/// label… `None` for a type left alone (the table gets no row).
fn value_for(schema: &Schema, column: &Column) -> Option<String> {
    if column.is_array {
        return Some(format!("'{{}}'::{}", column.r#type));
    }
    let literal = match column.type_kind {
        TypeKind::Enum => {
            let e = schema
                .enums
                .iter()
                .find(|e| e.schema == column.type_schema && e.name == column.type_name)?;
            quote_literal(e.labels.first()?)
        }
        TypeKind::Base => match column.type_name.as_str() {
            "int2" | "int4" | "int8" | "numeric" | "float4" | "float8" | "money" | "oid" => {
                "1".to_owned()
            }
            "text" | "varchar" | "bpchar" | "name" | "citext" | "char" => "'x'".to_owned(),
            "bool" => "true".to_owned(),
            "date" | "timestamp" | "timestamptz" | "time" | "timetz" => "now()".to_owned(),
            "interval" => "'1 day'".to_owned(),
            "uuid" => "'00000000-0000-0000-0000-000000000001'".to_owned(),
            "json" | "jsonb" => "'{}'".to_owned(),
            "bytea" => "'\\x00'".to_owned(),
            "inet" | "cidr" => "'127.0.0.1'".to_owned(),
            _ => return None,
        },
        _ => return None,
    };
    Some(format!("({literal})::{}", column.r#type))
}

/// A row in every table of `schema` that takes one (parents before what
/// references them: a few passes): the tables that have one.
async fn seed_rows(db: &mut PgConnection, schema: &Schema) -> Vec<(String, String)> {
    let mut seeded: Vec<(String, String)> = Vec::new();
    for _ in 0..3 {
        for e in schema.entities.iter().filter(|e| e.kind.is_table()) {
            if seeded.iter().any(|(s, n)| *s == e.schema && *n == e.name) {
                continue;
            }
            let needed: Vec<&Column> = e
                .columns
                .iter()
                .filter(|c| {
                    c.not_null
                        && c.default.is_none()
                        && c.identity.is_none()
                        && c.generated.is_none()
                })
                .collect();
            let Some(values) = needed
                .iter()
                .map(|c| value_for(schema, c))
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            let table = qualified(&e.schema, &e.name);
            let insert = if needed.is_empty() {
                format!("INSERT INTO {table} DEFAULT VALUES;")
            } else {
                let columns: Vec<String> = needed
                    .iter()
                    .map(|c| quote_ident(&c.name).into_owned())
                    .collect();
                format!(
                    "INSERT INTO {table} ({}) VALUES ({});",
                    columns.join(", "),
                    values.join(", ")
                )
            };
            if script(db, &insert).await.is_ok() {
                seeded.push((e.schema.clone(), e.name.clone()));
            }
        }
    }
    seeded
}

/// Whether each table of `seeded` (named as `renamed` maps it) still has
/// a row, when `m` flags nothing destructive (a table it drops on purpose
/// is flagged).
async fn rows_kept(
    db: &mut PgConnection,
    m: &Migration,
    seeded: &[(String, String)],
    renamed: impl Fn(&str, &str) -> (String, String),
    target: &Schema,
) -> Result<(), String> {
    if m.statements.iter().any(|s| s.destructive) {
        return Ok(());
    }
    for (schema, table) in seeded {
        let (schema, table) = renamed(schema, table);
        if !target
            .entities
            .iter()
            .any(|e| e.schema == schema && e.name == table && e.kind.is_table())
        {
            continue;
        }
        let count: i64 = sqlx::query(AssertSqlSafe(format!(
            "SELECT count(*) FROM {}",
            qualified(&schema, &table)
        )))
        .fetch_one(&mut *db)
        .await
        .map_err(|e| format!("count {schema}.{table}: {e}"))?
        .get(0);
        if count == 0 {
            return Err(format!(
                "{schema}.{table} lost its row in a migration flagging nothing destructive"
            ));
        }
    }
    Ok(())
}

/// Where the up migration `m` takes table `schema.table`: its renames and
/// moves.
fn table_after(m: &Migration, schema: &str, table: &str) -> (String, String) {
    m.applied
        .iter()
        .find(|r| {
            matches!(r.kind, RenameKind::Table | RenameKind::Partition)
                && r.schema == schema
                && r.from == table
        })
        .map_or_else(
            || (schema.to_owned(), table.to_owned()),
            |r| (r.final_schema().to_owned(), r.to.clone()),
        )
}

/// How a case went, when it did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOutcome {
    Passed,
    /// The pair cannot exist in one real database (see the reason).
    Excluded(&'static str),
}

/// A database of its own on the sandbox cluster, dropped by [`drop_scratch`].
pub async fn scratch(admin: &str) -> Result<(String, PgConnection), String> {
    let name = format!(
        "romulus_test_tools_{}_{}",
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

pub async fn drop_scratch(admin: &str, name: &str, db: PgConnection) {
    let _ = db.close().await;
    if let Ok(mut conn) = PgConnection::connect(admin).await {
        let _ = raw_sql(AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {name} WITH (FORCE)"
        )))
        .execute(&mut conn)
        .await;
    }
}

/// Roles the policies of either side name.
fn roles(schemas: [&Schema; 2]) -> BTreeSet<String> {
    schemas
        .iter()
        .flat_map(|s| &s.entities)
        .flat_map(|e| &e.policies)
        .flat_map(|p| &p.roles)
        .filter(|r| *r != "public")
        .cloned()
        .collect()
}

async fn create_roles(admin: &str, roles: &BTreeSet<String>) -> Result<(), String> {
    let mut conn = PgConnection::connect(admin)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    for role in roles {
        let exists: i64 = sqlx::query("SELECT count(*) FROM pg_roles WHERE rolname = $1")
            .bind(role)
            .fetch_one(&mut conn)
            .await
            .map_err(|e| format!("roles: {e}"))?
            .get(0);
        if exists == 0 {
            // Another case may create it meanwhile: "already exists" is fine.
            let _ = raw_sql(AssertSqlSafe(format!(
                "CREATE ROLE \"{}\"",
                role.replace('"', "\"\"")
            )))
            .execute(&mut conn)
            .await;
        }
    }
    Ok(())
}

/// Functions the schemas call without defining them, as the sandbox
/// inferred them from each side: one definition each, or `Err` when the
/// two sides need different return types (no real database has both).
async fn functions(
    compiler: &Compiler,
    old_sql: &str,
    new_sql: &str,
) -> Result<String, ToolOutcome> {
    let mut signatures: BTreeMap<(String, String, Vec<String>), String> = BTreeMap::new();
    let mut schemas = BTreeSet::new();
    for sql in [old_sql, new_sql] {
        let compilation = compiler
            .compile(sql)
            .await
            .map_err(|_| ToolOutcome::Excluded("the sandbox is unavailable"))?;
        schemas.extend(compilation.created_schemas.iter().cloned());
        for stub in &compilation.stubs {
            let key = (
                stub.schema.clone(),
                stub.name.clone(),
                stub.args().map(str::to_owned).collect::<Vec<_>>(),
            );
            match signatures.get(&key) {
                Some(returns) if returns != stub.returns() => {
                    return Err(ToolOutcome::Excluded(
                        "the new schema calls a function with another return type than the old one",
                    ));
                }
                _ => {
                    signatures.insert(key, stub.returns().to_owned());
                }
            }
            schemas.insert(stub.schema.clone());
        }
    }
    let mut sql = String::new();
    for schema in schemas {
        let _ = writeln!(sql, "CREATE SCHEMA IF NOT EXISTS \"{schema}\";");
    }
    for ((schema, name, args), returns) in signatures {
        let body = if returns.ends_with("void") {
            "BEGIN END"
        } else {
            "BEGIN RETURN NULL; END"
        };
        let _ = writeln!(
            sql,
            "CREATE FUNCTION \"{schema}\".\"{name}\"({}) RETURNS {returns} LANGUAGE plpgsql IMMUTABLE AS $f$ {body} $f$;",
            args.join(", ")
        );
    }
    Ok(sql)
}

async fn introspect(db: &mut PgConnection) -> Result<Schema, String> {
    let json: String = raw_sql(INTROSPECT_SQL)
        .fetch_one(&mut *db)
        .await
        .map_err(|e| format!("introspect: {e}"))?
        .get(0);
    Schema::from_json(&json)
        .map(Schema::canonical)
        .map_err(|e| format!("introspected JSON: {e}"))
}

/// The text rendered for a model, without what its extensions create.
fn rendered(schema: &Schema) -> String {
    let mut schema = schema.clone();
    without_extension_objects(&mut schema);
    render_sql(&schema)
}

/// One multi-statement query (an implicit transaction unless the text
/// controls its own).
pub async fn script(db: &mut PgConnection, sql: &str) -> Result<(), String> {
    raw_sql(AssertSqlSafe(sql.to_owned()))
        .execute(&mut *db)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Each statement on its own, as psql or Flyway send them.
pub async fn statements(db: &mut PgConnection, sql: &str) -> Result<(), String> {
    let parsed = parse(sql);
    if let Some(error) = parsed.errors.first() {
        return Err(format!("does not parse: {}", error.message));
    }
    for statement in &parsed.statements {
        let text = &sql[statement.span.range()];
        raw_sql(AssertSqlSafe(text.to_owned()))
            .execute(&mut *db)
            .await
            .map_err(|e| format!("{e}\n  in: {text}"))?;
    }
    Ok(())
}

async fn in_transaction(db: &mut PgConnection, sql: &str, split: bool) -> Result<(), String> {
    script(db, "BEGIN").await?;
    let result = if split {
        statements(db, sql).await
    } else {
        script(db, sql).await
    };
    if result.is_err() {
        let _ = script(db, "ROLLBACK").await;
        return result;
    }
    script(db, "COMMIT").await
}

/// The version a tool reads off a file name, for sorting: numbers, or
/// Flyway's dotted parts.
fn version_of(format: Format, path: &str) -> Vec<u64> {
    let name = path.rsplit('/').next_back().unwrap_or(path);
    let name = if format == Format::Prisma {
        path.split('/').next().unwrap_or(path)
    } else {
        name
    };
    let digits = if format == Format::Flyway {
        name.trim_start_matches('V')
            .split("__")
            .next()
            .unwrap_or("")
    } else {
        name.split('_').next().unwrap_or("")
    };
    digits.split('.').filter_map(|d| d.parse().ok()).collect()
}

fn ordered(
    format: Format,
    files: &[MigrationFile],
    role: MigrationFileRole,
) -> Vec<&MigrationFile> {
    let mut out: Vec<&MigrationFile> = files.iter().filter(|f| f.role == role).collect();
    out.sort_by_key(|f| version_of(format, &f.path));
    out
}

/// A dbmate file's section: the text after `-- migrate:<which>`, and
/// whether its marker says `transaction:false`.
fn dbmate_section<'c>(content: &'c str, which: &str) -> (&'c str, bool) {
    let marker = format!("-- migrate:{which}");
    let Some(at) = content.find(&marker) else {
        return ("", true);
    };
    let rest = &content[at..];
    let (line, body) = rest.split_once('\n').unwrap_or((rest, ""));
    let transaction = !line.contains("transaction:false");
    let body = if which == "up" {
        body.split("-- migrate:down").next().unwrap_or(body)
    } else {
        body
    };
    (body, transaction)
}

/// Applies the up (`down: false`) or down files of `format` as the tool does.
async fn apply(
    db: &mut PgConnection,
    format: Format,
    all: &[MigrationFile],
    down: bool,
) -> Result<(), String> {
    let role = if down && format == Format::GolangMigrate {
        MigrationFileRole::Down
    } else {
        MigrationFileRole::Up
    };
    let mut list = ordered(format, all, role);
    if down {
        list.reverse();
    }
    for file in list {
        let context = |e: String| format!("{}: {e}", file.path);
        match format {
            Format::Plain => statements(db, &file.content).await.map_err(context)?,
            Format::Sqlx => {
                if file.content.starts_with("-- no-transaction") {
                    script(db, &file.content).await.map_err(context)?;
                } else {
                    in_transaction(db, &file.content, false)
                        .await
                        .map_err(context)?;
                }
            }
            Format::Flyway => {
                let conf = all.iter().find(|f| f.path == format!("{}.conf", file.path));
                if conf.is_some_and(|c| c.content.contains("executeInTransaction=false")) {
                    statements(db, &file.content).await.map_err(context)?;
                } else {
                    in_transaction(db, &file.content, true)
                        .await
                        .map_err(context)?;
                }
            }
            Format::Prisma | Format::GolangMigrate => {
                script(db, &file.content).await.map_err(context)?;
            }
            Format::Dbmate => {
                let (section, transaction) =
                    dbmate_section(&file.content, if down { "down" } else { "up" });
                if transaction {
                    in_transaction(db, section, false).await.map_err(context)?;
                } else {
                    script(db, section).await.map_err(context)?;
                }
            }
        }
    }
    Ok(())
}

/// `actual` compared with `expected`, columns put in `expected`'s order
/// where the migration warned that Postgres would append them.
fn compare(
    label: &str,
    m: &Migration,
    mut actual: Schema,
    expected: &Schema,
) -> Result<(), String> {
    for warning in m
        .warnings
        .iter()
        .filter(|w| w.code == WarningCode::ColumnOrder)
    {
        let Some(object) = &warning.object else {
            continue;
        };
        let Some(wanted) = expected
            .entities
            .iter()
            .find(|e| format!("{}.{}", e.schema, e.name) == *object)
        else {
            continue;
        };
        if let Some(entity) = actual
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
        let found = actual
            .composites
            .iter_mut()
            .find(|c| format!("{}.{}", c.schema, c.name) == *object);
        if let (Some(composite), Some(wanted)) = (
            found,
            expected
                .composites
                .iter()
                .find(|c| format!("{}.{}", c.schema, c.name) == *object),
        ) {
            composite.attributes.sort_by_key(|a| {
                wanted
                    .attributes
                    .iter()
                    .position(|w| w.name == a.name)
                    .unwrap_or(usize::MAX)
            });
        }
    }
    let diff = romulus_diff::diff(&actual, expected);
    if diff.is_empty() {
        return Ok(());
    }
    let mut out = format!("{label}:\n");
    for line in diff.lines() {
        let _ = writeln!(out, "  {line}");
    }
    Err(out)
}

fn meta(format: Format) -> Meta {
    Meta {
        database: "acme/app".to_owned(),
        from: "v1".to_owned(),
        to: "v2".to_owned(),
        date: "2026-10-16".to_owned(),
        version: if format == Format::Flyway {
            "2".to_owned()
        } else {
            "20261016093000".to_owned()
        },
        name: "case".to_owned(),
    }
}

fn listing(files: &[MigrationFile]) -> String {
    files
        .iter()
        .map(|f| format!("===== {} =====\n{}", f.path, f.content))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs one case; `Err` explains how it went wrong.
pub async fn run(compiler: &Compiler, admin: &str, case: &ToolCase) -> Result<ToolOutcome, String> {
    let (old_sql, new_sql) = case
        .sources
        .clone()
        .unwrap_or_else(|| (rendered(&case.old), rendered(&case.new)));
    let prelude = match functions(compiler, &old_sql, &new_sql).await {
        Ok(prelude) => prelude,
        Err(outcome) => return Ok(outcome),
    };
    create_roles(admin, &roles([&case.old, &case.new])).await?;

    // The new schema, built directly: what the migration must reach.
    let (name, mut db) = scratch(admin).await?;
    let built = async {
        script(&mut db, &prelude).await?;
        script(&mut db, &new_sql).await?;
        introspect(&mut db).await
    }
    .await;
    drop_scratch(admin, &name, db).await;
    let Ok(new) = built else {
        return Ok(ToolOutcome::Excluded(
            "the new schema does not build with the old schema's functions",
        ));
    };

    let (name, mut db) = scratch(admin).await?;
    let result = async {
        script(&mut db, &prelude)
            .await
            .map_err(|e| format!("functions: {e}"))?;
        script(&mut db, &old_sql)
            .await
            .map_err(|e| format!("the old schema does not build: {e}"))?;
        let old = introspect(&mut db).await?;
        let up = migration(&old, &new, &crate::options_for(compiler, &case.options));
        let down = romulus_migration::down(&old, &new, &up);
        // Rows only where the up flags nothing and the down cannot fail on
        // them: what is flagged is proven by data.rs; this proves what is
        // not (the down's rows are checked only when it flags nothing).
        let harmless = |m: &Migration| !m.statements.iter().any(|s| s.destructive);
        let down_runs = !down
            .statements
            .iter()
            .any(|s| s.hazard == Some(romulus_migration::Hazard::CanFail));
        let seeded = if case.rows && harmless(&up) && down_runs {
            seed_rows(&mut db, &old).await
        } else {
            Vec::new()
        };
        if !seeded.is_empty() {
            SEEDED_RUNS.fetch_add(1, Ordering::Relaxed);
            SEEDED_TABLES.fetch_add(seeded.len(), Ordering::Relaxed);
        }
        let laid_out = files(&up, &down, case.format, &meta(case.format));
        apply(&mut db, case.format, &laid_out, false)
            .await
            .map_err(|e| format!("up: {e}\n{}", listing(&laid_out)))?;
        compare(
            "after the up migration",
            &up,
            introspect(&mut db).await?,
            &new,
        )
        .map_err(|e| format!("{e}{}", listing(&laid_out)))?;
        rows_kept(&mut db, &up, &seeded, |s, t| table_after(&up, s, t), &new)
            .await
            .map_err(|e| format!("up: {e}{}", listing(&laid_out)))?;
        if case.format.has_down() {
            apply(&mut db, case.format, &laid_out, true)
                .await
                .map_err(|e| format!("down: {e}\n{}", listing(&laid_out)))?;
            compare(
                "after the down migration",
                &down,
                introspect(&mut db).await?,
                &old,
            )
            .map_err(|e| format!("{e}{}", listing(&laid_out)))?;
            rows_kept(
                &mut db,
                &down,
                &seeded,
                |s, t| (s.to_owned(), t.to_owned()),
                &old,
            )
            .await
            .map_err(|e| format!("down: {e}{}", listing(&laid_out)))?;
        }
        Ok::<_, String>(())
    }
    .await;
    drop_scratch(admin, &name, db).await;
    result.map(|()| ToolOutcome::Passed)
}

/// Runs every case, a few at a time; fails with every failure.
pub async fn check_tools(compiler: &Compiler, cases: Vec<ToolCase>) {
    let Some(admin) = admin_url() else { return };
    let total = cases.len();
    let mut set = JoinSet::new();
    let mut pending = cases.into_iter();
    let mut running = 0;
    let mut failures = Vec::new();
    let mut excluded: BTreeMap<&'static str, usize> = BTreeMap::new();
    loop {
        while running < 6 {
            let Some(case) = pending.next() else { break };
            let (compiler, admin) = (compiler.clone(), admin.clone());
            set.spawn(async move {
                let label = format!(
                    "{} [{:?}{}]",
                    case.label,
                    case.format,
                    if case.options.concurrently {
                        ", concurrently"
                    } else {
                        ""
                    }
                );
                (label, run(&compiler, &admin, &case).await)
            });
            running += 1;
        }
        let Some(joined) = set.join_next().await else {
            break;
        };
        running -= 1;
        let (label, result) = joined.expect("a tool case task");
        match result {
            Ok(ToolOutcome::Passed) => {}
            Ok(ToolOutcome::Excluded(reason)) => *excluded.entry(reason).or_default() += 1,
            Err(failure) => failures.push(format!("{label}: {failure}")),
        }
    }
    let excluded_total: usize = excluded.values().sum();
    #[allow(clippy::print_stderr)]
    {
        eprintln!(
            "{total} tool runs: {} passed, {excluded_total} excluded {excluded:?}, {} failed; rows kept in {} runs ({} tables)",
            total - excluded_total - failures.len(),
            failures.len(),
            SEEDED_RUNS.load(Ordering::Relaxed),
            SEEDED_TABLES.load(Ordering::Relaxed),
        );
        for failure in &failures {
            let mut lines = failure.lines();
            eprintln!(
                "FAILED {} | {}",
                lines.next().unwrap_or(""),
                lines.next().unwrap_or("")
            );
        }
    }
    assert!(
        excluded_total * 10 <= total,
        "{excluded_total} of {total} tool runs excluded: too many to call this tested"
    );
    if !failures.is_empty() {
        let shown = failures
            .iter()
            .take(4)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n\n=====================================\n\n");
        panic!(
            "{} of {total} tool runs failed. The first ones:\n\n{shown}",
            failures.len()
        );
    }
}
