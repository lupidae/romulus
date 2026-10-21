//! Data: playgrounds (a real Postgres holding a version's schema, filled
//! with generated rows) and read-only queries on them, as the console runs
//! them.

use std::{fmt::Write as _, time::Duration};

use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    Audit, Ctx, Output, Target, Tool, ToolError, args, playground_place, read::one_line, schema,
    uuid,
};
use crate::{
    audit::AuditDetails,
    domain::PlaygroundStatus,
    playgrounds::{Playground, query::QueryResult},
};

/// How long `create_playground` waits for the build (then: poll).
const MAX_WAIT: Duration = Duration::from_secs(40);
const POLL: Duration = Duration::from_secs(2);
/// Characters of a value shown.
const CELL: usize = 200;

pub const TOOLS: &[&Tool] = &[&CREATE_PLAYGROUND, &PLAYGROUND_QUERY, &LIST_PLAYGROUNDS];

pub fn handles(name: &str) -> bool {
    TOOLS.iter().any(|tool| tool.name == name)
}

pub async fn call(ctx: &Ctx, name: &str, arguments: Value) -> Result<Output, ToolError> {
    match name {
        "create_playground" => create_playground(ctx, args(arguments)?).await,
        "playground_query" => playground_query(ctx, args(arguments)?).await,
        _ => list_playgrounds(ctx, args(arguments)?).await,
    }
}

#[derive(Deserialize)]
struct CreateArgs {
    #[serde(default)]
    org: Option<String>,
    database: String,
    #[serde(default)]
    version: Option<i32>,
    #[serde(default)]
    draft: Option<String>,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    wait_seconds: Option<u64>,
}

static CREATE_PLAYGROUND: Tool = Tool {
    name: "create_playground",
    title: "Create a playground",
    description: "Changes shared state: builds a playground, a real Postgres holding a \
                  version's schema (the latest unless `version`; a draft's base with `draft`) \
                  filled with generated rows, that the organization can query read-only \
                  (playground_query) and run migrations on. One per version or draft; it \
                  expires after days unused; caps per person and organization apply. Waits up \
                  to `wait_seconds` (40 at most) for the build.",
    writes: true,
    destructive: false,
    idempotent: false,
    input: || {
        schema::object(
            json!({
                "org": schema::org(),
                "database": schema::database(),
                "version": schema::version("The version whose schema it holds; the latest when absent."),
                "draft": { "type": "string", "format": "uuid", "description": "For this draft: it holds the draft's base version." },
                "size": { "type": "string", "enum": ["small", "medium"], "default": "small" },
                "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 40, "default": 40 },
            }),
            &["database"],
        )
    },
};

async fn create_playground(ctx: &Ctx, a: CreateArgs) -> Result<Output, ToolError> {
    let org = ctx.org(a.org.as_deref()).await?;
    let mut body = json!({});
    if let Some(version) = a.version {
        body["version"] = json!(version);
    }
    if let Some(draft) = &a.draft {
        body["draft_id"] = json!(uuid(draft, "draft")?);
    }
    if let Some(size) = &a.size {
        body["size"] = json!(size);
    }
    let base = [
        "orgs",
        org.as_str(),
        "databases",
        a.database.as_str(),
        "playgrounds",
    ];
    let mut playground: Playground = ctx.api.post(&base, &body).await?;
    ctx.audit(Audit {
        org: org.clone(),
        database: Some(Target::Slug(a.database.clone())),
        version: Some(playground.version),
        outcome: "created a playground".into(),
        details: AuditDetails {
            playground_id: Some(playground.id),
            ..AuditDetails::default()
        },
    })
    .await;
    let id = playground.id.to_string();
    let wait = ctx.wait_budget(Duration::from_secs(a.wait_seconds.unwrap_or(40)), MAX_WAIT);
    let deadline = tokio::time::Instant::now() + wait;
    while playground.status == PlaygroundStatus::Building
        && tokio::time::Instant::now() + POLL <= deadline
    {
        tokio::time::sleep(POLL).await;
        // Created: a failed read answers what it was (its id), not an error.
        match ctx
            .api
            .get(&[&base[..], &[id.as_str()]].concat(), &[])
            .await
        {
            Ok(now) => playground = now,
            Err(_) => break,
        }
    }
    Ok(Output::text(playground_text(
        ctx,
        &org,
        &a.database,
        &playground,
    )))
}

fn playground_text(ctx: &Ctx, org: &str, database: &str, p: &Playground) -> String {
    let status = match p.status {
        PlaygroundStatus::Building => {
            let progress = p.progress.as_ref().map_or_else(String::new, |b| {
                format!(" ({} of {} tables)", b.tables_done, b.tables_total)
            });
            format!("building{progress}: call list_playgrounds in a moment, or query it once ready")
        }
        PlaygroundStatus::Ready => "ready".into(),
        PlaygroundStatus::Failed => format!(
            "failed: {}",
            p.failure.as_deref().unwrap_or("the build failed")
        ),
        PlaygroundStatus::Expired => "expired".into(),
    };
    let mut text = format!(
        "Playground {} of {org}/{database}, v{}{}: {status}.\n",
        p.id,
        p.version,
        p.draft
            .as_ref()
            .map(|d| format!(" (draft \"{}\")", d.name))
            .unwrap_or_default(),
    );
    if !p.tables.is_empty() {
        let rows: Vec<String> = p
            .tables
            .iter()
            .take(30)
            .map(|t| format!("{} {}", t.table, t.rows))
            .collect();
        let _ = writeln!(text, "Rows: {}", rows.join(", "));
    }
    if p.migrated_by.is_some() {
        text.push_str("It holds the rows a migration run left (not the generated ones).\n");
    }
    let _ = writeln!(
        text,
        "Expires {} unless used.\nPage: {}",
        p.expires_at.format("%Y-%m-%d"),
        ctx.link(&format!("{org}/{database}/playgrounds/{}", p.id))
    );
    text
}

#[derive(Deserialize)]
struct QueryArgs {
    playground: String,
    sql: String,
    #[serde(default)]
    max_rows: Option<usize>,
    #[serde(default)]
    explain: bool,
}

static PLAYGROUND_QUERY: Tool = Tool {
    name: "playground_query",
    title: "Query a playground",
    description: "Runs SQL on a playground, read-only (the console's own read-only role: \
                  SELECT, EXPLAIN, catalog queries; writes are refused by Postgres), in one \
                  transaction rolled back after. Answers each statement's columns and first \
                  rows (`max_rows`, 100 by default). `explain`: the plan of one statement, \
                  analyzed. Runs are kept in the playground's shared history.",
    writes: false,
    destructive: false,
    idempotent: true,
    input: || {
        schema::object(
            json!({
                "playground": { "type": "string", "format": "uuid", "description": "The playground's id (list_playgrounds)." },
                "sql": { "type": "string", "description": "One or more statements." },
                "max_rows": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 },
                "explain": { "type": "boolean", "default": false },
            }),
            &["playground", "sql"],
        )
    },
};

async fn playground_query(ctx: &Ctx, a: QueryArgs) -> Result<Output, ToolError> {
    let id = uuid(&a.playground, "playground")?;
    let place = playground_place(ctx, id).await?;
    let id = id.to_string();
    let result: QueryResult = ctx
        .api
        .post_reading(
            &[
                "orgs",
                &place.org,
                "databases",
                &place.database,
                "playgrounds",
                &id,
                "query",
            ],
            &json!({ "sql": a.sql, "max_rows": a.max_rows, "explain": a.explain }),
        )
        .await?;
    Ok(Output::text(query_text(&result)))
}

/// A run's results: per statement, its tag and rows as tab-separated lines.
fn query_text(result: &QueryResult) -> String {
    let mut text = String::new();
    for statement in &result.statements {
        let _ = write!(text, "Statement {}", statement.index + 1);
        if let Some(tag) = &statement.tag {
            let _ = write!(text, ": {tag}");
        }
        if let Some(ms) = statement.ms {
            let _ = write!(text, " ({ms} ms)");
        }
        text.push('\n');
        if let Some(error) = &statement.error {
            let _ = writeln!(text, "ERROR {}: {}", error.sqlstate, error.message);
            if let Some(detail) = &error.detail {
                let _ = writeln!(text, "DETAIL: {detail}");
            }
            if let Some(hint) = &error.hint {
                let _ = writeln!(text, "HINT: {hint}");
            }
        }
        if let Some(plan) = &statement.plan {
            let _ = writeln!(text, "{}", plan.trim_end());
        }
        for notice in &statement.notices {
            let _ = writeln!(text, "{notice}");
        }
        if !statement.columns.is_empty() {
            let header: Vec<String> = statement
                .columns
                .iter()
                .map(|c| format!("{} ({})", c.name, c.type_name))
                .collect();
            let _ = writeln!(text, "{}", header.join("\t"));
        }
        for row in &statement.rows {
            let cells: Vec<String> = row
                .iter()
                .map(|cell| {
                    cell.as_deref().map_or_else(
                        || "NULL".to_owned(),
                        |value| one_line(&value.replace('\t', " "), CELL),
                    )
                })
                .collect();
            let _ = writeln!(text, "{}", cells.join("\t"));
        }
        if statement.truncated {
            let _ = writeln!(
                text,
                "({} rows in all; {} shown)",
                statement.row_count,
                statement.rows.len()
            );
        }
    }
    if let Some(error) = &result.error {
        let _ = writeln!(text, "{error}");
    }
    if text.is_empty() {
        text = format!("Nothing ran ({:?}).", result.status);
    }
    text
}

#[derive(Deserialize)]
struct ListArgs {
    #[serde(default)]
    org: Option<String>,
    database: String,
}

static LIST_PLAYGROUNDS: Tool = Tool {
    name: "list_playgrounds",
    title: "List playgrounds",
    description: "A database's playgrounds: id, the version they hold, whether they are ready, \
                  their rows per table, and when they expire.",
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

async fn list_playgrounds(ctx: &Ctx, a: ListArgs) -> Result<Output, ToolError> {
    let org = ctx.org(a.org.as_deref()).await?;
    let playgrounds: Vec<Playground> = ctx
        .api
        .get(
            &["orgs", &org, "databases", &a.database, "playgrounds"],
            &[],
        )
        .await?;
    if playgrounds.is_empty() {
        return Ok(Output::text(format!(
            "{org}/{} has no playground: create_playground builds one.",
            a.database
        )));
    }
    let text: String = playgrounds
        .iter()
        .map(|p| playground_text(ctx, &org, &a.database, p))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Output::text(text))
}
