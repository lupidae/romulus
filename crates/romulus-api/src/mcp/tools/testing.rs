//! Testing a migration on rows (Test migration): start one, wait a while for
//! its verdict, and poll it after. A test takes from seconds to minutes (its
//! turn, a playground built, the migration up and down): the call waits at
//! most [`MAX_WAIT`], well under what a proxy in front allows, then hands
//! back the test's id to poll with `get_migration_test`.

use std::{collections::BTreeMap, fmt::Write as _, time::Duration};

use romulus_migration_check::{Report, Status as CheckStatus};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    Audit, Ctx, Output, Tool, ToolError, args, draft_place, read::one_line, schema, test_place,
    uuid,
};
use crate::{
    audit::AuditDetails,
    domain::{MigrationTestStatus, MigrationTestVerdict},
    drafts::text::DraftText,
};

/// How long a call waits for a verdict (a request through a reverse proxy
/// must answer well within its limit).
const MAX_WAIT: Duration = Duration::from_secs(40);
const POLL: Duration = Duration::from_secs(2);

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
                  `database` with `version` (the migration it was written as). Waits up to \
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
            }),
            &[],
        )
    },
};

#[derive(Deserialize)]
struct Created {
    id: Uuid,
}

async fn test_migration(ctx: &Ctx, a: TestArgs) -> Result<Output, ToolError> {
    let mut body = json!({ "concurrently": a.concurrently });
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
    let wait = Duration::from_secs(a.wait_seconds.unwrap_or(40)).min(MAX_WAIT);
    let test = wait_for(ctx, &org, &database, created.id, wait).await?;
    let db: crate::databases::Database = ctx
        .api
        .get(&["orgs", &org, "databases", &database], &[])
        .await?;
    Ok(
        Output::text(test_text(ctx, &org, &database, &test)).audited(Audit {
            org,
            database: Some(db.id),
            version: None,
            outcome: "started a migration test".into(),
            details: AuditDetails {
                test_id: Some(created.id),
                draft_id: test.draft.as_ref().map(|d| d.id),
                draft_name: test.draft.as_ref().map(|d| d.name.clone()),
                ..AuditDetails::default()
            },
        }),
    )
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
    let wait = Duration::from_secs(a.wait_seconds.unwrap_or(0)).min(MAX_WAIT);
    let test = wait_for(ctx, &place.org, &place.database, id, wait).await?;
    Ok(Output::text(test_text(
        ctx,
        &place.org,
        &place.database,
        &test,
    )))
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
    id: Uuid,
    name: String,
}

/// The test once it is final, or as it is after `wait`.
async fn wait_for(
    ctx: &Ctx,
    org: &str,
    database: &str,
    id: Uuid,
    wait: Duration,
) -> Result<TestView, ToolError> {
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
        let test: TestView = ctx.api.get(&path, &[]).await?;
        if test.status.is_final() || tokio::time::Instant::now() + POLL > deadline {
            return Ok(test);
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
            "Not finished yet: call get_migration_test with test={} (wait_seconds up to 40) in \
             a moment.\nPage: {link}",
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
