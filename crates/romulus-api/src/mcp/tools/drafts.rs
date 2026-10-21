//! Changing a schema: drafts, their migration, review and publishing, with
//! the web app's rules (approvals, the passing-test gate, stale bases).
//! Nothing here overrides governance: no approving, no publishing without
//! what the organization requires.

use std::{collections::BTreeMap, fmt::Write as _};

use romulus_diff::rename::Candidate;
use romulus_migration::{File, Hazard, Statement, Warning};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    Audit, Ctx, Output, Target, Tool, ToolError, args, draft_place,
    read::{diagnostics, draft_line, one_line},
    schema, uuid,
};
use crate::{
    audit::AuditDetails,
    drafts::{Draft, text::DraftText},
    mcp::api::Failure,
    versions::VersionTexts,
};

pub const TOOLS: &[&Tool] = &[
    &CREATE_DRAFT,
    &UPDATE_DRAFT,
    &GET_DRAFT,
    &GENERATE_MIGRATION,
    &CONFIRM_RENAMES,
    &REQUEST_REVIEW,
    &PUBLISH_DRAFT,
];

pub fn handles(name: &str) -> bool {
    TOOLS.iter().any(|tool| tool.name == name)
}

pub async fn call(ctx: &Ctx, name: &str, arguments: Value) -> Result<Output, ToolError> {
    match name {
        "create_draft" => create_draft(ctx, args(arguments)?).await,
        "update_draft" => update_draft(ctx, args(arguments)?).await,
        "get_draft" => get_draft(ctx, args(arguments)?).await,
        "generate_migration" => generate_migration(ctx, args(arguments)?).await,
        "confirm_renames" => confirm_renames(ctx, args(arguments)?).await,
        "request_review" => request_review(ctx, args(arguments)?).await,
        _ => publish_draft(ctx, args(arguments)?).await,
    }
}

fn edits_schema() -> Value {
    json!({
        "type": "array",
        "maxItems": 100,
        "description": "Edits applied in order to the text as it is: each replaces `old` (exact text, found exactly once) with `new`; an empty `old` appends `new` at the end. Cheaper than resending a large text.",
        "items": {
            "type": "object",
            "properties": {
                "old": { "type": "string", "description": "Exact text to replace; empty to append." },
                "new": { "type": "string" },
            },
            "required": ["new"],
            "additionalProperties": false,
        },
    })
}

#[derive(Deserialize, Clone, serde::Serialize)]
struct TextEdit {
    #[serde(default)]
    old: String,
    new: String,
}

#[derive(Deserialize)]
struct CreateArgs {
    #[serde(default)]
    org: Option<String>,
    database: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    from_version: Option<i32>,
    #[serde(default)]
    sql: Option<String>,
    #[serde(default)]
    edits: Option<Vec<TextEdit>>,
}

static CREATE_DRAFT: Tool = Tool {
    name: "create_draft",
    title: "Create a draft",
    description: "Changes shared state: starts a draft of a database, the next version in the \
                  making, which everyone in the organization sees and can open live. It starts \
                  from the latest version's SQL (mode `schema`), or empty for a hand-written \
                  migration (mode `migration`). Then optionally: `from_version` copies an older \
                  version's SQL in (bringing it back), `sql` replaces the whole text, `edits` \
                  change parts of it. Answers the draft's id and whether its SQL compiles.",
    writes: true,
    destructive: false,
    idempotent: false,
    input: || {
        schema::object(
            json!({
                "org": schema::org(),
                "database": schema::database(),
                "name": { "type": "string", "maxLength": 200, "description": "What it changes: \"Add refunds\". By default \"Draft from vN\"." },
                "mode": { "type": "string", "enum": ["schema", "migration"], "default": "schema", "description": "`schema`: the text is the whole schema's SQL. `migration`: the text is a migration (ALTER …) from the latest version." },
                "from_version": schema::version("Start from this version's SQL instead of the latest's (schema mode)."),
                "sql": { "type": "string", "description": "The whole text: the schema's SQL (schema mode) or the migration (migration mode)." },
                "edits": edits_schema(),
            }),
            &["database"],
        )
    },
};

async fn create_draft(ctx: &Ctx, a: CreateArgs) -> Result<Output, ToolError> {
    if a.sql.is_some() && a.edits.is_some() {
        return Err(ToolError::Invalid(
            "send `sql` or `edits`, not both.".into(),
        ));
    }
    let migration = match a.mode.as_deref() {
        None | Some("schema") => false,
        Some("migration") => true,
        Some(other) => {
            return Err(ToolError::Invalid(format!(
                "`mode` is `schema` or `migration`, not `{other}`."
            )));
        }
    };
    if migration && a.from_version.is_some() {
        return Err(ToolError::Invalid(
            "`from_version` copies a schema's SQL: it goes with mode `schema`.".into(),
        ));
    }
    let org = ctx.org(a.org.as_deref()).await?;
    let base = ["orgs", org.as_str(), "databases", a.database.as_str()];
    let mut draft: Draft = ctx
        .api
        .post(
            &[&base[..], &["drafts"]].concat(),
            &json!({ "mode": if migration { "migration" } else { "schema" } }),
        )
        .await?;
    let id = draft.id.to_string();
    let path = [&base[..], &["drafts", id.as_str()]].concat();
    // Named before it is audited, so the Activity says which it is; a name
    // refused leaves the default one.
    let named = match a.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => ctx
            .api
            .patch::<Draft>(&path, &json!({ "name": name }))
            .await
            .map(|renamed| draft = renamed),
        None => Ok(()),
    };
    ctx.audit(Audit {
        org: org.clone(),
        database: Some(Target::Id(draft.database.id)),
        version: None,
        outcome: format!("created draft \"{}\"", draft.name),
        details: AuditDetails {
            draft_id: Some(draft.id),
            draft_name: Some(draft.name.clone()),
            ..AuditDetails::default()
        },
    })
    .await;
    // From here the draft exists: a failure says so, and how to go on with
    // it (a new create_draft would make a second one).
    let half_made = |err: ToolError| {
        ToolError::Say(format!(
            "The draft {} \"{}\" was created in {org}/{}, but its text was not written: {}\n\
             Go on with this draft (update_draft with draft {}), or have it deleted in the web \
             app ({}); do not call create_draft again.",
            draft.id,
            draft.name,
            a.database,
            err.text(),
            draft.id,
            draft_link(ctx, &org, &a.database, &draft),
        ))
    };
    named.map_err(|err| half_made(err.into()))?;
    let written = first_text(ctx, &base, &path, a.from_version, a.sql, a.edits)
        .await
        .map_err(|err| half_made(err.into()))?;
    let mut out = format!(
        "Created draft {} \"{}\" in {org}/{} ({}, {}).\n",
        draft.id,
        draft.name,
        a.database,
        if migration { "migration" } else { "schema SQL" },
        draft
            .base_version
            .map_or_else(|| "no base version".into(), |n| format!("from v{n}")),
    );
    out.push_str(&compile_text(&written));
    let _ = writeln!(
        out,
        "Text sha256: {} (send it as expected_sha256 with update_draft).\nPage: {}\nNext: \
         get_draft to read it, update_draft to change it, generate_migration and \
         test_migration to check it.",
        written.sha256,
        draft_link(ctx, &org, &a.database, &draft)
    );
    Ok(Output::text(out))
}

/// A new draft's text, written: `sql`, or `edits` (on `from_version`'s SQL
/// when given), or that version's SQL, or as it was created.
async fn first_text(
    ctx: &Ctx,
    base: &[&str],
    path: &[&str],
    from_version: Option<i32>,
    sql: Option<String>,
    edits: Option<Vec<TextEdit>>,
) -> Result<DraftText, Failure> {
    let text_path = [path, &["text"]].concat();
    let mut text = None;
    if let Some(number) = from_version {
        let n = number.to_string();
        let texts: VersionTexts = ctx
            .api
            .get(&[base, &["versions", n.as_str(), "texts"]].concat(), &[])
            .await?;
        text = Some(texts.sql);
    }
    let mut body = json!({ "wait": true });
    match (sql, edits, text) {
        (Some(sql), _, _) => body["text"] = json!(sql),
        (None, Some(edits), Some(start)) => {
            // The older version's SQL first, then the edits on it.
            let _: DraftText = ctx.api.put(&text_path, &json!({ "text": start })).await?;
            body["edits"] = json!(edits);
        }
        (None, Some(edits), None) => body["edits"] = json!(edits),
        (None, None, Some(start)) => body["text"] = json!(start),
        (None, None, None) => {
            return ctx.api.get(&text_path, &[("wait", "true".into())]).await;
        }
    }
    ctx.api.put(&text_path, &body).await
}

fn draft_link(ctx: &Ctx, org: &str, database: &str, draft: &Draft) -> String {
    ctx.link(&format!("{org}/{database}/drafts/{}", draft.id))
}

/// Where the compile stands, and its problems.
fn compile_text(text: &DraftText) -> String {
    let problems = text
        .compile
        .diagnostics
        .as_array()
        .cloned()
        .unwrap_or_default();
    let errors = problems.iter().filter(|d| d["severity"] == "error").count();
    let state = match (text.compile.state.as_str(), text.compile.current) {
        ("ok", true) => "compiles".to_owned(),
        ("failed", true) => format!(
            "does NOT compile ({errors} error{})",
            if errors == 1 { "" } else { "s" }
        ),
        ("unavailable", _) => "not checked: the compiler is unavailable right now".to_owned(),
        _ => "not compiled yet (try get_draft in a few seconds)".to_owned(),
    };
    let mut out = format!("Its SQL {state}.\n");
    if text.compile.current && !problems.is_empty() {
        out.push_str(&diagnostics(&problems, 20));
    }
    out
}

#[derive(Deserialize)]
struct UpdateArgs {
    draft: String,
    #[serde(default)]
    sql: Option<String>,
    #[serde(default)]
    edits: Option<Vec<TextEdit>>,
    #[serde(default)]
    expected_sha256: Option<String>,
}

static UPDATE_DRAFT: Tool = Tool {
    name: "update_draft",
    title: "Update a draft",
    description: "Changes shared state: changes a draft's text (its schema SQL, or its \
                  migration in migration mode), live for everyone who has it open, as your \
                  edit (approvals of the old text stop counting). Send `sql` (the whole text) \
                  or `edits` (exact replacements; cheaper). With `expected_sha256` (from \
                  get_draft or the last update), it is refused if someone else changed the \
                  draft meanwhile, instead of overwriting their work. Answers whether it \
                  compiles, with each problem's line.",
    writes: true,
    destructive: true,
    idempotent: false,
    input: || {
        schema::object(
            json!({
                "draft": schema::draft(),
                "sql": { "type": "string", "description": "The whole new text." },
                "edits": edits_schema(),
                "expected_sha256": { "type": "string", "description": "The text's sha256 as you read it: refused if it changed since." },
            }),
            &["draft"],
        )
    },
};

async fn update_draft(ctx: &Ctx, a: UpdateArgs) -> Result<Output, ToolError> {
    let id = uuid(&a.draft, "draft")?;
    let place = draft_place(ctx, id).await?;
    let mut body = json!({ "wait": true });
    match (a.sql, a.edits) {
        (Some(sql), None) => body["text"] = json!(sql),
        (None, Some(edits)) => body["edits"] = json!(edits),
        _ => {
            return Err(ToolError::Invalid(
                "send either `sql` (the whole text) or `edits`.".into(),
            ));
        }
    }
    if let Some(sha) = a.expected_sha256 {
        body["sha256"] = json!(sha);
    }
    let id = id.to_string();
    let path = [
        "orgs",
        place.org.as_str(),
        "databases",
        place.database.as_str(),
        "drafts",
        id.as_str(),
    ];
    let written: DraftText = ctx.api.put(&[&path[..], &["text"]].concat(), &body).await?;
    ctx.audit(Audit {
        org: place.org.clone(),
        database: Some(Target::Id(place.database_id)),
        version: None,
        outcome: "edited a draft's text".into(),
        details: AuditDetails {
            draft_id: uuid(&id, "draft").ok(),
            ..AuditDetails::default()
        },
    })
    .await;
    let mut out = format!("Updated draft {id}. ");
    out.push_str(&compile_text(&written));
    let _ = writeln!(
        out,
        "Text sha256: {}\nPage: {}",
        written.sha256,
        ctx.link(&format!("{}/{}/drafts/{id}", place.org, place.database))
    );
    Ok(Output::text(out))
}

#[derive(Deserialize)]
struct GetArgs {
    draft: String,
    #[serde(default = "yes")]
    text: bool,
    #[serde(default = "yes")]
    migration: bool,
}

fn yes() -> bool {
    true
}

static GET_DRAFT: Tool = Tool {
    name: "get_draft",
    title: "Get a draft",
    description: "A draft: its text (`text`: false to leave it out), whether it compiles with \
                  each problem's line, its review state (approvals, review requested, who has \
                  it open), and the migration it generates from its base version, with every \
                  statement that can lose data, fail on existing rows or widen access flagged \
                  (`migration`: false to leave it out). Its reviewed_sha256 is what \
                  publish_draft takes.",
    writes: false,
    destructive: false,
    idempotent: true,
    input: || {
        schema::object(
            json!({
                "draft": schema::draft(),
                "text": { "type": "boolean", "default": true },
                "migration": { "type": "boolean", "default": true },
            }),
            &["draft"],
        )
    },
};

async fn get_draft(ctx: &Ctx, a: GetArgs) -> Result<Output, ToolError> {
    let id = uuid(&a.draft, "draft")?;
    let place = draft_place(ctx, id).await?;
    let id = id.to_string();
    let path = [
        "orgs",
        place.org.as_str(),
        "databases",
        place.database.as_str(),
        "drafts",
        id.as_str(),
    ];
    let draft: Draft = ctx.api.get(&path, &[]).await?;
    let text: DraftText = ctx
        .api
        .get(&[&path[..], &["text"]].concat(), &[("wait", "true".into())])
        .await?;
    let mut out = format!(
        "In {}/{}: {}\nPublishing it makes v{}.\n",
        place.org,
        place.database,
        draft_line(&draft),
        draft.next_version
    );
    out.push_str(&compile_text(&text));
    let _ = writeln!(
        out,
        "reviewed_sha256: {}\nPage: {}",
        text.reviewed_sha256,
        ctx.link(&format!("{}/{}/drafts/{id}", place.org, place.database))
    );
    if a.text {
        let what = match draft.mode {
            crate::domain::DraftMode::Schema => "schema SQL",
            crate::domain::DraftMode::Migration => "migration",
        };
        let _ = write!(
            out,
            "\n--- the draft's {what} (sha256 {}) ---\n{}\n--- end ---\n",
            text.sha256,
            text.text.trim_end()
        );
        if let Some(schema_sql) = &text.schema_sql {
            let _ = write!(
                out,
                "\n--- the schema it makes (read-only) ---\n{}\n--- end ---\n",
                schema_sql.trim_end()
            );
        }
    }
    let compiles = text.compile.state == "ok" && text.compile.current;
    if a.migration && compiles && !draft.stale {
        let migration: MigrationView = ctx
            .api
            .get(
                &[&path[..], &["migration"]].concat(),
                &[("sha256", text.reviewed_sha256.clone())],
            )
            .await?;
        out.push('\n');
        out.push_str(&migration_text(&migration, false));
    } else if a.migration && draft.stale {
        out.push_str(
            "\nNo migration: a newer version was published since the draft started. Update \
             the draft in the web app (Update draft) first.\n",
        );
    } else if a.migration {
        out.push_str("\nNo migration until the text compiles.\n");
    }
    Ok(Output::text(out))
}

/// A migration, as the API answers it.
#[derive(Deserialize)]
pub struct MigrationView {
    pub from: Option<i32>,
    pub to: i32,
    pub statements: Vec<Statement>,
    pub warnings: Vec<Warning>,
    pub destructive: usize,
    pub files: Vec<File>,
    pub renames: Vec<Candidate>,
}

/// A migration for an agent: its statements with what each can do, the
/// renames considered, the warnings, and the files when asked.
pub fn migration_text(m: &MigrationView, files: bool) -> String {
    let from = m
        .from
        .map_or_else(|| "nothing".to_owned(), |n| format!("v{n}"));
    let mut out = format!(
        "Migration {from} → v{}: {} statement{}, {} that can lose data or fail on existing rows.\n",
        m.to,
        m.statements.len(),
        if m.statements.len() == 1 { "" } else { "s" },
        m.destructive
    );
    if m.statements.is_empty() {
        out.push_str("Nothing to migrate: the schemas are the same.\n");
    }
    for (i, statement) in m.statements.iter().enumerate() {
        let hazard = match statement.hazard {
            Some(Hazard::DeletesData) => " [DELETES DATA]",
            Some(Hazard::CanFail) => " [CAN FAIL on existing rows]",
            Some(Hazard::WidensAccess) => " [WIDENS ACCESS]",
            None => "",
        };
        let _ = writeln!(out, "{}.{hazard} {}", i + 1, statement.sql.trim());
        if let Some(note) = &statement.note {
            let _ = writeln!(out, "   note: {}", one_line(note, 300));
        }
        if let Some(alternative) = &statement.alternative {
            let _ = writeln!(out, "   instead (a rename): {}", alternative.trim());
        }
    }
    for warning in &m.warnings {
        let _ = writeln!(out, "Warning: {}", warning.message);
    }
    let offered: Vec<&Candidate> = m.renames.iter().filter(|c| c.follows.is_none()).collect();
    if !offered.is_empty() {
        out.push_str("Renames considered (turn one on or off with its key):\n");
        for c in offered {
            let _ = writeln!(
                out,
                "- {} {} ({:?}, score {}): {}. key: {}",
                if c.applied { "APPLIED" } else { "not applied" },
                c.rename.describe(),
                c.confidence,
                c.score,
                c.reason,
                c.key
            );
        }
    }
    if files {
        for file in &m.files {
            let _ = write!(out, "\n--- file {} ---\n{}", file.path, file.content);
            if !file.content.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    out
}

#[derive(Deserialize)]
struct MigrationArgs {
    #[serde(default)]
    draft: Option<String>,
    #[serde(default)]
    org: Option<String>,
    #[serde(default)]
    database: Option<String>,
    #[serde(default)]
    from: Option<i32>,
    #[serde(default)]
    to: Option<i32>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    renames: Option<BTreeMap<String, bool>>,
    #[serde(default)]
    concurrently: bool,
}

static GENERATE_MIGRATION: Tool = Tool {
    name: "generate_migration",
    title: "Generate a migration",
    description: "The migration (ALTER … in dependency order) from a draft's base version to \
                  the draft (`draft`), or between two versions (`database`, `from`, `to`; `to` \
                  may be older: a rollback), as the files of a migration tool (`format`), with \
                  a down migration. Every statement that can lose data, fail on existing rows \
                  or widen access is flagged. Renames are detected (a rename keeps the data a \
                  drop and an add would lose) and listed with a key: `renames` turns them on \
                  or off for this answer; for a draft, confirm_renames keeps the choice.",
    writes: false,
    destructive: false,
    idempotent: true,
    input: || {
        schema::object(
            json!({
                "draft": schema::draft(),
                "org": schema::org(),
                "database": schema::database(),
                "from": schema::version("The version the database is at (with `to`, instead of `draft`)."),
                "to": schema::version("The version to migrate it to."),
                "format": { "type": "string", "enum": ["plain", "sqlx", "flyway", "prisma", "dbmate", "golang_migrate"], "default": "plain" },
                "renames": { "type": "object", "additionalProperties": { "type": "boolean" }, "description": "Rename keys (from the listing) to apply (true) or not (false)." },
                "concurrently": { "type": "boolean", "default": false, "description": "Build indexes on existing tables CONCURRENTLY, outside the transaction." },
            }),
            &[],
        )
    },
};

async fn generate_migration(ctx: &Ctx, a: MigrationArgs) -> Result<Output, ToolError> {
    let mut query = vec![("format", a.format.clone().unwrap_or_else(|| "plain".into()))];
    if a.concurrently {
        query.push(("concurrently", "true".into()));
    }
    if let Some(renames) = &a.renames {
        query.push(("renames", json!(renames).to_string()));
    }
    let (view, link): (MigrationView, String) = match (&a.draft, &a.database, a.from, a.to) {
        (Some(draft), None, None, None) => {
            let id = uuid(draft, "draft")?;
            let place = draft_place(ctx, id).await?;
            let id = id.to_string();
            let path = [
                "orgs",
                place.org.as_str(),
                "databases",
                place.database.as_str(),
                "drafts",
                id.as_str(),
            ];
            let text: DraftText = ctx
                .api
                .get(&[&path[..], &["text"]].concat(), &[("wait", "true".into())])
                .await?;
            query.push(("sha256", text.reviewed_sha256));
            let view = ctx
                .api
                .get(&[&path[..], &["migration"]].concat(), &query)
                .await?;
            (
                view,
                ctx.link(&format!("{}/{}/drafts/{id}", place.org, place.database)),
            )
        }
        (None, Some(database), Some(from), Some(to)) => {
            let org = ctx.org(a.org.as_deref()).await?;
            query.push(("from", from.to_string()));
            query.push(("to", to.to_string()));
            let view = ctx
                .api
                .get(&["orgs", &org, "databases", database, "migration"], &query)
                .await?;
            (
                view,
                ctx.link(&format!(
                    "{org}/{database}/diff/{from}...{to}?view=migration"
                )),
            )
        }
        _ => {
            return Err(ToolError::Invalid(
                "give a `draft`, or a `database` with `from` and `to` (version numbers).".into(),
            ));
        }
    };
    let mut out = migration_text(&view, true);
    let _ = writeln!(out, "\nPage: {link}");
    Ok(Output::text(out))
}

#[derive(Deserialize)]
struct RenamesArgs {
    draft: String,
    renames: BTreeMap<String, Option<bool>>,
}

static CONFIRM_RENAMES: Tool = Tool {
    name: "confirm_renames",
    title: "Confirm renames",
    description: "Changes shared state: keeps, in a draft, which detected renames its \
                  migration applies, as the migration view's switches do: each key (from \
                  get_draft or generate_migration) to true (rename: the data stays), false \
                  (drop and add), or null (romulus's own choice). It is part of what reviewers \
                  approve: approvals of other choices stop counting.",
    writes: true,
    destructive: false,
    idempotent: true,
    input: || {
        schema::object(
            json!({
                "draft": schema::draft(),
                "renames": { "type": "object", "additionalProperties": { "anyOf": [{ "type": "boolean" }, { "type": "null" }] }, "description": "Rename key → true, false or null." },
            }),
            &["draft", "renames"],
        )
    },
};

#[derive(Deserialize)]
struct Decisions {
    renames: BTreeMap<String, bool>,
}

async fn confirm_renames(ctx: &Ctx, a: RenamesArgs) -> Result<Output, ToolError> {
    let id = uuid(&a.draft, "draft")?;
    let place = draft_place(ctx, id).await?;
    let id = id.to_string();
    let decisions: Decisions = ctx
        .api
        .put(
            &[
                "orgs",
                &place.org,
                "databases",
                &place.database,
                "drafts",
                &id,
                "renames",
            ],
            &json!({ "renames": a.renames }),
        )
        .await?;
    ctx.audit(Audit {
        org: place.org.clone(),
        database: Some(Target::Id(place.database_id)),
        version: None,
        outcome: "chose a draft's renames".into(),
        details: AuditDetails {
            draft_id: uuid(&id, "draft").ok(),
            ..AuditDetails::default()
        },
    })
    .await;
    let mut out = format!(
        "The draft {id} now decides {} rename(s) itself",
        decisions.renames.len()
    );
    if decisions.renames.is_empty() {
        out.push_str(" (romulus's own choices for all).\n");
    } else {
        out.push_str(":\n");
        for (key, on) in &decisions.renames {
            let _ = writeln!(
                out,
                "- {key}: {}",
                if *on { "rename" } else { "drop and add" }
            );
        }
    }
    out.push_str("generate_migration or get_draft shows the migration with them.\n");
    Ok(Output::text(out))
}

#[derive(Deserialize)]
struct ReviewArgs {
    draft: String,
    #[serde(default)]
    cancel: bool,
}

static REQUEST_REVIEW: Tool = Tool {
    name: "request_review",
    title: "Request a review",
    description: "Changes shared state: marks a draft as ready for review (the drafts list \
                  shows it to the organization), or takes that back (`cancel`). Approving is \
                  for people, in the web app: an approval counts only from someone who did not \
                  edit the draft.",
    writes: true,
    destructive: false,
    idempotent: true,
    input: || {
        schema::object(
            json!({ "draft": schema::draft(), "cancel": { "type": "boolean", "default": false } }),
            &["draft"],
        )
    },
};

async fn request_review(ctx: &Ctx, a: ReviewArgs) -> Result<Output, ToolError> {
    let id = uuid(&a.draft, "draft")?;
    let place = draft_place(ctx, id).await?;
    let id = id.to_string();
    let path = [
        "orgs",
        place.org.as_str(),
        "databases",
        place.database.as_str(),
        "drafts",
        id.as_str(),
        "review-request",
    ];
    if a.cancel {
        ctx.api.delete(&path).await?;
    } else {
        let _: Value = ctx.api.post(&path, &json!({})).await?;
    }
    ctx.audit(Audit {
        org: place.org.clone(),
        database: Some(Target::Id(place.database_id)),
        version: None,
        outcome: if a.cancel {
            "withdrew a review request".into()
        } else {
            "requested a review".into()
        },
        details: AuditDetails {
            draft_id: uuid(&id, "draft").ok(),
            ..AuditDetails::default()
        },
    })
    .await;
    let draft: Draft = ctx.api.get(&path[..6], &[]).await?;
    let mut out = if a.cancel {
        format!("Review request withdrawn for draft {id}.\n")
    } else {
        format!(
            "Review requested for draft {id}: the organization sees it in the drafts list \
             (no notification is sent: tell the reviewers).\n"
        )
    };
    if draft.review.required > 0 {
        let _ = writeln!(
            out,
            "Approvals: {}/{} (people who did not edit it approve in the web app).",
            draft.review.approvals, draft.review.required
        );
    }
    let _ = writeln!(
        out,
        "Page: {}",
        ctx.link(&format!("{}/{}/drafts/{id}", place.org, place.database))
    );
    Ok(Output::text(out))
}

#[derive(Deserialize)]
struct PublishArgs {
    draft: String,
    reviewed_sha256: String,
    #[serde(default)]
    message: Option<String>,
}

static PUBLISH_DRAFT: Tool = Tool {
    name: "publish_draft",
    title: "Publish a draft",
    description: "Changes shared state: publishes a draft as the database's next version, \
                  exactly as you reviewed it (`reviewed_sha256` from get_draft: refused if it \
                  changed since), with the organization's rules: the approvals it requires \
                  (from people who did not edit it), a passing migration test when required, \
                  no newer version published meanwhile. Nothing is overridden here: when a \
                  rule holds, it says what is missing. Confirm with your person first.",
    writes: true,
    destructive: false,
    idempotent: false,
    input: || {
        schema::object(
            json!({
                "draft": schema::draft(),
                "reviewed_sha256": { "type": "string", "description": "get_draft's reviewed_sha256 for the text you reviewed." },
                "message": { "type": "string", "maxLength": 1000, "description": "What changed, in a line." },
            }),
            &["draft", "reviewed_sha256"],
        )
    },
};

#[derive(Deserialize)]
struct Preview {
    stale: bool,
    sha256: String,
    renames: BTreeMap<String, bool>,
    diagnostics: Vec<Value>,
    approvals: PreviewApprovals,
    test_gate: TestGate,
    diff: Option<romulus_diff::SchemaDiff>,
}

#[derive(Deserialize)]
struct PreviewApprovals {
    required: i32,
    approved_by: Vec<crate::drafts::Person>,
}

#[derive(Deserialize)]
struct TestGate {
    required: bool,
    available: bool,
    nothing_to_test: bool,
    passing: Option<uuid::Uuid>,
    reason: Option<String>,
}

/// Why the organization's rules hold publishing the previewed draft, if
/// they do (said to the agent: nothing here overrides them).
fn blocked(preview: &Preview, reviewed: &str) -> Option<String> {
    if preview.sha256 != reviewed {
        return Some(
            "The draft changed since you reviewed it (its reviewed_sha256 is another now): \
             read it again with get_draft, check the migration, then publish that."
                .into(),
        );
    }
    if preview.stale {
        return Some(
            "A newer version was published since the draft started: update the draft in \
             the web app (Update draft), review it again, then publish."
                .into(),
        );
    }
    let errors: Vec<Value> = preview
        .diagnostics
        .iter()
        .filter(|d| d["severity"] == "error")
        .cloned()
        .collect();
    if !errors.is_empty() {
        return Some(format!(
            "The draft does not compile, so it cannot be published:\n{}",
            diagnostics(&errors, 20)
        ));
    }
    let approved = i32::try_from(preview.approvals.approved_by.len()).unwrap_or(i32::MAX);
    if approved < preview.approvals.required {
        return Some(format!(
            "The organization requires {} approval(s) of this text from people who did not \
             edit it, and it has {approved}. Ask for a review (request_review) and wait for \
             approvals in the web app.",
            preview.approvals.required
        ));
    }
    let gate = &preview.test_gate;
    if gate.required && !gate.nothing_to_test && gate.passing.is_none() {
        return Some(format!(
            "The organization publishes only migrations that passed a test, and this one has \
             none yet{}. Run test_migration on the draft, then publish.",
            if gate.available {
                String::new()
            } else {
                format!(
                    " (and tests cannot run here: {})",
                    gate.reason.as_deref().unwrap_or("ask an admin")
                )
            }
        ));
    }
    None
}

#[derive(Deserialize)]
struct Published {
    number: i32,
}

async fn publish_draft(ctx: &Ctx, a: PublishArgs) -> Result<Output, ToolError> {
    let id = uuid(&a.draft, "draft")?;
    let place = draft_place(ctx, id).await?;
    let id = id.to_string();
    let path = [
        "orgs",
        place.org.as_str(),
        "databases",
        place.database.as_str(),
        "drafts",
        id.as_str(),
        "publish",
    ];
    let preview: Preview = ctx.api.get(&path, &[]).await?;
    if let Some(reason) = blocked(&preview, a.reviewed_sha256.trim()) {
        return Err(ToolError::Say(reason));
    }
    let published: Published = ctx
        .api
        .post(
            &path,
            &json!({
                "sha256": preview.sha256,
                "message": a.message,
                "renames": preview.renames,
            }),
        )
        .await?;
    ctx.audit(Audit {
        org: place.org.clone(),
        database: Some(Target::Id(place.database_id)),
        version: Some(published.number),
        outcome: format!("published v{}", published.number),
        details: AuditDetails {
            draft_id: uuid(&id, "draft").ok(),
            ..AuditDetails::default()
        },
    })
    .await;
    let summary = preview
        .diff
        .as_ref()
        .map(|d| d.summary.text.clone())
        .unwrap_or_default();
    let out = format!(
        "Published v{} of {}/{} ({summary}).\nPage: {}\n",
        published.number,
        place.org,
        place.database,
        ctx.link(&format!(
            "{}/{}/v/{}",
            place.org, place.database, published.number
        ))
    );
    Ok(Output::text(out))
}
