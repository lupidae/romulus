//! Testing a migration on rows (Test migration): start one, wait a while for
//! its verdict, and poll it after. A test takes from seconds to minutes (its
//! turn, a playground built, the migration up and down): the call waits at
//! most [`MAX_WAIT`], and never past the request's deadline
//! ([`Ctx::wait_budget`]), then hands back the test's id to poll with
//! `get_migration_test`: once a test is started, the answer always carries
//! its id, so a retry never starts a second one.

use std::{collections::BTreeMap, fmt::Write as _, time::Duration};

use romulus_migration_check::{Report, Status as CheckStatus};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    Audit, Ctx, Output, Target, Tool, ToolError, args, draft_place, files::import_format,
    read::one_line, schema, test_place, upload_place, uuid,
};
use crate::{
    audit::AuditDetails,
    domain::{DraftMode, MigrationTestStatus, MigrationTestVerdict},
    drafts::{Draft, text::DraftText},
    imports::pending::NewMigration,
};

/// How long a call waits for a verdict (a request through a reverse proxy
/// must answer well within its limit).
const MAX_WAIT: Duration = Duration::from_secs(40);
const POLL: Duration = Duration::from_secs(2);
/// A compile is waited for again only with this much of the request left
/// (a wait takes up to 10 s, then the test starts and the answer is made).
const COMPILE_MARGIN: Duration = Duration::from_secs(25);

pub const TOOLS: &[&Tool] = &[&TEST_MIGRATION, &GET_MIGRATION_TEST];

pub fn handles(name: &str) -> bool {
    TOOLS.iter().any(|tool| tool.name == name)
}

pub async fn call(ctx: &Ctx, name: &str, arguments: Value) -> Result<Output, ToolError> {
    match name {
        "test_migration" => test_migration(ctx, args(arguments)?).await,
        _ => get_migration_test(ctx, args(arguments)?).await,
    }
}

#[derive(Deserialize)]
struct TestArgs {
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
    version: Option<i32>,
    #[serde(default)]
    playground: Option<String>,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    down: Option<String>,
    #[serde(default)]
    renames: Option<BTreeMap<String, bool>>,
    #[serde(default)]
    concurrently: bool,
    #[serde(default)]
    wait_seconds: Option<u64>,
    #[serde(default)]
    upload: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    fresh: bool,
}

static TEST_MIGRATION: Tool = Tool {
    name: "test_migration",
    title: "Test a migration on data",
    description: "Changes shared state: runs a migration up and down on a throwaway Postgres \
                  filled with generated rows (or a copy of a playground's), then checks the \
                  schema after each direction, row counts, sampled values through renames and \
                  conversions, and data lost against what the migration declares. Its report \
                  is kept for the organization (a publish gate may require a pass). Give a \
                  `draft` (its migration as it is now), a `database` with `from` and `to`, or a \
                  `database` with `version` (the migration it was written as), or an `upload` \
                  of migration files (import_migrations with commit: false): the ones the \
                  database has not imported yet go into a migration-mode draft (`name`: your \
                  open draft of that name is reused) and are tested. Waits up to \
                  `wait_seconds` (40 at most) for the verdict, else answers the test's id: \
                  poll get_migration_test.",
    writes: true,
    destructive: false,
    idempotent: false,
    input: || {
        schema::object(
            json!({
                "draft": schema::draft(),
                "org": schema::org(),
                "database": schema::database(),
                "from": schema::version("With `to`: the version the database is at."),
                "to": schema::version("With `from`: the version migrated to."),
                "version": schema::version("A version's own written or imported migration, from the version before it."),
                "playground": { "type": "string", "format": "uuid", "description": "Test on a copy of this playground's rows (it must hold the base version). Generated rows otherwise." },
                "size": { "type": "string", "enum": ["small", "medium"], "default": "small", "description": "How many generated rows." },
                "down": { "type": "string", "enum": ["generate", "none"], "description": "Run a generated down migration (the default for generated migrations) or none." },
                "renames": { "type": "object", "additionalProperties": { "type": "boolean" }, "description": "Rename keys turned on or off, as generate_migration lists them." },
                "concurrently": { "type": "boolean", "default": false },
                "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 40, "default": 40 },
                "upload": { "type": "string", "format": "uuid", "description": "Migration files sent with import_migrations (commit: false): test the ones the database has not imported yet. The upload is used up." },
                "name": { "type": "string", "maxLength": 200, "description": "With `upload`: the draft's name (\"PR #42: add refunds\"); your open migration-mode draft of that name is reused." },
                "format": { "type": "string", "enum": ["auto", "plain", "sqlx", "flyway", "prisma", "dbmate", "golang_migrate", "node_pg_migrate", "alembic", "django", "rails"], "default": "auto", "description": "With `upload`: the migration tool." },
                "fresh": { "type": "boolean", "default": false, "description": "With `upload`: test again even when the draft's text was tested already (its test is answered otherwise)." },
            }),
            &[],
        )
    },
};

#[derive(Deserialize)]
struct Created {
    id: Uuid,
}

#[allow(
    clippy::too_many_lines,
    reason = "what to test, the options, the start, the wait: one call's steps"
)]
async fn test_migration(ctx: &Ctx, a: TestArgs) -> Result<Output, ToolError> {
    if a.upload.is_some() {
        return test_upload(ctx, a).await;
    }
    if a.name.is_some() || a.format.is_some() || a.fresh {
        return Err(ToolError::Invalid(
            "`name`, `format` and `fresh` go with `upload`.".into(),
        ));
    }
    let mut body = json!({ "concurrently": a.concurrently });
    let mut target = None;
    let (org, database) = match (&a.draft, &a.database, a.from, a.to, a.version) {
        (Some(draft), None, None, None, None) => {
            let id = uuid(draft, "draft")?;
            let place = draft_place(ctx, id).await?;
            let id_text = id.to_string();
            let text: DraftText = ctx
                .api
                .get(
                    &[
                        "orgs",
                        &place.org,
                        "databases",
                        &place.database,
                        "drafts",
                        &id_text,
                        "text",
                    ],
                    &[("wait", "true".into())],
                )
                .await?;
            if text.compile.state != "ok" || !text.compile.current {
                return Err(ToolError::Say(
                    "The draft's text does not compile (yet): get_draft shows its problems.".into(),
                ));
            }
            body["draft_id"] = json!(id);
            body["sha256"] = json!(text.reviewed_sha256);
            target = Some(Target::Id(place.database_id));
            (place.org, place.database)
        }
        (None, Some(database), Some(from), Some(to), None) => {
            body["from"] = json!(from);
            body["to"] = json!(to);
            (ctx.org(a.org.as_deref()).await?, database.clone())
        }
        (None, Some(database), None, None, Some(version)) => {
            body["version"] = json!(version);
            (ctx.org(a.org.as_deref()).await?, database.clone())
        }
        _ => {
            return Err(ToolError::Invalid(
                "give a `draft`, or a `database` with `from` and `to`, or a `database` with \
                 `version`."
                    .into(),
            ));
        }
    };
    if let Some(playground) = &a.playground {
        body["playground"] = json!(uuid(playground, "playground")?);
    }
    if let Some(size) = &a.size {
        body["size"] = json!(size);
    }
    if let Some(down) = &a.down {
        body["down"] = json!(down);
    }
    if let Some(renames) = &a.renames {
        body["renames"] = json!(renames);
    }
    let tests = [
        "orgs",
        org.as_str(),
        "databases",
        database.as_str(),
        "migration-tests",
    ];
    let created: Created = ctx.api.post(&tests, &body).await?;
    ctx.audit(Audit {
        org: org.clone(),
        database: Some(target.unwrap_or_else(|| Target::Slug(database.clone()))),
        version: None,
        outcome: "started a migration test".into(),
        details: AuditDetails {
            test_id: Some(created.id),
            draft_id: a.draft.as_deref().and_then(|d| uuid(d, "draft").ok()),
            ..AuditDetails::default()
        },
    })
    .await;
    let wait = ctx.wait_budget(Duration::from_secs(a.wait_seconds.unwrap_or(40)), MAX_WAIT);
    match wait_for(ctx, &org, &database, created.id, wait).await {
        Ok((test, value)) => Ok(Output::text(test_text(ctx, &org, &database, &test))
            .with_data(test_data(ctx, &org, &database, &value))),
        // Started, and still running as far as this call can tell: its id,
        // never an error a client would retry.
        Err(err) => Ok(Output::text(format!(
            "Migration test {id} of {org}/{database} started and is still running (reading \
             it back failed: {}). Call get_migration_test with test={id} to follow it; do \
             not start another.\nPage: {}\n",
            one_line(&err.text(), 300),
            ctx.link(&format!("{org}/{database}/tests/{}", created.id)),
            id = created.id,
        ))),
    }
}

/// What the upload's new migrations came to (`imports::pending`), as far
/// as a tool needs it.
#[derive(Deserialize)]
struct Taken {
    imported: usize,
    migrations: Vec<NewMigration>,
    ignored: Vec<Value>,
    sql: String,
}

/// Tests the migrations of an upload the database has not imported yet:
/// taken out of the upload as one migration, written into a migration-mode
/// draft (the caller's open one of that `name`, else a new one), then
/// tested as `draft` would be; a test of that very text is answered
/// instead of starting another, unless `fresh`.
#[allow(
    clippy::too_many_lines,
    reason = "take, draft, write, test: one call's steps in order"
)]
async fn test_upload(ctx: &Ctx, a: TestArgs) -> Result<Output, ToolError> {
    if a.draft.is_some()
        || a.database.is_some()
        || a.from.is_some()
        || a.to.is_some()
        || a.version.is_some()
        || a.playground.is_some()
        || a.renames.is_some()
    {
        return Err(ToolError::Invalid(
            "with `upload`, give none of `draft`, `database`, `from`, `to`, `version`, \
             `playground` or `renames`: the upload says which database, its migrations what \
             to test."
                .into(),
        ));
    }
    let upload = uuid(a.upload.as_deref().unwrap_or_default(), "upload")?;
    let format = import_format(a.format.as_deref())?;
    let place = upload_place(ctx, upload).await?;
    let (org, database) = (place.org, place.database);
    let base = ["orgs", org.as_str(), "databases", database.as_str()];
    let upload_text = upload.to_string();
    let taken: Taken = ctx
        .api
        .post(
            &[
                &base[..],
                &["imports", "uploads", upload_text.as_str(), "new-migrations"],
            ]
            .concat(),
            &json!({ "format": format }),
        )
        .await?;
    // The person's own upload, used up: nothing anyone else sees yet.
    ctx.nothing_to_audit();
    let listed = json!({
        "org": org,
        "database": database,
        "imported": taken.imported,
        "migrations": taken.migrations,
        "ignored": taken.ignored,
    });
    if taken.migrations.is_empty() {
        let mut data = listed;
        data["outcome"] = json!("nothing_new");
        return Ok(Output::text(format!(
            "No migration in these files is new to {org}/{database} ({} imported already): \
             nothing to test.\n",
            taken.imported
        ))
        .with_data(data));
    }

    let (draft, reused) = upload_draft(ctx, &org, &database, a.name.as_deref()).await?;
    let draft_id = draft.id.to_string();
    let draft_path = [&base[..], &["drafts", draft_id.as_str()]].concat();
    let draft_page = ctx.link(&format!("{org}/{database}/drafts/{draft_id}"));
    let mut written: DraftText = ctx
        .api
        .put(
            &[&draft_path[..], &["text"]].concat(),
            &json!({ "text": taken.sql, "wait": true }),
        )
        .await
        .map_err(|failure| {
            ToolError::Say(format!(
                "The draft {draft_id} \"{}\" holds the migrations to test, but writing them \
                 failed: {}\nPage: {draft_page}",
                draft.name,
                ToolError::Api(failure).text()
            ))
        })?;
    let count = taken.migrations.len();
    ctx.audit(Audit {
        org: org.clone(),
        database: Some(Target::Id(draft.database.id)),
        version: None,
        outcome: format!(
            "wrote {count} new migration{} into draft \"{}\"",
            if count == 1 { "" } else { "s" },
            draft.name
        ),
        details: AuditDetails {
            draft_id: Some(draft.id),
            draft_name: Some(draft.name.clone()),
            ..AuditDetails::default()
        },
    })
    .await;
    let mut data = listed;
    data["draft"] = json!({
        "id": draft.id,
        "name": draft.name,
        "base_version": written.base_version,
        "reused": reused,
        "page": draft_page,
    });
    let mut out = format!(
        "{count} new migration{} of {org}/{database} ({} imported already) in draft {draft_id} \
         \"{}\"{}, from v{}:\n",
        if count == 1 { "" } else { "s" },
        taken.imported,
        draft.name,
        if reused { " (reused)" } else { "" },
        written
            .base_version
            .map_or_else(|| "?".into(), |n| n.to_string()),
    );
    for migration in &taken.migrations {
        let _ = writeln!(
            out,
            "- {} ({} statement{})",
            migration.path,
            migration.statements,
            if migration.statements == 1 { "" } else { "s" }
        );
    }

    // A long migration on a large schema compiles past one wait: waited
    // for again while the request has time.
    while written.compile.state == "pending"
        && !written.compile.current
        && ctx.api.time_left() > COMPILE_MARGIN
    {
        written = ctx
            .api
            .get(
                &[&draft_path[..], &["text"]].concat(),
                &[("wait", "true".into())],
            )
            .await?;
    }
    if written.compile.state != "ok" || !written.compile.current {
        let diagnostics = in_files(&written.compile.diagnostics, &taken.migrations);
        data["outcome"] = json!(if written.compile.state == "failed" {
            "compile_failed"
        } else {
            "not_compiled"
        });
        data["compile"] =
            json!({ "state": written.compile.state, "current": written.compile.current });
        data["diagnostics"] = json!(diagnostics);
        if written.compile.state == "failed" {
            out.push_str("They do NOT apply to the latest version:\n");
            for d in diagnostics.iter().take(20) {
                let _ = writeln!(
                    out,
                    "- {}{}: {}",
                    d["path"].as_str().unwrap_or("?"),
                    d["line"]
                        .as_u64()
                        .map_or_else(String::new, |l| format!(":{l}")),
                    d["message"].as_str().unwrap_or("")
                );
            }
        } else {
            out.push_str(
                "They were not compiled yet (or the compiler is unavailable): call \
                 test_migration with the draft in a moment.\n",
            );
        }
        let _ = writeln!(out, "Page: {draft_page}");
        return Ok(Output::text(out).with_data(data));
    }

    let down = a.down.as_deref().unwrap_or("none");
    let size = a.size.as_deref().unwrap_or("small");
    let earlier = if a.fresh {
        None
    } else {
        tested_already(ctx, &base, draft.id, down, size).await?
    };
    let id = if let Some(id) = earlier {
        data["reused_test"] = json!(true);
        id
    } else {
        let mut body = json!({
            "draft_id": draft.id,
            "sha256": written.reviewed_sha256,
            "concurrently": a.concurrently,
            "size": size,
        });
        if let Some(down) = &a.down {
            body["down"] = json!(down);
        }
        let created: Created = ctx
            .api
            .post(&[&base[..], &["migration-tests"]].concat(), &body)
            .await?;
        ctx.audit(Audit {
            org: org.clone(),
            database: Some(Target::Id(draft.database.id)),
            version: None,
            outcome: "started a migration test".into(),
            details: AuditDetails {
                test_id: Some(created.id),
                draft_id: Some(draft.id),
                ..AuditDetails::default()
            },
        })
        .await;
        data["reused_test"] = json!(false);
        created.id
    };
    let wait = ctx.wait_budget(Duration::from_secs(a.wait_seconds.unwrap_or(40)), MAX_WAIT);
    match wait_for(ctx, &org, &database, id, wait).await {
        Ok((test, value)) => {
            data["outcome"] = json!(if test.status.is_final() {
                "tested"
            } else {
                "testing"
            });
            data["page"] = json!(ctx.link(&format!("{org}/{database}/tests/{id}")));
            data["test"] = value;
            if earlier.is_some() {
                out.push_str("This text was tested already: its test.\n");
            }
            out.push_str(&test_text(ctx, &org, &database, &test));
            Ok(Output::text(out).with_data(data))
        }
        Err(err) => {
            data["outcome"] = json!("testing");
            data["test"] = json!({ "id": id });
            data["page"] = json!(ctx.link(&format!("{org}/{database}/tests/{id}")));
            let _ = writeln!(
                out,
                "Migration test {id} started (reading it back failed: {}). Call \
                 get_migration_test with test={id} to follow it; do not start another.",
                one_line(&err.text(), 300),
            );
            Ok(Output::text(out).with_data(data))
        }
    }
}

/// The draft an upload's migrations go into: the caller's open
/// migration-mode draft named `name` when there is one on the latest
/// version (one on an older version is deleted: its base would be the
/// wrong one), else a new one. Whether it was reused.
async fn upload_draft(
    ctx: &Ctx,
    org: &str,
    database: &str,
    name: Option<&str>,
) -> Result<(Draft, bool), ToolError> {
    let me = ctx.api.agent().user.id;
    let base = ["orgs", org, "databases", database];
    let drafts_path = [&base[..], &["drafts"]].concat();
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    if let Some(name) = name {
        let drafts: Vec<Draft> = ctx.api.get(&drafts_path, &[]).await?;
        let mine = drafts.into_iter().find(|d| {
            d.name == name
                && d.mode == DraftMode::Migration
                && d.created_by.as_ref().is_some_and(|p| p.id == me)
        });
        if let Some(draft) = mine {
            if !draft.stale {
                return Ok((draft, true));
            }
            let id = draft.id.to_string();
            ctx.api
                .delete(&[&drafts_path[..], &[id.as_str()]].concat())
                .await?;
            ctx.audit(Audit {
                org: org.to_owned(),
                database: Some(Target::Id(draft.database.id)),
                version: None,
                outcome: format!(
                    "deleted draft \"{}\": a version came after its base, a new one tests the \
                     migrations",
                    draft.name
                ),
                details: AuditDetails {
                    draft_id: Some(draft.id),
                    draft_name: Some(draft.name.clone()),
                    ..AuditDetails::default()
                },
            })
            .await;
        }
    }
    let mut draft: Draft = ctx
        .api
        .post(&drafts_path, &json!({ "mode": "migration" }))
        .await?;
    if let Some(name) = name {
        let id = draft.id.to_string();
        // A name refused leaves the default one: the draft is made all the
        // same, and the test goes on.
        if let Ok(renamed) = ctx
            .api
            .patch::<Draft>(
                &[&drafts_path[..], &[id.as_str()]].concat(),
                &json!({ "name": name }),
            )
            .await
        {
            draft = renamed;
        }
    }
    ctx.audit(Audit {
        org: org.to_owned(),
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
    Ok((draft, false))
}

/// A test of the draft's text as it is now, run (or running) with the same
/// rows and down migration, that did not stop short: its id.
async fn tested_already(
    ctx: &Ctx,
    base: &[&str],
    draft: Uuid,
    down: &str,
    size: &str,
) -> Result<Option<Uuid>, ToolError> {
    let tests: Value = ctx
        .api
        .get(
            &[base, &["migration-tests"]].concat(),
            &[("draft", draft.to_string())],
        )
        .await?;
    let down = if down == "generate" {
        "generated"
    } else {
        down
    };
    Ok(tests
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| {
            t["current"] == true
                && !matches!(t["status"].as_str(), Some("error" | "cancelled"))
                && t["down"] == down
                && t["data"]["size"] == size
                && t["data"]["playground"].is_null()
        })
        .and_then(|t| t["id"].as_str())
        .and_then(|id| Uuid::parse_str(id).ok()))
}

/// The compile's diagnostics told against the files: each with the `path`
/// and `line` of the migration it falls in (`null` in the comment lines
/// between them).
fn in_files(diagnostics: &Value, migrations: &[NewMigration]) -> Vec<Value> {
    diagnostics
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| {
            let line = d["span"]["from"]["line"]
                .as_u64()
                .and_then(|l| u32::try_from(l).ok());
            let at = line.and_then(|line| {
                migrations
                    .iter()
                    .find(|m| (m.sql_line..m.sql_line + m.lines.max(1)).contains(&line))
                    .map(|m| (m, m.line + (line - m.sql_line)))
            });
            json!({
                "path": at.map(|(m, _)| m.path.as_str()),
                "line": at.map(|(_, line)| line),
                "column": d["span"]["from"]["column"],
                "severity": d["severity"],
                "code": d["code"],
                "message": d["message"],
                "detail": d["detail"],
                "hint": d["hint"],
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct GetArgs {
    test: String,
    #[serde(default)]
    wait_seconds: Option<u64>,
}

static GET_MIGRATION_TEST: Tool = Tool {
    name: "get_migration_test",
    title: "Get a migration test",
    description: "A migration test (from test_migration): its place in line or progress while \
                  it runs, then its verdict (passed, passed up only, needs acknowledgement, \
                  failed), what failed and why, and the data it found lost. Waits up to \
                  `wait_seconds` (40 at most; 0 by default) for it to finish.",
    writes: false,
    destructive: false,
    idempotent: true,
    input: || {
        schema::object(
            json!({
                "test": { "type": "string", "format": "uuid", "description": "The test's id." },
                "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 40, "default": 0 },
            }),
            &["test"],
        )
    },
};

async fn get_migration_test(ctx: &Ctx, a: GetArgs) -> Result<Output, ToolError> {
    let id = uuid(&a.test, "test")?;
    let place = test_place(ctx, id).await?;
    let wait = ctx.wait_budget(Duration::from_secs(a.wait_seconds.unwrap_or(0)), MAX_WAIT);
    let (test, value) = wait_for(ctx, &place.org, &place.database, id, wait).await?;
    let data = test_data(ctx, &place.org, &place.database, &value);
    Ok(Output::text(test_text(ctx, &place.org, &place.database, &test)).with_data(data))
}

/// A test as a program reads it: the API's JSON, and its page.
fn test_data(ctx: &Ctx, org: &str, database: &str, test: &Value) -> Value {
    let page = ctx.link(&format!(
        "{org}/{database}/tests/{}",
        test["id"].as_str().unwrap_or("")
    ));
    json!({ "org": org, "database": database, "test": test, "page": page })
}

/// A test, as an agent needs it.
#[derive(Deserialize)]
struct TestView {
    id: Uuid,
    status: MigrationTestStatus,
    draft: Option<TestDraft>,
    from: i32,
    to: Option<i32>,
    verdict: Option<MigrationTestVerdict>,
    rows_checked: bool,
    position: Option<usize>,
    progress: Option<Value>,
    report: Option<Box<Report>>,
    failure: Option<String>,
    #[serde(default)]
    summary: Option<Value>,
}

#[derive(Deserialize)]
struct TestDraft {
    name: String,
}

/// The test once it is final, or as it is after `wait`: read, and as the
/// API answered it.
async fn wait_for(
    ctx: &Ctx,
    org: &str,
    database: &str,
    id: Uuid,
    wait: Duration,
) -> Result<(TestView, Value), ToolError> {
    let deadline = tokio::time::Instant::now() + wait;
    let id_text = id.to_string();
    let path = [
        "orgs",
        org,
        "databases",
        database,
        "migration-tests",
        &id_text,
    ];
    loop {
        let value: Value = ctx.api.get(&path, &[]).await?;
        let test: TestView = serde_json::from_value(value.clone())
            .map_err(|err| ToolError::Say(format!("Unreadable migration test: {err}")))?;
        if test.status.is_final() || tokio::time::Instant::now() + POLL > deadline {
            return Ok((test, value));
        }
        tokio::time::sleep(POLL).await;
    }
}

fn test_text(ctx: &Ctx, org: &str, database: &str, test: &TestView) -> String {
    let what = match (&test.draft, test.to) {
        (Some(draft), _) => format!("draft \"{}\" from v{}", draft.name, test.from),
        (None, Some(to)) => format!("v{} → v{to}", test.from),
        (None, None) => format!("from v{}", test.from),
    };
    let link = ctx.link(&format!("{org}/{database}/tests/{}", test.id));
    let mut out = format!("Migration test {} of {org}/{database} ({what}): ", test.id);
    if !test.status.is_final() {
        match test.position {
            Some(position) => {
                let _ = writeln!(out, "queued, number {position} in line.");
            }
            None => {
                let _ = writeln!(out, "running.");
            }
        }
        if let Some(progress) = &test.progress {
            let stage = progress["stage"].as_str().unwrap_or("");
            let _ = writeln!(out, "Stage: {stage}.");
        }
        let _ = writeln!(
            out,
            "Still running: call get_migration_test with test={} (wait_seconds up to 40) in a \
             moment; do not start another.\nPage: {link}",
            test.id
        );
        return out;
    }
    let verdict = test.verdict.map_or_else(
        || format!("{:?}", test.status).to_lowercase(),
        |v| {
            match v {
                MigrationTestVerdict::Passed => "PASSED (up and down)",
                MigrationTestVerdict::PassedUpOnly => "PASSED, up only (no down migration ran)",
                MigrationTestVerdict::NeedsAcknowledgement => {
                    "NEEDS ACKNOWLEDGEMENT: it loses data the migration does not declare"
                }
                MigrationTestVerdict::Failed => "FAILED",
            }
            .to_owned()
        },
    );
    let _ = writeln!(out, "{verdict}.");
    if let Some(failure) = &test.failure {
        let _ = writeln!(out, "It could not run: {failure}");
    }
    if let Some(report) = &test.report {
        let _ = writeln!(out, "{}", report.headline);
        for phase in &report.phases {
            if phase.status == CheckStatus::Passed || phase.status == CheckStatus::Skipped {
                continue;
            }
            let _ = writeln!(
                out,
                "- {:?} {:?}: {}",
                phase.phase, phase.status, phase.summary
            );
            for check in phase
                .checks
                .iter()
                .filter(|c| c.status != CheckStatus::Passed && c.status != CheckStatus::Skipped)
            {
                let _ = writeln!(out, "  - {}", check.summary);
                for detail in check.details.iter().take(5) {
                    let _ = writeln!(out, "    {}", one_line(detail, 300));
                }
            }
        }
        for loss in &report.data_loss {
            let at = loss
                .column
                .as_ref()
                .map_or_else(|| loss.table.clone(), |c| format!("{}.{c}", loss.table));
            let _ = writeln!(
                out,
                "- data lost in {at}: {:?}, {} row(s){}",
                loss.kind,
                loss.rows,
                if loss.expected {
                    " (declared by the migration)"
                } else {
                    " (NOT declared by any statement)"
                }
            );
        }
        if report.nothing_checked {
            out.push_str(
                "Note: every table the migration changes was empty, so no rows were checked.\n",
            );
        }
    } else if let Some(summary) = &test.summary {
        let _ = writeln!(out, "{}", summary["headline"].as_str().unwrap_or(""));
    }
    if !test.rows_checked && test.failure.is_none() {
        out.push_str("No rows were checked (a publish gate does not count this test).\n");
    }
    let _ = writeln!(out, "Report: {link}");
    out
}
