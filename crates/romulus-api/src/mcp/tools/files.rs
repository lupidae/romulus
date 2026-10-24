//! Migration files between the person's repository and romulus. romulus
//! cannot read the person's disk: the agent reads the files and sends their
//! contents (`import_migrations`), and writes the files romulus answers
//! (`get_migration_files`).
//!
//! An import goes through an upload (`imports::uploads`): the files, in one
//! call or several (a message carries 4 MiB at most), then the web import's
//! own engine, which runs on after the call; `get_import` follows it. The
//! files of a migration are the web download's (the same endpoints, the
//! same options), paged when long, with every page pinned to the first
//! one's choices (its version, the draft's text) by a cursor. Each file
//! sits between markers carrying a code random to the answer, with its
//! length in bytes: a file's content (a comment someone wrote, a name)
//! cannot end it early or start another one.

use std::{collections::BTreeMap, fmt::Write as _, time::Duration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use romulus_migration::{File, Format, Warning};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    Audit, Ctx, Output, Target, Tool, ToolError, args, draft_place, read::one_line, schema,
    upload_place, uuid,
};
use crate::{
    audit::AuditDetails,
    domain::{ImportUploadStatus, MigrationFormat},
    drafts::text::DraftText,
    error::ErrorBody,
    mcp::api::Failure,
    token,
};

pub const TOOLS: &[&Tool] = &[&IMPORT_MIGRATIONS, &GET_IMPORT, &GET_MIGRATION_FILES];

pub fn handles(name: &str) -> bool {
    TOOLS.iter().any(|tool| tool.name == name)
}

pub async fn call(ctx: &Ctx, name: &str, arguments: Value) -> Result<Output, ToolError> {
    match name {
        "import_migrations" => import_migrations(ctx, args(arguments)?).await,
        "get_import" => get_import(ctx, args(arguments)?).await,
        _ => get_migration_files(ctx, args(arguments)?).await,
    }
}

/// Migrations, skipped statements a report lists at most.
const LISTED: usize = 40;

/// How long a call waits for an import to end.
const MAX_WAIT: Duration = Duration::from_secs(40);
const POLL: Duration = Duration::from_secs(1);

#[derive(Deserialize, Serialize)]
struct FileArg {
    path: String,
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    part: Option<i32>,
}

#[derive(Deserialize)]
struct ImportArgs {
    #[serde(default)]
    org: Option<String>,
    #[serde(default)]
    database: Option<String>,
    #[serde(default)]
    upload: Option<String>,
    #[serde(default)]
    files: Vec<FileArg>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    append: bool,
    #[serde(default)]
    dry_run: bool,
    #[serde(default = "yes")]
    commit: bool,
    #[serde(default)]
    wait_seconds: Option<u64>,
}

fn yes() -> bool {
    true
}

static IMPORT_MIGRATIONS: Tool = Tool {
    name: "import_migrations",
    title: "Import migration files",
    description: "Changes shared state: imports a repository's migration files as a database's \
                  version history, one version per migration, as the web import does (data \
                  statements skipped and reported; migrations imported before skipped; a changed \
                  one refused). romulus cannot read your disk: read the migrations folder \
                  yourself and send each file's `path` (relative to the repository) and exact \
                  `content`. All in one call when they fit (about 3 MB); otherwise `commit: \
                  false` with the first files, then the answered `upload` id with the next, \
                  `commit: true` (the default) on the last. In an organization that reviews \
                  changes (approvals or tests required), it only gives a database without \
                  versions its history: later changes go through a draft (create_draft in \
                  migration mode). Rails, Django and Alembic migrations are code: import the SQL \
                  their exporter prints (romulus://export-migrations/<tool>.sh). Waits up to \
                  `wait_seconds` (40 at most), else answers its id: poll get_import.",
    writes: true,
    destructive: false,
    idempotent: false,
    input: || {
        schema::object(
            json!({
                "org": schema::org(),
                "database": { "type": "string", "description": "The database's slug (list_databases lists them; create_database makes one). Not needed with `upload`." },
                "upload": { "type": "string", "format": "uuid", "description": "The upload an earlier call with `commit: false` answered: these files are added to it." },
                "files": {
                    "type": "array",
                    "description": "Files read from the repository: every file of the migrations folder (SQL files and the tool's own, like migration_lock.toml; no secrets). Keep one call under about 3 MB of content.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "Relative to the repository, with `/`: db/migration/V2__add_orders.sql, prisma/migrations/20240101000000_init/migration.sql. Never absolute." },
                            "content": { "type": "string", "description": "The file's text, exactly as on disk." },
                            "part": { "type": "integer", "minimum": 1, "maximum": 10, "description": "Only for a file larger than one call: send it in parts 1, 2… (each in its own call, same path, none empty); they are joined in order." },
                        },
                        "required": ["path", "content"],
                        "additionalProperties": false,
                    },
                },
                "format": { "type": "string", "enum": ["auto", "plain", "sqlx", "flyway", "prisma", "dbmate", "golang_migrate", "node_pg_migrate", "alembic", "django", "rails"], "default": "auto", "description": "The migration tool; `auto` detects it from the paths and contents. Applies on the call that commits." },
                "append": { "type": "boolean", "default": false, "description": "Import even though the database has versions that were not imported (they stay first). Only when your person agrees." },
                "dry_run": { "type": "boolean", "default": false, "description": "Replay the files without storing anything: a check. The files stay uploaded, to commit for real next." },
                "commit": { "type": "boolean", "default": true, "description": "true: these are the last files, import now. false: more files follow in another call (with `upload`)." },
                "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 40, "default": 40 },
            }),
            &[],
        )
    },
};

/// An upload, as the API answers it (`imports::uploads::ImportUpload`).
#[derive(Deserialize)]
struct UploadView {
    id: Uuid,
    status: ImportUploadStatus,
    files: i64,
    bytes: i64,
    dry_run: bool,
    expires_at: DateTime<Utc>,
    expired: bool,
    progress: Option<Progress>,
    report: Option<Report>,
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct Progress {
    stage: String,
    replayed: usize,
    total: usize,
    first_number: Option<i32>,
}

/// An import's report (`imports::ImportReport`), as far as an agent needs.
#[derive(Deserialize)]
struct Report {
    status: String,
    format: MigrationFormat,
    migrations: Vec<ReportedMigration>,
    ignored: Vec<Ignored>,
    created: usize,
}

#[derive(Deserialize)]
struct ReportedMigration {
    path: String,
    status: String,
    version: Option<i32>,
    changes: Option<String>,
    skipped: Vec<Skipped>,
}

#[derive(Deserialize)]
struct Skipped {
    line: u32,
    kind: String,
    reason: String,
    text: String,
}

#[derive(Deserialize)]
struct Ignored {
    path: String,
    reason: String,
}

/// `auto` (or nothing) is `None`: detected from the files.
pub(super) fn import_format(format: Option<&str>) -> Result<Option<MigrationFormat>, ToolError> {
    match format.map(str::trim) {
        None | Some("" | "auto") => Ok(None),
        Some(name) => serde_json::from_value(json!(name)).map(Some).map_err(|_| {
            ToolError::Invalid(format!(
                "`format` {name:?}: use auto, plain, sqlx, flyway, prisma, dbmate, golang_migrate, \
                 node_pg_migrate, alembic, django or rails."
            ))
        }),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "stage, commit, wait, answer: one call's steps in order"
)]
async fn import_migrations(ctx: &Ctx, a: ImportArgs) -> Result<Output, ToolError> {
    let format = import_format(a.format.as_deref())?;
    let given = match &a.upload {
        Some(upload) => Some(uuid(upload, "upload")?),
        None => None,
    };
    if given.is_none() && a.files.is_empty() {
        return Err(ToolError::Invalid(
            "send `files` (each with its `path` and `content`, read from the repository), or \
             `upload` to import files sent before."
                .into(),
        ));
    }
    let (org, database) = if let Some(id) = given {
        let place = upload_place(ctx, id).await?;
        (place.org, place.database)
    } else {
        let database = a.database.clone().ok_or_else(|| {
            ToolError::Invalid("give the `database` (its slug) to import into.".into())
        })?;
        (ctx.org(a.org.as_deref()).await?, database)
    };
    let uploads = ["orgs", &org, "databases", &database, "imports", "uploads"];
    let body = json!({ "files": a.files });
    let sent = a.files.len();
    let upload: UploadView = match given {
        None => ctx.api.post(&uploads, &body).await?,
        Some(id) if sent > 0 => {
            let id = id.to_string();
            ctx.api
                .post(&[&uploads[..], &[id.as_str(), "files"]].concat(), &body)
                .await?
        }
        Some(id) => {
            let id = id.to_string();
            ctx.api
                .get(&[&uploads[..], &[id.as_str()]].concat(), &[])
                .await?
        }
    };
    // Files staged for the person alone: nothing for the Activity yet.
    ctx.nothing_to_audit();
    let id = upload.id;
    if !a.commit {
        let later = if format.is_some() || a.append || a.dry_run {
            " Give `format`, `append` and `dry_run` on that last call: they apply when it imports."
        } else {
            ""
        };
        let data = json!({
            "org": org,
            "database": database,
            "upload": {
                "id": id,
                "files": upload.files,
                "bytes": upload.bytes,
                "expires_at": upload.expires_at,
            },
        });
        return Ok(Output::text(format!(
            "Upload {id} for {org}/{database}: {} file{} ({}) received so far, {sent} in this \
             call. Nothing is imported yet. Send the next files with upload={id} (commit: false \
             while more follow), then commit: true on the last call (or with no files). It \
             expires at {} (an hour after its last files, a day after it started at most).\
             {later}\n",
            upload.files,
            if upload.files == 1 { "" } else { "s" },
            size(upload.bytes),
            upload.expires_at.format("%H:%M UTC"),
        ))
        .with_data(data));
    }

    let id_text = id.to_string();
    let mut options = json!({ "append": a.append, "dry_run": a.dry_run });
    if let Some(format) = format {
        options["format"] = json!(format);
    }
    let started: Result<UploadView, Failure> = ctx
        .api
        .post(
            &[&uploads[..], &[id_text.as_str(), "import"]].concat(),
            &options,
        )
        .await;
    let started = match started {
        Ok(started) => started,
        Err(failure) => {
            return Err(ToolError::Say(refused_text(&failure, id, a.append)));
        }
    };
    if !a.dry_run {
        ctx.audit(Audit {
            org: org.clone(),
            database: Some(Target::Slug(database.clone())),
            version: None,
            outcome: format!(
                "started importing {} migration file{} (import {id})",
                started.files,
                if started.files == 1 { "" } else { "s" }
            ),
            details: AuditDetails::default(),
        })
        .await;
    }
    let wait = ctx.wait_budget(Duration::from_secs(a.wait_seconds.unwrap_or(40)), MAX_WAIT);
    match wait_for(ctx, &org, &database, id, wait).await {
        Ok(upload) => Ok(Output::text(upload_text(ctx, &org, &database, &upload))),
        // Started: its id, never an error a client would retry.
        Err(err) => Ok(Output::text(format!(
            "Import {id} into {org}/{database} started (reading it back failed: {}). Call \
             get_import with import={id} to follow it; do not import again.\n",
            one_line(&err.text(), 300)
        ))),
    }
}

/// The import refused before it ran, with where it stopped and what to do;
/// the files stay uploaded.
fn refused_text(failure: &Failure, id: Uuid, appended: bool) -> String {
    let mut out = ToolError::Api(failure.clone()).text();
    if let Some(details) = &failure.body.details {
        failure_details(&mut out, failure.code(), details);
    }
    let kept = matches!(
        failure.code(),
        "has_versions"
            | "migration_changed"
            | "migration_out_of_order"
            | "import_failed"
            | "invalid_migrations"
            | "incomplete_upload"
            | "too_many_migrations"
            | "concurrent_import"
            | "plan_limit"
            | "rate_limited"
    );
    if failure.code() == "has_versions" && !appended {
        let _ = write!(
            out,
            "\nAsk your person before appending: the imported migrations would come after the \
             versions the database has. If they agree, call import_migrations with \
             upload={id}, append: true."
        );
    }
    if kept {
        let _ = write!(
            out,
            "\nThe files stay uploaded (upload {id}, an hour): send a corrected file with \
             upload={id} and commit: false (it replaces the one with the same path), then \
             commit again with upload={id}."
        );
    }
    out.push('\n');
    out
}

/// What an import's error details say (`import_failed`: the migration, its
/// file and lines).
fn failure_details(out: &mut String, code: &str, details: &Value) {
    if code == "import_failed" {
        let path = details["path"].as_str().unwrap_or("?");
        let _ = write!(out, "\nIn {path}:");
        for error in details["errors"].as_array().into_iter().flatten().take(10) {
            let line = error["line"]
                .as_u64()
                .map_or_else(String::new, |l| format!(" line {l}"));
            let _ = write!(
                out,
                "\n-{line}: {}",
                one_line(error["message"].as_str().unwrap_or(""), 300)
            );
            if let Some(hint) = error["hint"].as_str() {
                let _ = write!(out, " (hint: {})", one_line(hint, 300));
            }
        }
    } else if code == "incomplete_upload" || code == "invalid_file" {
        // The message names the file already.
    } else if !matches!(code, "validation" | "plan_limit" | "rate_limited") {
        let _ = write!(out, "\nDetails: {}", one_line(&details.to_string(), 500));
    }
}

#[derive(Deserialize)]
struct GetArgs {
    import: String,
    #[serde(default)]
    wait_seconds: Option<u64>,
}

static GET_IMPORT: Tool = Tool {
    name: "get_import",
    title: "Get an import",
    description: "An import of migration files (from import_migrations): its progress while it \
                  runs (queued, migrations replayed of how many), then the versions it created, \
                  the statements it skipped and the files it ignored, or why it stopped and \
                  where. Waits up to `wait_seconds` (40 at most; 0 by default) for it to end.",
    writes: false,
    destructive: false,
    idempotent: true,
    input: || {
        schema::object(
            json!({
                "import": { "type": "string", "format": "uuid", "description": "The import's id (the upload's, from import_migrations)." },
                "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 40, "default": 0 },
            }),
            &["import"],
        )
    },
};

async fn get_import(ctx: &Ctx, a: GetArgs) -> Result<Output, ToolError> {
    let id = uuid(&a.import, "import")?;
    let place = upload_place(ctx, id).await?;
    let wait = ctx.wait_budget(Duration::from_secs(a.wait_seconds.unwrap_or(0)), MAX_WAIT);
    let upload = wait_for(ctx, &place.org, &place.database, id, wait).await?;
    Ok(Output::text(upload_text(
        ctx,
        &place.org,
        &place.database,
        &upload,
    )))
}

/// The upload once its import ended, or as it is after `wait`.
async fn wait_for(
    ctx: &Ctx,
    org: &str,
    database: &str,
    id: Uuid,
    wait: Duration,
) -> Result<UploadView, ToolError> {
    let deadline = tokio::time::Instant::now() + wait;
    let id = id.to_string();
    let path = [
        "orgs",
        org,
        "databases",
        database,
        "imports",
        "uploads",
        &id,
    ];
    loop {
        let upload: UploadView = ctx.api.get(&path, &[]).await?;
        if upload.status.is_final() || tokio::time::Instant::now() + POLL > deadline {
            return Ok(upload);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// `812 bytes`, `14 KB`, `2.5 MB`.
fn size(bytes: i64) -> String {
    const MB: i64 = 1024 * 1024;
    match bytes {
        b if b < 1024 => format!("{b} bytes"),
        b if b < MB => format!("{} KB", b / 1024),
        b => format!("{}.{} MB", b / MB, b % MB * 10 / MB),
    }
}

/// An upload and its import, as an agent reports it.
fn upload_text(ctx: &Ctx, org: &str, database: &str, upload: &UploadView) -> String {
    let id = upload.id;
    let history = ctx.link(&format!("{org}/{database}/history"));
    let mut out = String::new();
    match upload.status {
        ImportUploadStatus::Open if upload.expired => {
            let _ = writeln!(
                out,
                "Upload {id} for {org}/{database} expired: its files are gone. Send every file \
                 again (a new import_migrations call without `upload`)."
            );
        }
        ImportUploadStatus::Open => {
            let _ = writeln!(
                out,
                "Upload {id} for {org}/{database}: {} files ({}), not imported yet. Commit it: \
                 import_migrations with upload={id} (and commit: true).",
                upload.files,
                size(upload.bytes)
            );
        }
        ImportUploadStatus::Importing => {
            let _ = write!(out, "Import {id} into {org}/{database}: running, ");
            match &upload.progress {
                Some(p) if p.stage == "queued" => {
                    let _ = write!(out, "waiting for its turn (other imports replay first)");
                }
                Some(p) if p.stage == "replaying" => {
                    let _ = write!(
                        out,
                        "{} of {} migrations replayed{}",
                        p.replayed,
                        p.total,
                        p.first_number
                            .map(|n| format!(" (they become v{n} onward)"))
                            .unwrap_or_default()
                    );
                }
                Some(p) => {
                    let _ = write!(out, "{} new migrations planned", p.total);
                }
                None => {
                    let _ = write!(out, "starting");
                }
            }
            let _ = writeln!(
                out,
                ".\nStill running: call get_import with import={id} (wait_seconds up to 40) in \
                 a moment; do not import again."
            );
            return out;
        }
        ImportUploadStatus::Failed => {
            let error = upload.error.as_ref();
            let code = error.map_or("failed", |e| e.code.as_str());
            let _ = write!(
                out,
                "Import {id} into {org}/{database} stopped: {} ({code}). Nothing was stored.",
                error.map_or("", |e| e.message.as_str())
            );
            if let Some(details) = error.and_then(|e| e.details.as_ref()) {
                failure_details(&mut out, code, details);
            }
            if code == "import_interrupted" {
                let _ = writeln!(out, "\nHistory: {history}");
            } else {
                let _ = writeln!(
                    out,
                    "\nThe files stay uploaded until {}: send a corrected file with upload={id} \
                     and commit: false (it replaces the one with the same path), then commit \
                     again with upload={id}.",
                    upload.expires_at.format("%H:%M UTC")
                );
            }
            return out;
        }
        ImportUploadStatus::Checked | ImportUploadStatus::Imported => {}
    }
    let Some(report) = &upload.report else {
        return out;
    };
    report_text(&mut out, id, org, database, report, upload.dry_run);
    if upload.status == ImportUploadStatus::Checked {
        let _ = writeln!(
            out,
            "A dry run: nothing was stored. To import for real, call import_migrations with \
             upload={id} and commit: true (no files)."
        );
    } else {
        let _ = writeln!(out, "History: {history}");
    }
    out
}

/// The report, compact: the versions created, what was skipped and
/// ignored, what to know.
#[allow(
    clippy::too_many_lines,
    reason = "each part of the report, one after the other"
)]
fn report_text(
    out: &mut String,
    id: Uuid,
    org: &str,
    database: &str,
    report: &Report,
    dry_run: bool,
) {
    let new: Vec<&ReportedMigration> = report
        .migrations
        .iter()
        .filter(|m| m.status == "new")
        .collect();
    let before = report
        .migrations
        .iter()
        .filter(|m| m.status == "imported")
        .count();
    let removed: Vec<&ReportedMigration> = report
        .migrations
        .iter()
        .filter(|m| m.status == "removed")
        .collect();
    let format = serde_json::to_value(report.format)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let verb = if dry_run {
        "Dry run of import"
    } else {
        "Import"
    };
    let _ = write!(
        out,
        "{verb} {id} into {org}/{database} ({format}): {} migration{} in the files",
        report.migrations.len(),
        if report.migrations.len() == 1 {
            ""
        } else {
            "s"
        }
    );
    let numbers: Vec<i32> = new.iter().filter_map(|m| m.version).collect();
    match (report.status.as_str(), numbers.first(), numbers.last()) {
        ("created", Some(first), Some(last)) if first == last => {
            let _ = write!(out, ", 1 new as version v{first}");
        }
        ("created", Some(first), Some(last)) => {
            let _ = write!(out, ", {} new as versions v{first}–v{last}", report.created);
        }
        ("checked", _, _) => {
            let _ = write!(out, ", {} new that replay cleanly", new.len());
        }
        _ => {
            let _ = write!(out, ", nothing new");
        }
    }
    if before > 0 {
        let _ = write!(out, ", {before} imported before (unchanged, skipped)");
    }
    out.push_str(".\n");
    for m in new.iter().take(LISTED) {
        let version = m.version.map(|n| format!("v{n} ")).unwrap_or_default();
        let _ = writeln!(
            out,
            "- {version}{}: {}",
            m.path,
            m.changes.as_deref().unwrap_or("replayed")
        );
    }
    if new.len() > LISTED {
        let _ = writeln!(out, "- … and {} more", new.len() - LISTED);
    }
    let skipped: Vec<(&str, &Skipped)> = new
        .iter()
        .flat_map(|m| m.skipped.iter().map(|s| (m.path.as_str(), s)))
        .collect();
    if !skipped.is_empty() {
        let files = new.iter().filter(|m| !m.skipped.is_empty()).count();
        let _ = writeln!(
            out,
            "Skipped statements (reported, not replayed: not part of a schema): {} in {files} \
             file{}:",
            skipped.len(),
            if files == 1 { "" } else { "s" }
        );
        for (path, s) in skipped.iter().take(LISTED) {
            let _ = writeln!(
                out,
                "- {path} line {}: {} ({}): {}",
                s.line,
                s.kind,
                s.reason.replace('_', " "),
                one_line(&s.text, 120)
            );
        }
        if skipped.len() > LISTED {
            let _ = writeln!(out, "- … and {} more", skipped.len() - LISTED);
        }
    }
    if !report.ignored.is_empty() {
        let shown: Vec<String> = report
            .ignored
            .iter()
            .take(20)
            .map(|i| format!("{} ({})", i.path, i.reason.replace('_', " ")))
            .collect();
        let _ = writeln!(
            out,
            "Ignored files: {}{}",
            shown.join(", "),
            if report.ignored.len() > 20 {
                format!(", and {} more", report.ignored.len() - 20)
            } else {
                String::new()
            }
        );
    }
    for m in &removed {
        let _ = writeln!(
            out,
            "Warning: {} was imported as v{} once, and that version was removed in romulus: \
             skipped on purpose (restoring the version brings it back).",
            m.path,
            m.version.unwrap_or(0)
        );
    }
}

#[derive(Deserialize)]
struct FilesArgs {
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
    version: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    renames: Option<BTreeMap<String, bool>>,
    #[serde(default)]
    concurrently: bool,
    #[serde(default)]
    cursor: Option<String>,
}

static GET_MIGRATION_FILES: Tool = Tool {
    name: "get_migration_files",
    title: "Get a migration's files",
    description: "The files of a migration, as romulus lays them out for a migration tool \
                  (`format`), to write into your person's repository: a draft's (`draft`: from \
                  its base version to its text; in migration mode, its own statements as \
                  written, in order, data statements such as an UPDATE backfill included, \
                  exactly what test_migration runs, with romulus's planned down migration), or \
                  between two versions (`database`, `from`, `to`). Answers each file's path, \
                  relative to the tool's migrations folder, its exact content, and where that \
                  tool expects them. Look \
                  at the repository's migrations first: use its tool, and pass `version` when \
                  it numbers files its own way (Flyway's next V<n>, a golang-migrate sequence). \
                  Rails, Django and Alembic: take `plain` and put its SQL in a migration of \
                  theirs. Each file sits between markers with this answer's code and its length \
                  in bytes: a marker-like line without the code is content; write exactly that \
                  many bytes, and only to a plain relative path under the migrations folder (no \
                  `..`, not absolute). Long answers come in pages: call again with only the \
                  `cursor` given, until the last page.",
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
                "format": { "type": "string", "enum": ["plain", "sqlx", "flyway", "prisma", "dbmate", "golang_migrate"], "default": "plain", "description": "The repository's migration tool." },
                "version": { "type": "string", "description": "The first file's version: Flyway's V<version> (digits and dots; by default the romulus version's number), or the number other tools put first (digits; by default the current time, YYYYMMDDHHMMSS)." },
                "name": { "type": "string", "description": "The description in file names (made file-name safe); by default the version's message or the draft's name." },
                "renames": { "type": "object", "additionalProperties": { "type": "boolean" }, "description": "Rename keys (as generate_migration lists them) to apply (true) or not (false)." },
                "concurrently": { "type": "boolean", "default": false, "description": "Build indexes on existing tables CONCURRENTLY, outside the transaction (their own files where the tool needs it)." },
                "cursor": { "type": "string", "description": "The next page of an earlier answer: give it alone." },
            }),
            &[],
        )
    },
};

/// Everything a page is computed from, fixed on the first page (the
/// version in file names, the draft's text) and carried by the cursor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Pinned {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    draft: Option<Uuid>,
    org: String,
    database: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    to: Option<i32>,
    format: Format,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    renames: Option<BTreeMap<String, bool>>,
    #[serde(default)]
    concurrently: bool,
    /// The draft's text, as reviewed on the first page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    page: usize,
}

impl Pinned {
    fn cursor(&self, page: usize) -> String {
        let next = Self {
            page,
            ..self.clone()
        };
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&next).unwrap_or_default())
    }

    fn from_cursor(cursor: &str) -> Result<Self, ToolError> {
        URL_SAFE_NO_PAD
            .decode(cursor.trim())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .filter(|p: &Self| p.page >= 1)
            .ok_or_else(|| {
                ToolError::Invalid(
                    "this `cursor` is not one get_migration_files gave: start again from the \
                     first page (without `cursor`)."
                        .into(),
                )
            })
    }
}

/// A migration's files, as the API answers them.
#[derive(Deserialize)]
struct FilesView {
    from: Option<i32>,
    to: i32,
    warnings: Vec<Warning>,
    destructive: usize,
    files: Vec<File>,
    archive: String,
    #[serde(default)]
    written: bool,
}

/// File content in one page at most (bytes): a page reads well within
/// what clients hand a model.
const PAGE_BUDGET: usize = 50_000;

#[allow(
    clippy::too_many_lines,
    reason = "the first page and the later ones, then the page's text"
)]
async fn get_migration_files(ctx: &Ctx, a: FilesArgs) -> Result<Output, ToolError> {
    let pinned = match &a.cursor {
        Some(cursor) => Pinned::from_cursor(cursor)?,
        None => pin(ctx, &a).await?,
    };
    let mut query = vec![("format", pinned.format.as_str().to_owned())];
    if let Some(version) = &pinned.version {
        query.push(("version", version.clone()));
    }
    if let Some(name) = &pinned.name {
        query.push(("name", name.clone()));
    }
    if pinned.concurrently {
        query.push(("concurrently", "true".into()));
    }
    if let Some(renames) = &pinned.renames {
        query.push(("renames", json!(renames).to_string()));
    }
    let base = [
        "orgs",
        pinned.org.as_str(),
        "databases",
        pinned.database.as_str(),
    ];
    let (view, what, link): (FilesView, String, String) = if let Some(draft) = pinned.draft {
        let id = draft.to_string();
        query.push(("sha256", pinned.sha256.clone().unwrap_or_default()));
        let view = ctx
            .api
            .get(&[&base[..], &["drafts", &id, "migration"]].concat(), &query)
            .await
            .map_err(|failure| changed_draft(failure, pinned.page))?;
        let link = ctx.link(&format!("{}/{}/drafts/{id}", pinned.org, pinned.database));
        (view, format!("the draft {id}"), link)
    } else {
        let (from, to) = (pinned.from.unwrap_or(0), pinned.to.unwrap_or(0));
        query.push(("from", from.to_string()));
        query.push(("to", to.to_string()));
        let view = ctx
            .api
            .get(&[&base[..], &["migration"]].concat(), &query)
            .await?;
        let link = ctx.link(&format!(
            "{}/{}/diff/{from}...{to}?view=migration",
            pinned.org, pinned.database
        ));
        (view, format!("v{from} → v{to}"), link)
    };
    let pages = paginate(&view.files, PAGE_BUDGET);
    let Some(page) = pages.get(pinned.page - 1) else {
        return Err(ToolError::Invalid(format!(
            "there are {} pages: this cursor points past them; start again without `cursor`.",
            pages.len()
        )));
    };
    let mut out = String::new();
    if pinned.page == 1 {
        let from = view
            .from
            .map_or_else(|| "nothing".to_owned(), |n| format!("v{n}"));
        let _ = writeln!(
            out,
            "Migration files of {}/{}, {what} ({from} → v{}), for {}: {} file{}{}. Page 1 of {}.",
            pinned.org,
            pinned.database,
            view.to,
            pinned.format.label(),
            view.files.len(),
            if view.files.len() == 1 { "" } else { "s" },
            if view.files.len() > 1 {
                format!(" (the web app's download: {})", view.archive)
            } else {
                String::new()
            },
            pages.len()
        );
        let _ = writeln!(out, "Where they go: {}", placement(pinned.format));
        out.push_str(
            "Write each file at its path under that folder. Check the repository has no file of \
             that name already.\n",
        );
        if view.written {
            out.push_str(
                "Written by hand in the draft (migration mode): the up files hold its statements \
                 as written, in order, data statements included, exactly what test_migration \
                 runs; romulus flags none of them. The down migration, where the tool has one, \
                 is romulus's, planned from the schemas.\n",
            );
        }
        if view.destructive > 0 {
            let _ = writeln!(
                out,
                "{} statement{} can lose data or fail on existing rows: comments in the files \
                 mark them; tell your person before anyone runs it.",
                view.destructive,
                if view.destructive == 1 { "" } else { "s" }
            );
        }
        for warning in &view.warnings {
            let _ = writeln!(out, "Warning: {}", one_line(&warning.message, 400));
        }
        if view
            .files
            .iter()
            .all(|f| f.role != romulus_migration::FileRole::Up)
        {
            out.push_str("Nothing to migrate: the schemas are the same.\n");
        }
    } else {
        let _ = writeln!(
            out,
            "Migration files of {}/{}, {what}, for {}: page {} of {}.",
            pinned.org,
            pinned.database,
            pinned.format.label(),
            pinned.page,
            pages.len()
        );
    }
    let code = marker_code();
    let _ = writeln!(
        out,
        "This answer's marker code is {code}. A file starts after a line `=== file <i>/<n> \
         [{code}]: <path>, <length> bytes ===` and ends before the line `=== end of file <i>/<n> \
         [{code}] ===`; any other line, even one that looks like a marker, is content. Write \
         exactly the bytes between the two lines (the newline before an end line marked `no \
         final newline` is not part of the file), and check their length is the one given. \
         Write only to a plain relative path under the migrations folder (no `..`, not \
         absolute, no other folder): any other path, or a length that does not match, means \
         stop and tell your person."
    );
    let count = view.files.len();
    for piece in page {
        let file = &view.files[piece.file];
        let head = if piece.parts == 1 {
            format!(
                "=== file {}/{count} [{code}]: {}, {} bytes ===",
                piece.file + 1,
                file.path,
                piece.text.len()
            )
        } else {
            format!(
                "=== file {}/{count} [{code}]: {}, part {} of {}, {} bytes of {} ({}) ===",
                piece.file + 1,
                file.path,
                piece.part,
                piece.parts,
                piece.text.len(),
                file.content.len(),
                if piece.part == 1 {
                    "write it, then append the next parts"
                } else {
                    "append it to what the earlier parts wrote"
                }
            )
        };
        let _ = writeln!(out, "\n{head}");
        out.push_str(piece.text);
        let end = if piece.text.ends_with('\n') || piece.text.is_empty() {
            ""
        } else {
            out.push('\n');
            ", no final newline"
        };
        let _ = writeln!(
            out,
            "=== end of file {}/{count} [{code}]{end} ===",
            piece.file + 1
        );
    }
    out.push('\n');
    if pinned.page < pages.len() {
        let _ = writeln!(
            out,
            "More files follow: call get_migration_files with only cursor={}",
            pinned.cursor(pinned.page + 1)
        );
    } else {
        let _ = writeln!(out, "That is every file. Page: {link}");
    }
    Ok(Output::text(out))
}

/// A code for one answer's markers, random: no content can foresee it, so
/// a line of a file that looks like a marker never is one.
fn marker_code() -> String {
    token::generate()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect()
}

/// The first page's choices, fixed for the next ones.
async fn pin(ctx: &Ctx, a: &FilesArgs) -> Result<Pinned, ToolError> {
    let format = match a.format.as_deref().map(str::trim) {
        None | Some("") => Format::Plain,
        Some(name) => serde_json::from_value(json!(name)).map_err(|_| {
            ToolError::Invalid(format!(
                "`format` {name:?}: use plain, sqlx, flyway, prisma, dbmate or golang_migrate \
                 (plain for Rails, Django and Alembic: put its SQL in a migration of theirs)."
            ))
        })?,
    };
    // The time in file names, fixed now: every page names files alike.
    let version = a
        .version
        .clone()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            (format != Format::Flyway).then(|| Utc::now().format("%Y%m%d%H%M%S").to_string())
        });
    let mut pinned = Pinned {
        draft: None,
        org: String::new(),
        database: String::new(),
        from: None,
        to: None,
        format,
        version,
        name: a.name.clone().filter(|n| !n.trim().is_empty()),
        renames: a.renames.clone(),
        concurrently: a.concurrently,
        sha256: None,
        page: 1,
    };
    match (&a.draft, &a.database, a.from, a.to) {
        (Some(draft), None, None, None) => {
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
            pinned.draft = Some(id);
            pinned.org = place.org;
            pinned.database = place.database;
            pinned.sha256 = Some(text.reviewed_sha256);
        }
        (None, Some(database), Some(from), Some(to)) => {
            pinned.org = ctx.org(a.org.as_deref()).await?;
            pinned.database = database.clone();
            pinned.from = Some(from);
            pinned.to = Some(to);
        }
        _ => {
            return Err(ToolError::Invalid(
                "give a `draft`, or a `database` with `from` and `to` (version numbers).".into(),
            ));
        }
    }
    Ok(pinned)
}

/// A draft that changed between pages: the files written so far are of
/// another text.
fn changed_draft(failure: Failure, page: usize) -> ToolError {
    if failure.code() == "draft_changed" && page > 1 {
        return ToolError::Say(format!(
            "{} (draft_changed)\nThe draft was edited since the first page: the files already \
             written are of its earlier text. Delete them, then start again from the first page \
             (get_migration_files with `draft`, no cursor).",
            failure.body.message
        ));
    }
    ToolError::Api(failure)
}

/// Where each tool expects its files, for an agent writing them.
fn placement(format: Format) -> &'static str {
    match format {
        Format::Plain => {
            "one .sql file that runs in its own transaction (BEGIN … COMMIT): where the project \
             keeps SQL migrations, or run as is with psql -v ON_ERROR_STOP=1 -f. For Rails, \
             Django or Alembic, write a new migration of theirs named like the existing ones \
             (Rails: db/migrate/<YYYYMMDDHHMMSS>_<name>.rb with execute <<~SQL; Django: \
             migrations.RunSQL; Alembic: op.execute) holding these statements without BEGIN and \
             COMMIT (the tool runs its own transaction)."
        }
        Format::Sqlx => {
            "sqlx's migrations folder, migrations/ next to Cargo.toml (or the one given to \
             sqlx::migrate! or --source). Files <version>_<name>.sql run in order by sqlx \
             migrate run, each in a transaction unless its first line is -- no-transaction."
        }
        Format::Flyway => {
            "Flyway's location: src/main/resources/db/migration/ in a Maven or Gradle project \
             (classpath:db/migration), else the folder flyway.locations names (sql/ for the \
             command line). Files V<version>__<name>.sql; a .sql.conf beside one runs it \
             outside a transaction. The version must sort after every V file the project has: \
             if it does not, ask again with `version` set to the project's next number."
        }
        Format::Prisma => {
            "Prisma's migrations folder, prisma/migrations/ (next to schema.prisma, with \
             migration_lock.toml). Each migration is a folder <timestamp>_<name>/ holding its \
             migration.sql; prisma migrate deploy applies it. Update the models in \
             schema.prisma to match (or prisma db pull once applied), or prisma migrate dev \
             reports a drift."
        }
        Format::Dbmate => {
            "dbmate's migrations folder, db/migrations/ unless DBMATE_MIGRATIONS_DIR or \
             --migrations-dir says otherwise. Each file holds a -- migrate:up and a -- \
             migrate:down section."
        }
        Format::GolangMigrate => {
            "the folder the project gives migrate -path (often migrations/ or db/migrations/). \
             Each migration is a pair, <version>_<name>.up.sql and .down.sql; if the project \
             numbers them in sequence (000042_…), ask again with `version` set to its next \
             number."
        }
    }
}

/// A file, or a part of one too long for a page.
#[derive(Debug, PartialEq, Eq)]
struct Piece<'a> {
    /// Its index in the files.
    file: usize,
    /// 1-based, of `parts`.
    part: usize,
    parts: usize,
    text: &'a str,
}

/// The files in pages of `budget` bytes of content at most (a file longer
/// than that in parts, cut after a line, or mid-line for a line longer than
/// the budget), in order. Always one page at least.
fn paginate(files: &[File], budget: usize) -> Vec<Vec<Piece<'_>>> {
    let mut pages: Vec<Vec<Piece<'_>>> = vec![Vec::new()];
    let mut used = 0;
    for (index, file) in files.iter().enumerate() {
        let chunks = chunks(&file.content, budget);
        let parts = chunks.len();
        for (k, text) in chunks.into_iter().enumerate() {
            if used + text.len() > budget && pages.last().is_some_and(|p| !p.is_empty()) {
                pages.push(Vec::new());
                used = 0;
            }
            used += text.len();
            if let Some(page) = pages.last_mut() {
                page.push(Piece {
                    file: index,
                    part: k + 1,
                    parts,
                    text,
                });
            }
        }
    }
    pages
}

/// `text` in chunks of `budget` bytes at most, cut after a newline when
/// one is in reach (on a character boundary otherwise). One chunk (maybe
/// empty) at least.
fn chunks(text: &str, budget: usize) -> Vec<&str> {
    let budget = budget.max(4);
    let mut out = Vec::new();
    let mut rest = text;
    while rest.len() > budget {
        let mut cut = budget;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        if let Some(newline) = rest[..cut].rfind('\n') {
            cut = newline + 1;
        }
        out.push(&rest[..cut]);
        rest = &rest[cut..];
    }
    out.push(rest);
    out
}

#[cfg(test)]
mod tests {
    use romulus_migration::FileRole;

    use super::*;

    fn file(path: &str, content: &str) -> File {
        File {
            path: path.into(),
            content: content.into(),
            role: FileRole::Up,
            marks: Vec::new(),
        }
    }

    #[test]
    fn long_files_are_cut_after_a_line_and_joined_back_whole() {
        let text = "CREATE TABLE a ();\n".repeat(10);
        let parts = chunks(&text, 40);
        assert!(parts.iter().all(|p| p.len() <= 40 && p.ends_with('\n')));
        assert_eq!(parts.concat(), text);
        // A line longer than the budget is cut mid-line, never mid-character.
        let wide = format!("-- {}\n", "é".repeat(30));
        let parts = chunks(&wide, 7);
        assert!(parts.iter().all(|p| p.len() <= 7));
        assert_eq!(parts.concat(), wide);
        assert_eq!(chunks("", 10), [""]);
    }

    #[test]
    fn pages_hold_whole_files_when_they_fit_and_parts_otherwise() {
        let files = [
            file("a.sql", &"a\n".repeat(10)),
            file("b.sql", &"b\n".repeat(10)),
            file("c.sql", &"c\n".repeat(30)),
        ];
        let pages = paginate(&files, 40);
        let shape: Vec<Vec<(usize, usize, usize)>> = pages
            .iter()
            .map(|page| page.iter().map(|p| (p.file, p.part, p.parts)).collect())
            .collect();
        assert_eq!(
            shape,
            [vec![(0, 1, 1), (1, 1, 1)], vec![(2, 1, 2)], vec![(2, 2, 2)],]
        );
        let c: String = pages
            .iter()
            .flatten()
            .filter(|p| p.file == 2)
            .map(|p| p.text)
            .collect();
        assert_eq!(c, files[2].content);
        assert_eq!(paginate(&[], 40).len(), 1);
    }

    #[test]
    fn a_cursor_carries_every_choice_and_refuses_what_it_did_not_make() {
        let pinned = Pinned {
            draft: Some(Uuid::nil()),
            org: "acme".into(),
            database: "shop".into(),
            from: None,
            to: None,
            format: Format::Prisma,
            version: Some("20261021093000".into()),
            name: None,
            renames: Some(BTreeMap::from([("table/public/a/b".into(), true)])),
            concurrently: true,
            sha256: Some("ab".repeat(32)),
            page: 1,
        };
        let next = Pinned::from_cursor(&pinned.cursor(2)).unwrap();
        assert_eq!(next, Pinned { page: 2, ..pinned });
        assert!(Pinned::from_cursor("not a cursor").is_err());
        assert!(Pinned::from_cursor(&next.cursor(0)).is_err());
    }

    #[test]
    fn formats_are_named_as_the_import_names_them() {
        assert_eq!(import_format(Some("auto")).unwrap(), None);
        assert_eq!(import_format(None).unwrap(), None);
        assert_eq!(
            import_format(Some("golang_migrate")).unwrap(),
            Some(MigrationFormat::GolangMigrate)
        );
        assert_eq!(
            import_format(Some("rails")).unwrap(),
            Some(MigrationFormat::Rails)
        );
        assert!(import_format(Some("liquibase")).is_err());
    }
}
