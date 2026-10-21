//! Orientation and reading: who the token is, the organizations' databases,
//! their schemas, versions and diffs, and searching them.

use std::fmt::Write as _;

use romulus_diff::SchemaDiff;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Ctx, Output, Tool, ToolError, args, schema};
use crate::{
    auth::{CurrentUser, access_tokens::TokenKind},
    databases::{Database, search::SearchResults},
    drafts::Draft,
    orgs::Org,
    versions::{VersionDetail, VersionPage, VersionSummary, VersionTexts},
};

pub const TOOLS: &[&Tool] = &[
    &WHOAMI,
    &LIST_DATABASES,
    &GET_DATABASE,
    &GET_SCHEMA,
    &LIST_VERSIONS,
    &DIFF_VERSIONS,
    &SEARCH_SCHEMA,
];

pub fn handles(name: &str) -> bool {
    TOOLS.iter().any(|tool| tool.name == name)
}

pub async fn call(ctx: &Ctx, name: &str, arguments: Value) -> Result<Output, ToolError> {
    match name {
        "whoami" => whoami(ctx).await,
        "list_databases" => list_databases(ctx, args(arguments)?).await,
        "get_database" => get_database(ctx, args(arguments)?).await,
        "get_schema" => get_schema(ctx, args(arguments)?).await,
        "list_versions" => list_versions(ctx, args(arguments)?).await,
        "diff_versions" => diff_versions(ctx, args(arguments)?).await,
        _ => search_schema(ctx, args(arguments)?).await,
    }
}

static WHOAMI: Tool = Tool {
    name: "whoami",
    title: "Who am I",
    description: "Who this token acts as in romulus: the person, the token's access (read-only \
                  or read-write), and every organization they belong to with their role there \
                  (reviewer: reads and comments; developer: changes schemas; admin: also \
                  members and settings). Start here.",
    writes: false,
    destructive: false,
    idempotent: true,
    input: || schema::object(json!({}), &[]),
};

async fn whoami(ctx: &Ctx) -> Result<Output, ToolError> {
    let me: CurrentUser = ctx.api.get(&["me"], &[]).await?;
    let orgs: Vec<Org> = ctx.api.get(&["orgs"], &[]).await?;
    let token = &ctx.api.agent().token;
    let access = if token.can_write() {
        "read-write"
    } else {
        "read-only"
    };
    let (through, kind) = match token.kind {
        TokenKind::Personal => ("the access token", "personal"),
        TokenKind::App => ("the connected app", "app"),
    };
    let mut text = format!(
        "{} <{}>, through {through} \"{}\" ({access}).\nromulus: {}\n",
        me.name,
        me.email,
        token.name,
        ctx.link("")
    );
    if orgs.is_empty() {
        text.push_str("No organization yet: create one in romulus, or ask for an invitation.\n");
    } else {
        text.push_str("Organizations (slug: name, your role):\n");
        for org in &orgs {
            let _ = writeln!(
                text,
                "- {}: {}, {}{}",
                org.slug,
                org.name,
                role(org),
                rules(org)
            );
        }
    }
    // The same facts, for a program checking a connection.
    let structured = json!({
        "user": { "name": me.name, "email": me.email },
        "token": { "name": token.name, "access": access, "kind": kind },
        "organizations": orgs.iter().map(|org| json!({
            "slug": org.slug, "name": org.name, "role": role(org),
        })).collect::<Vec<_>>(),
    });
    Ok(Output {
        text,
        structured: Some(structured),
    })
}

fn role(org: &Org) -> String {
    serde_json::to_value(org.role)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// What publishing needs there, when it needs anything.
fn rules(org: &Org) -> String {
    let mut rules = Vec::new();
    if org.approvals_required > 0 {
        rules.push(format!(
            "publishing needs {} approval{}",
            org.approvals_required,
            plural(org.approvals_required)
        ));
    }
    if org.require_passing_test {
        rules.push("publishing needs a passing migration test".to_owned());
    }
    if rules.is_empty() {
        String::new()
    } else {
        format!(" ({})", rules.join("; "))
    }
}

fn plural(n: i32) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[derive(Deserialize)]
struct OrgArgs {
    #[serde(default)]
    org: Option<String>,
}

static LIST_DATABASES: Tool = Tool {
    name: "list_databases",
    title: "List databases",
    description: "The databases an organization keeps in romulus: slug, name, the Postgres \
                  major it targets, its latest version and when something last happened. Use \
                  a database's slug with the other tools.",
    writes: false,
    destructive: false,
    idempotent: true,
    input: || schema::object(json!({ "org": schema::org() }), &[]),
};

async fn list_databases(ctx: &Ctx, a: OrgArgs) -> Result<Output, ToolError> {
    let org = ctx.org(a.org.as_deref()).await?;
    let databases: Vec<Database> = ctx.api.get(&["orgs", &org, "databases"], &[]).await?;
    if databases.is_empty() {
        return Ok(Output::text(format!(
            "{org} has no database yet. create_database makes one; push_schema gives it its \
             first version."
        )));
    }
    let mut text = format!("Databases of {org} ({}):\n", databases.len());
    for database in &databases {
        let _ = writeln!(
            text,
            "- {}: {}, Postgres {}, {}",
            database.slug,
            database.name,
            database.pg_major,
            latest(database)
        );
    }
    Ok(Output::text(text))
}

fn latest(database: &Database) -> String {
    database.latest.as_ref().map_or_else(
        || "no version yet".to_owned(),
        |v| format!("latest v{} ({})", v.number, version_line(v)),
    )
}

/// `2026-10-19, push by "CI": add invoices`.
pub fn version_line(v: &VersionSummary) -> String {
    let origin = serde_json::to_value(v.origin)
        .ok()
        .and_then(|o| o.as_str().map(str::to_owned))
        .unwrap_or_default();
    let author = match &v.author {
        Some(crate::versions::Author::User { name, .. }) => format!(" by {name}"),
        Some(crate::versions::Author::Token { name, .. }) => format!(" by token \"{name}\""),
        None => String::new(),
    };
    let message = v
        .message
        .as_deref()
        .map(|m| format!(": {}", one_line(m, 120)))
        .unwrap_or_default();
    format!(
        "{}, {origin}{author}{message}",
        v.created_at.format("%Y-%m-%d")
    )
}

/// The first line of `text`, at most `max` characters.
pub fn one_line(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max).collect::<String>())
    } else {
        line.to_owned()
    }
}

#[derive(Deserialize)]
struct DatabaseArgs {
    #[serde(default)]
    org: Option<String>,
    database: String,
}

static GET_DATABASE: Tool = Tool {
    name: "get_database",
    title: "Get a database",
    description: "One database: the Postgres major it targets, its latest version, its open \
                  drafts (ids, who is editing, review state, whether they need updating) and \
                  what publishing needs in its organization (approvals, a passing migration \
                  test).",
    writes: false,
    destructive: false,
    idempotent: true,
    input: || {
        schema::object(
            json!({ "org": schema::org(), "database": schema::database() }),
            &["database"],
        )
    },
};

async fn get_database(ctx: &Ctx, a: DatabaseArgs) -> Result<Output, ToolError> {
    let org = ctx.org(a.org.as_deref()).await?;
    let database: Database = ctx
        .api
        .get(&["orgs", &org, "databases", &a.database], &[])
        .await?;
    let drafts: Vec<Draft> = ctx
        .api
        .get(&["orgs", &org, "databases", &a.database, "drafts"], &[])
        .await?;
    let settings: Org = ctx.api.get(&["orgs", &org], &[]).await?;
    let mut text = format!(
        "{org}/{}: {}, Postgres {}{}\n",
        database.slug,
        database.name,
        database.pg_major,
        if database.description.is_empty() {
            String::new()
        } else {
            format!(" — {}", one_line(&database.description, 200))
        }
    );
    let _ = writeln!(
        text,
        "{}; {} version{}.",
        latest(&database),
        database.versions,
        if database.versions == 1 { "" } else { "s" }
    );
    let _ = writeln!(text, "Your role: {}.{}", role(&settings), rules(&settings));
    if drafts.is_empty() {
        text.push_str("No open draft (create_draft starts one).\n");
    } else {
        let _ = writeln!(text, "Open drafts ({}):", drafts.len());
        for draft in &drafts {
            let _ = writeln!(text, "- {}", draft_line(draft));
        }
    }
    let _ = writeln!(
        text,
        "Database id (for CI pushes): {}\nPage: {}",
        database.id,
        ctx.link(&format!("{org}/{}", database.slug))
    );
    Ok(Output::text(text))
}

/// A draft in a line: id, name, mode, base, author, review, presence.
pub fn draft_line(draft: &Draft) -> String {
    let mode = match draft.mode {
        crate::domain::DraftMode::Schema => "schema SQL",
        crate::domain::DraftMode::Migration => "migration",
    };
    let base = draft
        .base_version
        .map_or_else(|| "no base version".to_owned(), |n| format!("from v{n}"));
    let by = draft
        .created_by
        .as_ref()
        .map(|p| format!(" by {}", p.name))
        .unwrap_or_default();
    let mut line = format!(
        "{} \"{}\" ({mode}, {base}{by}, edited {})",
        draft.id,
        draft.name,
        draft.updated_at.format("%Y-%m-%d %H:%M UTC")
    );
    if draft.review.required > 0 {
        let _ = write!(
            line,
            "; approvals {}/{}",
            draft.review.approvals, draft.review.required
        );
    }
    if draft.review.requested.is_some() {
        line.push_str("; review requested");
    }
    let present: Vec<_> = draft.present.iter().map(|p| p.name.as_str()).collect();
    if !present.is_empty() {
        let _ = write!(line, "; open now: {}", present.join(", "));
    }
    if draft.stale {
        line.push_str("; STALE: a newer version exists, update it in the web app first");
    }
    line
}

#[derive(Deserialize)]
struct SchemaArgs {
    #[serde(default)]
    org: Option<String>,
    database: String,
    #[serde(default)]
    version: Option<i32>,
    #[serde(default)]
    format: Option<String>,
}

static GET_SCHEMA: Tool = Tool {
    name: "get_schema",
    title: "Get a schema",
    description: "A version of a database's schema (the latest unless `version` is given). \
                  format `sql` (default): the DDL, as uploaded or rendered by remus. `summary`: \
                  one compact line per table (columns with types, keys, foreign keys) and per \
                  type, cheapest to read for a large schema. `json`: the full remus model (large).",
    writes: false,
    destructive: false,
    idempotent: true,
    input: || {
        schema::object(
            json!({
                "org": schema::org(),
                "database": schema::database(),
                "version": schema::version("The version's number; the latest when absent."),
                "format": { "type": "string", "enum": ["sql", "summary", "json"], "default": "sql" },
            }),
            &["database"],
        )
    },
};

/// The number of the database's latest version, or what to do without one.
pub async fn latest_number(ctx: &Ctx, org: &str, database: &str) -> Result<i32, ToolError> {
    let found: Database = ctx
        .api
        .get(&["orgs", org, "databases", database], &[])
        .await?;
    found.latest.map(|v| v.number).ok_or_else(|| {
        ToolError::Say(format!(
            "{org}/{database} has no version yet: push_schema gives it its first."
        ))
    })
}

async fn get_schema(ctx: &Ctx, a: SchemaArgs) -> Result<Output, ToolError> {
    let org = ctx.org(a.org.as_deref()).await?;
    let number = match a.version {
        Some(n) => n,
        None => latest_number(ctx, &org, &a.database).await?,
    };
    let n = number.to_string();
    let path = ["orgs", &org, "databases", &a.database, "versions", &n];
    match a.format.as_deref().unwrap_or("sql") {
        "sql" => {
            let texts: VersionTexts = ctx.api.get(&[&path[..], &["texts"]].concat(), &[]).await?;
            Ok(Output::text(format!(
                "-- {org}/{} v{number}\n{}",
                a.database, texts.sql
            )))
        }
        "json" => {
            let detail: VersionDetail = ctx.api.get(&path, &[]).await?;
            Ok(Output::text(detail.model.to_string()))
        }
        "summary" => {
            let detail: VersionDetail = ctx.api.get(&path, &[]).await?;
            let model: remus_core::Schema = serde_json::from_value(detail.model)
                .map_err(|err| ToolError::Say(format!("The model did not read: {err}")))?;
            Ok(Output::text(format!(
                "{org}/{} v{number}: {}",
                a.database,
                summary(&model)
            )))
        }
        other => Err(ToolError::Invalid(format!(
            "`format` is `sql`, `summary` or `json`, not `{other}`."
        ))),
    }
}

/// A model in a few lines: each table with its columns, keys and foreign
/// keys; each view, enum and domain.
pub fn summary(model: &remus_core::Schema) -> String {
    use remus_core::model::{ConstraintKind, EntityKind};
    let mut text = format!(
        "{} relations, {} enums, {} domains\n",
        model.entities.len(),
        model.enums.len(),
        model.domains.len()
    );
    for entity in &model.entities {
        let kind = match entity.kind {
            EntityKind::Table => "table",
            EntityKind::PartitionedTable => "partitioned table",
            EntityKind::View => "view",
            EntityKind::MaterializedView => "materialized view",
            EntityKind::ForeignTable => "foreign table",
        };
        let primary: Vec<&str> = entity
            .constraints
            .iter()
            .filter(|c| c.kind == ConstraintKind::PrimaryKey)
            .flat_map(|c| c.columns.iter().map(String::as_str))
            .collect();
        let columns: Vec<String> = entity
            .columns
            .iter()
            .map(|c| {
                let mut column = format!("{} {}", c.name, c.r#type);
                if primary.contains(&c.name.as_str()) {
                    column.push_str(" pk");
                } else if c.not_null {
                    column.push_str(" not null");
                }
                if c.identity.is_some() {
                    column.push_str(" identity");
                }
                if let Some(default) = &c.default {
                    let _ = write!(column, " default {}", one_line(default, 40));
                }
                column
            })
            .collect();
        let _ = write!(
            text,
            "{} {}.{}({})",
            kind,
            entity.schema,
            entity.name,
            columns.join(", ")
        );
        for constraint in &entity.constraints {
            match constraint.kind {
                ConstraintKind::ForeignKey => {
                    let _ = write!(
                        text,
                        "; fk ({}) → {}.{}({})",
                        constraint.columns.join(", "),
                        constraint.ref_schema.as_deref().unwrap_or(""),
                        constraint.ref_table.as_deref().unwrap_or(""),
                        constraint.ref_columns.join(", ")
                    );
                }
                ConstraintKind::Unique => {
                    let _ = write!(text, "; unique ({})", constraint.columns.join(", "));
                }
                _ => {}
            }
        }
        if !entity.indexes.is_empty() {
            let _ = write!(text, "; {} index(es)", entity.indexes.len());
        }
        if let Some(comment) = &entity.comment {
            let _ = write!(text, " -- {}", one_line(comment, 100));
        }
        text.push('\n');
    }
    for enumeration in &model.enums {
        let _ = writeln!(
            text,
            "enum {}.{}({})",
            enumeration.schema,
            enumeration.name,
            enumeration.labels.join(", ")
        );
    }
    for domain in &model.domains {
        let _ = writeln!(
            text,
            "domain {}.{} {}",
            domain.schema, domain.name, domain.base_type
        );
    }
    text
}

#[derive(Deserialize)]
struct VersionsArgs {
    #[serde(default)]
    org: Option<String>,
    database: String,
    #[serde(default)]
    before: Option<i32>,
    #[serde(default)]
    limit: Option<i64>,
}

static LIST_VERSIONS: Tool = Tool {
    name: "list_versions",
    title: "List versions",
    description: "A database's versions, newest first: number, date, how it came (upload, CI \
                  push, published draft, imported migration), who, and its message. Versions \
                  are immutable; numbers are never reused.",
    writes: false,
    destructive: false,
    idempotent: true,
    input: || {
        schema::object(
            json!({
                "org": schema::org(),
                "database": schema::database(),
                "before": schema::version("Versions numbered below this (the next page)."),
                "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
            }),
            &["database"],
        )
    },
};

async fn list_versions(ctx: &Ctx, a: VersionsArgs) -> Result<Output, ToolError> {
    let org = ctx.org(a.org.as_deref()).await?;
    let mut query = vec![("limit", a.limit.unwrap_or(20).clamp(1, 100).to_string())];
    if let Some(before) = a.before {
        query.push(("before", before.to_string()));
    }
    let page: VersionPage = ctx
        .api
        .get(
            &["orgs", &org, "databases", &a.database, "versions"],
            &query,
        )
        .await?;
    if page.versions.is_empty() {
        return Ok(Output::text(format!(
            "{org}/{} has no version yet: push_schema gives it its first.",
            a.database
        )));
    }
    let mut text = format!("Versions of {org}/{}:\n", a.database);
    for v in &page.versions {
        let _ = writeln!(text, "- v{}: {}", v.number, version_line(v));
    }
    if let Some(next) = page.next_before {
        let _ = writeln!(text, "More: call again with before={next}.");
    }
    Ok(Output::text(text))
}

#[derive(Deserialize)]
struct DiffArgs {
    #[serde(default)]
    org: Option<String>,
    database: String,
    from: i32,
    to: i32,
    #[serde(default = "yes")]
    sql: bool,
}

fn yes() -> bool {
    true
}

static DIFF_VERSIONS: Tool = Tool {
    name: "diff_versions",
    title: "Diff two versions",
    description: "What changed between two versions of a database (`from` → `to`, either \
                  order): a summary, one line per change (tables, columns, keys, indexes, \
                  policies, types), and each changed object's SQL before and after (`sql`: \
                  false to leave it out). Renames show as a drop and an add here; \
                  generate_migration detects them.",
    writes: false,
    destructive: false,
    idempotent: true,
    input: || {
        schema::object(
            json!({
                "org": schema::org(),
                "database": schema::database(),
                "from": schema::version("The version compared from."),
                "to": schema::version("The version compared to."),
                "sql": { "type": "boolean", "default": true, "description": "Include each changed object's SQL before and after." },
            }),
            &["database", "from", "to"],
        )
    },
};

#[derive(Deserialize)]
struct DiffView {
    from: VersionSummary,
    to: VersionSummary,
    diff: SchemaDiff,
}

/// Past this, a diff's SQL is cut (the change lines are always whole).
const DIFF_SQL_BUDGET: usize = 60_000;

async fn diff_versions(ctx: &Ctx, a: DiffArgs) -> Result<Output, ToolError> {
    let org = ctx.org(a.org.as_deref()).await?;
    let view: DiffView = ctx
        .api
        .get(
            &["orgs", &org, "databases", &a.database, "diff"],
            &[("from", a.from.to_string()), ("to", a.to.to_string())],
        )
        .await?;
    let mut text = format!(
        "{org}/{} v{} → v{}: {}\n",
        a.database, view.from.number, view.to.number, view.diff.summary.text
    );
    text.push_str(&diff_text(&view.diff, a.sql));
    let _ = writeln!(
        text,
        "Page: {}",
        ctx.link(&format!(
            "{org}/{}/diff/{}...{}",
            a.database, view.from.number, view.to.number
        ))
    );
    Ok(Output::text(text))
}

/// A diff's change lines, by object, with their SQL when asked (cut past a
/// budget).
pub fn diff_text(diff: &SchemaDiff, with_sql: bool) -> String {
    let mut text = String::new();
    let mut budget = DIFF_SQL_BUDGET;
    let mut cut = false;
    let mut object =
        |label: String, lines: &[String], sql: Option<&romulus_diff::Sides>, text: &mut String| {
            if lines.is_empty() {
                return;
            }
            let _ = writeln!(text, "{label}:");
            for line in lines {
                let _ = writeln!(text, "  - {line}");
            }
            if let (true, Some(sql)) = (with_sql, sql) {
                for (side, body) in [("before", &sql.before), ("after", &sql.after)] {
                    let Some(body) = body else { continue };
                    if body.len() > budget {
                        cut = true;
                        continue;
                    }
                    budget -= body.len();
                    let _ = writeln!(text, "  -- {side}:\n{}", indent(body));
                }
            }
        };
    for entity in &diff.entities {
        object(
            format!("{}.{}", entity.schema, entity.name),
            &entity.lines,
            Some(&entity.sql),
            &mut text,
        );
    }
    for e in &diff.enums {
        object(
            format!("enum {}.{}", e.schema, e.name),
            &e.lines,
            None,
            &mut text,
        );
    }
    for d in &diff.domains {
        object(
            format!("domain {}.{}", d.schema, d.name),
            &d.lines,
            None,
            &mut text,
        );
    }
    for c in &diff.composites {
        object(
            format!("type {}.{}", c.schema, c.name),
            &c.lines,
            None,
            &mut text,
        );
    }
    for x in &diff.extensions {
        object(format!("extension {}", x.name), &x.lines, None, &mut text);
    }
    object(
        "partitions without their table".into(),
        &diff.orphan_partitions.lines,
        None,
        &mut text,
    );
    if cut {
        text.push_str("(Some SQL was left out to keep this short: get_schema has it whole.)\n");
    }
    text
}

fn indent(body: &str) -> String {
    body.trim_end()
        .lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    org: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

static SEARCH_SCHEMA: Tool = Tool {
    name: "search_schema",
    title: "Search schemas",
    description: "Finds tables (and views), columns and enums whose name or comment contains \
                  `query` (any case), in the latest version of every database of an \
                  organization, or of all yours when `org` is not given. Exact names first.",
    writes: false,
    destructive: false,
    idempotent: true,
    input: || {
        schema::object(
            json!({
                "query": { "type": "string", "minLength": 1, "maxLength": 100, "description": "Part of a name or comment." },
                "org": { "type": "string", "description": "An organization's slug; all of yours when absent." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 },
            }),
            &["query"],
        )
    },
};

async fn search_schema(ctx: &Ctx, a: SearchArgs) -> Result<Output, ToolError> {
    let orgs: Vec<String> =
        if let Some(org) = a.org.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            vec![org.to_owned()]
        } else {
            let orgs: Vec<Org> = ctx.api.get(&["orgs"], &[]).await?;
            orgs.into_iter().map(|o| o.slug).collect()
        };
    let limit = a.limit.unwrap_or(50).clamp(1, 200);
    let mut text = String::new();
    let mut found = 0;
    for org in &orgs {
        let results: SearchResults = ctx
            .api
            .get(
                &["orgs", org, "search"],
                &[("q", a.query.clone()), ("limit", limit.to_string())],
            )
            .await?;
        for hit in &results.hits {
            found += 1;
            let at = match &hit.column {
                Some(column) => format!("{}.{}.{column}", hit.schema, hit.name),
                None => format!("{}.{}", hit.schema, hit.name),
            };
            let _ = write!(
                text,
                "- {org}/{} v{}: {} {at}",
                hit.database, hit.version, hit.kind
            );
            if let Some(ty) = &hit.r#type {
                let _ = write!(text, " {}", one_line(ty, 80));
            }
            if let Some(comment) = &hit.comment {
                let _ = write!(text, " -- {}", one_line(comment, 120));
            }
            text.push('\n');
        }
        if results.truncated {
            let _ = writeln!(text, "(more in {org}: narrow the query or raise `limit`)");
        }
    }
    if found == 0 {
        return Ok(Output::text(format!("Nothing matches \"{}\".", a.query)));
    }
    Ok(Output::text(format!(
        "{found} match{} for \"{}\":\n{text}",
        if found == 1 { "" } else { "es" },
        a.query
    )))
}

/// Compile diagnostics, one line each (at most `max`).
pub fn diagnostics(list: &[Value], max: usize) -> String {
    let mut text = String::new();
    for diagnostic in list.iter().take(max) {
        let severity = diagnostic["severity"].as_str().unwrap_or("error");
        let at = diagnostic["span"]["from"]
            .as_object()
            .map(|from| {
                format!(
                    "line {}:{} ",
                    from.get("line").and_then(Value::as_u64).unwrap_or(0),
                    from.get("column").and_then(Value::as_u64).unwrap_or(0)
                )
            })
            .unwrap_or_default();
        let _ = write!(
            text,
            "- {at}{severity}: {}",
            diagnostic["message"].as_str().unwrap_or("")
        );
        for key in ["detail", "hint"] {
            if let Some(more) = diagnostic[key].as_str() {
                let _ = write!(text, " ({key}: {more})");
            }
        }
        text.push('\n');
    }
    if list.len() > max {
        let _ = writeln!(text, "… and {} more.", list.len() - max);
    }
    text
}
