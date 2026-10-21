//! Migration files sent a few at a time, then imported (`import_uploads`):
//! how an agent brings a repository's migrations in over MCP, where one
//! message carries 4 MiB at most and a call has a minute. The person starts
//! an upload with some files, adds the rest, then imports them: the same
//! import as the web's and CI's ([`run::prepare`]: the plan, its refusals,
//! the caps, one version per migration), run in its own task. The upload
//! then follows it ([`record`]: its progress, then its report or its
//! error), and is read back with `GET` until it ends. Nothing else differs
//! from an import of the same files in one request.
//!
//! Endpoints (who may call them in brackets; an upload is its starter's
//! alone, anyone else gets 404):
//! - `POST /orgs/{org}/databases/{db}/imports/uploads` [developer]: starts
//!   one, with files or none.
//! - `POST …/imports/uploads/{id}/files` [developer]: adds files. A path
//!   (and part) sent again replaces what was sent, so a retried request
//!   changes nothing; a file larger than a request is sent in parts
//!   (`part` 1, 2…), joined in order when imported.
//! - `POST …/imports/uploads/{id}/import` [developer]: imports the files
//!   (a JSON body: `format`, `append`, `dry_run`, as for an import). What the import
//!   refuses before it runs is answered at once (409, 402, 422); otherwise
//!   202 and the import runs on.
//! - `GET …/imports/uploads/{id}` [member]: where it stands.
//!
//! The files stay until an import stores them: after a refusal, a failure
//! or a dry run, fix a file (send it again) and import again. An upload
//! expires an hour after its last files or its last import ended (a day
//! once imported); expired, it answers 409 `upload_expired` for a day, then
//! is gone ([`sweep`]). A person has [`MAX_OPEN`] uploads in progress at
//! most.

use axum::{
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use romulus_sandbox::import::SourceFile;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use tokio::sync::mpsc;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{
    ImportEvent, ImportOptions, ImportPreview, ImportReport,
    run::{self, Author, Target},
    upload,
};
use crate::{
    ApiError, ApiResult, AppState,
    auth::{Member, min},
    databases,
    domain::{ImportUploadStatus, MigrationFormat},
    error::ErrorBody,
    extract::{Json, Path},
    orgs::invalid,
};

/// One request's body at most: files sent through MCP (4 MiB a message)
/// arrive here re-encoded, with room to spare.
const CHUNK_LIMIT: usize = 8 << 20;
/// Parts of one file, at most.
const MAX_PARTS: i32 = 100;
/// A person's uploads in progress (not imported, not expired), at most.
pub const MAX_OPEN: i64 = 3;
/// An import silent this long (no progress) was lost with its process.
const LOST_AFTER_MINUTES: i32 = 30;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(start_upload))
        .routes(routes!(add_files))
        .routes(routes!(import_upload))
        .routes(routes!(get_upload))
        .layer(DefaultBodyLimit::max(CHUNK_LIMIT))
}

/// A file, or a part of one.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UploadedFile {
    /// Its path relative to the repository (or to the migrations folder),
    /// with `/`: `db/migration/V2__add_invoices.sql`,
    /// `prisma/migrations/20240101000000_init/migration.sql`. The import
    /// reads the tool and each migration's name from paths. Never absolute,
    /// never `..`.
    pub path: String,
    /// The file's text (UTF-8), exactly as it is on disk.
    pub content: String,
    /// For a file too large for one request: which part this is, 1, 2…
    /// (joined in order). 1 by default.
    #[serde(default)]
    pub part: Option<i32>,
}

/// Files to add.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct UploadFiles {
    #[serde(default)]
    pub files: Vec<UploadedFile>,
}

/// How to import an upload's files (all optional: a bare `POST` detects
/// the format and refuses to append).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct UploadImportOptions {
    /// Read the files as this tool's; detected from them when absent.
    pub format: Option<MigrationFormat>,
    /// Import even though the database has versions that were not
    /// imported: the imported ones continue the numbering.
    #[serde(default)]
    pub append: bool,
    /// Replay without storing anything: the files are kept, to import for
    /// real next.
    #[serde(default)]
    pub dry_run: bool,
}

/// Where a running import is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportStage {
    /// Planned: it waits for the sandbox.
    Planned,
    /// Other imports replay first (organizations take turns).
    Queued,
    /// The migrations replay, one version each.
    Replaying,
}

/// A running import's progress.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportProgress {
    pub stage: ImportStage,
    /// New migrations replayed so far…
    pub replayed: usize,
    /// …of these many (the versions it creates).
    pub total: usize,
    /// The number the first new version gets.
    pub first_number: Option<i32>,
}

impl ImportProgress {
    fn planned(preview: &ImportPreview) -> Self {
        Self {
            stage: ImportStage::Planned,
            replayed: 0,
            total: preview.new,
            first_number: preview.first_number,
        }
    }
}

/// An upload: its files, and its import once asked for.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportUpload {
    pub id: Uuid,
    pub status: ImportUploadStatus,
    /// Files received (one sent in parts counts once), and their size.
    pub files: i64,
    pub bytes: i64,
    /// The last import asked was a dry run.
    pub dry_run: bool,
    pub created_at: DateTime<Utc>,
    /// Past it, the files are gone: start again.
    pub expires_at: DateTime<Utc>,
    pub expired: bool,
    /// While importing.
    pub progress: Option<ImportProgress>,
    /// How the last import ended (`checked`, `imported`).
    #[schema(value_type = Option<ImportReport>)]
    pub report: Option<Value>,
    /// Why the last import was refused or stopped (`failed`): the import's
    /// own errors (`import_failed` with an `ImportFailure`, `has_versions`…).
    pub error: Option<ErrorBody>,
}

/// Starts an upload of migration files, with some or none. Developers and
/// admins. See the module documentation.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/imports/uploads",
    operation_id = "start_import_upload",
    tag = "imports",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
    ),
    request_body = UploadFiles,
    responses(
        (status = 201, body = ImportUpload),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`too_many_uploads`", body = ErrorBody),
        (status = 413, description = "`payload_too_large`: send fewer files per request", body = ErrorBody),
        (status = 422, description = "`validation` (a path, a part, too many or too large files), `invalid_file`", body = ErrorBody),
        (status = 429, description = "`rate_limited`", body = ErrorBody),
    )
)]
async fn start_upload(
    member: Member<min::Developer>,
    State(state): State<AppState>,
    Path((_, slug)): Path<(String, String)>,
    Json(body): Json<UploadFiles>,
) -> ApiResult<(StatusCode, Json<ImportUpload>)> {
    let database = databases::find(&state.db, member.org.id, &slug).await?;
    state.limits.import_uploads.check(member.user.id)?;
    let files = checked(body.files)?;
    let mut tx = state.db.begin().await?;
    let open = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM import_uploads
           WHERE created_by = $1 AND status <> 'imported' AND expires_at > now()"#,
        member.user.id,
    )
    .fetch_one(&mut *tx)
    .await?;
    if open >= MAX_OPEN {
        return Err(ApiError::Conflict {
            code: "too_many_uploads",
            message: format!(
                "You have {open} uploads of migrations in progress: import one (or let it expire, an hour after its last files) before starting another."
            ),
        });
    }
    let id = sqlx::query_scalar!(
        "INSERT INTO import_uploads (database_id, created_by, expires_at)
         VALUES ($1, $2, now() + interval '1 hour') RETURNING id",
        database.id,
        member.user.id,
    )
    .fetch_one(&mut *tx)
    .await?;
    add(&mut tx, id, files).await?;
    tx.commit().await?;
    let upload = load(&state.db, database.id, id, member.user.id).await?;
    Ok((StatusCode::CREATED, Json(upload)))
}

/// Adds files to an upload (a path and part sent again replaces it).
/// Developers and admins, the upload's starter only.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/imports/uploads/{id}/files",
    operation_id = "add_import_files",
    tag = "imports",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        ("id" = Uuid, Path, description = "The upload's id"),
    ),
    request_body = UploadFiles,
    responses(
        (status = 200, body = ImportUpload),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`upload_expired`, `upload_busy` (its import runs), `upload_closed` (imported)", body = ErrorBody),
        (status = 413, description = "`payload_too_large`: send fewer files per request", body = ErrorBody),
        (status = 422, description = "`validation` (a path, a part, too many or too large files), `invalid_file`", body = ErrorBody),
        (status = 429, description = "`rate_limited`", body = ErrorBody),
    )
)]
async fn add_files(
    member: Member<min::Developer>,
    State(state): State<AppState>,
    Path((_, slug, id)): Path<(String, String, Uuid)>,
    Json(body): Json<UploadFiles>,
) -> ApiResult<Json<ImportUpload>> {
    let database = databases::find(&state.db, member.org.id, &slug).await?;
    state.limits.import_uploads.check(member.user.id)?;
    let files = checked(body.files)?;
    let mut tx = state.db.begin().await?;
    claim(&mut tx, database.id, id, member.user.id).await?;
    add(&mut tx, id, files).await?;
    sqlx::query!(
        "UPDATE import_uploads
         SET status = 'open', progress = NULL, outcome = NULL, updated_at = now(),
             expires_at = now() + interval '1 hour'
         WHERE id = $1",
        id,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(
        load(&state.db, database.id, id, member.user.id).await?,
    ))
}

/// Imports an upload's files: what the import refuses is answered at once,
/// otherwise it runs on (202); read the upload to follow it. Developers and
/// admins, the upload's starter only.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/imports/uploads/{id}/import",
    operation_id = "import_upload",
    tag = "imports",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        ("id" = Uuid, Path, description = "The upload's id"),
    ),
    request_body(content = Option<UploadImportOptions>, description = "How to import them; none: the format detected, no append"),
    responses(
        (status = 202, description = "The import runs: read the upload to follow it", body = ImportUpload),
        (status = 402, description = "`plan_limit`: the versions would pass the plan's cap; nothing stored", body = ErrorBody),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`upload_expired`, `upload_busy`, `upload_closed`; the import's own: `has_versions` (pass `append=true`), `migration_changed`, `migration_out_of_order`, `concurrent_import`", body = ErrorBody),
        (status = 422, description = "`incomplete_upload` (a part missing), `invalid_migrations`, `import_failed` (a syntax error), `too_many_migrations`, `validation`", body = ErrorBody),
        (status = 429, description = "`rate_limited`", body = ErrorBody),
    )
)]
async fn import_upload(
    member: Member<min::Developer>,
    State(state): State<AppState>,
    Path((_, slug, id)): Path<(String, String, Uuid)>,
    options: Option<Json<UploadImportOptions>>,
) -> ApiResult<(StatusCode, Json<ImportUpload>)> {
    let options = options.map(|Json(options)| options).unwrap_or_default();
    let database = databases::find(&state.db, member.org.id, &slug).await?;
    state.limits.compile.check(member.user.id)?;
    state.limits.imports.check(database.id)?;
    let mut tx = state.db.begin().await?;
    claim(&mut tx, database.id, id, member.user.id).await?;
    let files = files(&mut tx, id).await?;
    sqlx::query!(
        "UPDATE import_uploads
         SET status = 'importing', dry_run = $2, progress = NULL, outcome = NULL,
             updated_at = now()
         WHERE id = $1",
        id,
        options.dry_run,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let target = Target {
        org_id: member.org.id,
        database_id: database.id,
        slug: database.slug,
        major: database.pg_major,
    };
    let dry_run = options.dry_run;
    let options = ImportOptions {
        format: options.format,
        append: options.append,
        dry_run,
        filename: None,
    };
    let job = match run::prepare(
        state.clone(),
        target,
        Author::User(member.user.id),
        files,
        options,
    )
    .await
    {
        Ok(job) => job,
        Err(err) => {
            failed(&state.db, id, &err.body()).await;
            return Err(err);
        }
    };
    let progress = ImportProgress::planned(job.preview());
    save_progress(&state.db, id, &progress).await;
    tokio::spawn(record(state.db.clone(), id, job.spawn(), progress, dry_run));
    let upload = load(&state.db, database.id, id, member.user.id).await?;
    Ok((StatusCode::ACCEPTED, Json(upload)))
}

/// An upload: its files, and its import's progress or outcome. Its starter
/// only.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/imports/uploads/{id}",
    operation_id = "get_import_upload",
    tag = "imports",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        ("id" = Uuid, Path, description = "The upload's id"),
    ),
    responses(
        (status = 200, body = ImportUpload),
        (status = 404, body = ErrorBody),
    )
)]
async fn get_upload(
    member: Member,
    State(state): State<AppState>,
    Path((_, slug, id)): Path<(String, String, Uuid)>,
) -> ApiResult<Json<ImportUpload>> {
    let database = databases::find(&state.db, member.org.id, &slug).await?;
    Ok(Json(
        load(&state.db, database.id, id, member.user.id).await?,
    ))
}

fn no_such_upload() -> ApiError {
    ApiError::not_found(
        "No such upload of migrations: it may have expired a while ago, or be someone else's.",
    )
}

/// Locks the upload for a change: it must be the person's, unexpired, and
/// take files (not importing, not imported).
async fn claim(
    tx: &mut Transaction<'_, Postgres>,
    database_id: Uuid,
    id: Uuid,
    user: Uuid,
) -> ApiResult<()> {
    let row = sqlx::query!(
        r#"SELECT status AS "status: ImportUploadStatus", expires_at <= now() AS "expired!"
           FROM import_uploads WHERE id = $1 AND database_id = $2 AND created_by = $3
           FOR UPDATE"#,
        id,
        database_id,
        user,
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(no_such_upload)?;
    match row.status {
        ImportUploadStatus::Importing => Err(ApiError::Conflict {
            code: "upload_busy",
            message: "These files are being imported: wait for the import to end.".into(),
        }),
        ImportUploadStatus::Imported => Err(ApiError::Conflict {
            code: "upload_closed",
            message: "These files were imported already: start a new upload for more.".into(),
        }),
        _ if row.expired => Err(ApiError::Conflict {
            code: "upload_expired",
            message: "This upload expired (an hour after its last files) and its files are gone: start a new one and send every file again.".into(),
        }),
        _ => Ok(()),
    }
}

/// A file (or part) to store, checked.
#[derive(Debug, PartialEq, Eq)]
struct Checked {
    path: String,
    part: i32,
    content: String,
}

/// The files as stored: paths made relative with `/`, the last of a path
/// and part sent twice kept.
fn checked(files: Vec<UploadedFile>) -> ApiResult<Vec<Checked>> {
    let mut out: std::collections::BTreeMap<(String, i32), String> =
        std::collections::BTreeMap::new();
    for file in files {
        let path = relative_path(&file.path)?;
        let part = file.part.unwrap_or(1);
        if !(1..=MAX_PARTS).contains(&part) {
            return Err(invalid(
                "part",
                &format!("{path}: `part` goes from 1 to {MAX_PARTS}."),
            ));
        }
        if file.content.contains('\0') {
            return Err(ApiError::Unprocessable {
                code: "invalid_file",
                message: format!(
                    "{path} holds a NUL character: it is not a text file. Send only the migrations folder's text files."
                ),
                details: Some(serde_json::json!({ "path": path })),
            });
        }
        out.insert((path, part), file.content);
    }
    Ok(out
        .into_iter()
        .map(|((path, part), content)| Checked {
            path,
            part,
            content,
        })
        .collect())
}

/// `path` relative, with `/` (`./a\b.sql` → `a/b.sql`), or 422: never
/// absolute, never `..` (a path is kept with its version, and must say
/// where the file is in the project, not on someone's disk).
fn relative_path(path: &str) -> ApiResult<String> {
    let refuse = |why: &str| {
        Err(invalid(
            "path",
            &format!(
                "{why}: send each file's path relative to the repository (or its migrations folder), like db/migrate/V1__init.sql."
            ),
        ))
    };
    let mut path = path.trim().replace('\\', "/");
    while let Some(rest) = path.strip_prefix("./") {
        path = rest.to_owned();
    }
    if path.is_empty() {
        return refuse("A file has no path");
    }
    if path.chars().any(char::is_control) {
        return refuse(&format!("{path:?} holds control characters"));
    }
    let drive = path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic();
    if path.starts_with('/') || path.starts_with('~') || drive {
        return refuse(&format!("{path} is absolute"));
    }
    if path
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return refuse(&format!("{path} is not a plain relative path"));
    }
    if path.chars().count() > 1_000 {
        return refuse("A path is longer than 1000 characters");
    }
    Ok(path)
}

/// Stores `files` in the upload, then checks what it holds against what an
/// import reads (the transaction is dropped otherwise).
async fn add(tx: &mut Transaction<'_, Postgres>, id: Uuid, files: Vec<Checked>) -> ApiResult<()> {
    if !files.is_empty() {
        let (mut paths, mut parts, mut contents) = (Vec::new(), Vec::new(), Vec::new());
        for file in files {
            paths.push(file.path);
            parts.push(file.part);
            contents.push(file.content);
        }
        sqlx::query!(
            "INSERT INTO import_upload_files (upload_id, path, part, content)
             SELECT $1::uuid, * FROM unnest($2::text[], $3::int[], $4::text[])
             ON CONFLICT (upload_id, path, part) DO UPDATE SET content = EXCLUDED.content",
            id,
            &paths,
            &parts,
            &contents,
        )
        .execute(&mut **tx)
        .await?;
    }
    let held = sqlx::query!(
        r#"SELECT count(DISTINCT path) AS "files!",
                  coalesce(sum(octet_length(content)), 0)::bigint AS "bytes!",
                  (SELECT path FROM import_upload_files WHERE upload_id = $1
                   GROUP BY path HAVING sum(octet_length(content)) > $2 LIMIT 1) AS too_large
           FROM import_upload_files WHERE upload_id = $1"#,
        id,
        i64::try_from(upload::FILE_LIMIT).unwrap_or(i64::MAX),
    )
    .fetch_one(&mut **tx)
    .await?;
    if usize::try_from(held.files).unwrap_or(usize::MAX) > upload::MAX_FILES {
        return Err(upload::too_many_files());
    }
    if let Some(path) = held.too_large {
        return Err(upload::file_too_large(&path, upload::FILE_LIMIT));
    }
    // An upload's files together, at most: what one import request carries.
    if held.bytes > i64::try_from(upload::BODY_LIMIT).unwrap_or(i64::MAX) {
        return Err(invalid(
            "files",
            &format!(
                "These files add up to more than {} MiB; an import reads at most that. Send only the migrations folder.",
                upload::BODY_LIMIT >> 20
            ),
        ));
    }
    Ok(())
}

/// The upload's files, parts joined, or 422 when a part is missing or there
/// is no file.
async fn files(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> ApiResult<Vec<SourceFile>> {
    let rows = sqlx::query!(
        r#"SELECT path, array_agg(part ORDER BY part) AS "parts!",
                  string_agg(content, '' ORDER BY part) AS "content!"
           FROM import_upload_files WHERE upload_id = $1
           GROUP BY path ORDER BY path"#,
        id,
    )
    .fetch_all(&mut **tx)
    .await?;
    if rows.is_empty() {
        return Err(invalid(
            "files",
            "No file arrived: send the migrations folder's files first.",
        ));
    }
    let mut files = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(missing) = missing_part(&row.parts) {
            return Err(ApiError::Unprocessable {
                code: "incomplete_upload",
                message: format!(
                    "{}: part {missing} is missing (parts {} arrived). Send it, then import again.",
                    row.path,
                    row.parts
                        .iter()
                        .map(i32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                details: Some(serde_json::json!({ "path": row.path, "part": missing })),
            });
        }
        files.push(SourceFile::new(row.path, row.content));
    }
    Ok(files)
}

/// The first part missing from `parts` (sorted), which must be 1, 2… n.
fn missing_part(parts: &[i32]) -> Option<i32> {
    (1..)
        .zip(parts)
        .find(|(k, part)| k != *part)
        .map(|(k, _)| k)
}

/// The upload as its starter sees it.
async fn load(db: &PgPool, database_id: Uuid, id: Uuid, user: Uuid) -> ApiResult<ImportUpload> {
    let row = sqlx::query!(
        r#"SELECT u.id, u.status AS "status: ImportUploadStatus", u.dry_run, u.progress,
                  u.outcome, u.created_at, u.expires_at, u.expires_at <= now() AS "expired!",
                  (SELECT count(DISTINCT path) FROM import_upload_files f
                   WHERE f.upload_id = u.id) AS "files!",
                  (SELECT coalesce(sum(octet_length(content)), 0) FROM import_upload_files f
                   WHERE f.upload_id = u.id)::bigint AS "bytes!"
           FROM import_uploads u
           WHERE u.id = $1 AND u.database_id = $2 AND u.created_by = $3"#,
        id,
        database_id,
        user,
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(no_such_upload)?;
    let (report, error) = match (row.status, row.outcome) {
        (ImportUploadStatus::Failed, Some(outcome)) => {
            (None, serde_json::from_value::<ErrorBody>(outcome).ok())
        }
        (_, outcome) => (outcome, None),
    };
    Ok(ImportUpload {
        id: row.id,
        status: row.status,
        files: row.files,
        bytes: row.bytes,
        dry_run: row.dry_run,
        created_at: row.created_at,
        expires_at: row.expires_at,
        expired: row.expired && row.status.takes_files(),
        progress: row
            .progress
            .filter(|_| row.status == ImportUploadStatus::Importing)
            .and_then(|p| serde_json::from_value(p).ok()),
        report,
        error,
    })
}

async fn save_progress(db: &PgPool, id: Uuid, progress: &ImportProgress) {
    let saved = sqlx::query!(
        "UPDATE import_uploads SET progress = $2, updated_at = now()
         WHERE id = $1 AND status = 'importing'",
        id,
        serde_json::to_value(progress).unwrap_or_default(),
    )
    .execute(db)
    .await;
    if let Err(err) = saved {
        tracing::warn!(error = %err, upload = %id, "an import's progress was not saved");
    }
}

/// The import refused or stopped: the error kept, the files too (fix one,
/// import again).
async fn failed(db: &PgPool, id: Uuid, error: &ErrorBody) {
    let saved = sqlx::query!(
        "UPDATE import_uploads
         SET status = 'failed', outcome = $2, progress = NULL, updated_at = now(),
             expires_at = now() + interval '1 hour'
         WHERE id = $1 AND status = 'importing'",
        id,
        serde_json::to_value(error).unwrap_or_default(),
    )
    .execute(db)
    .await;
    if let Err(err) = saved {
        tracing::warn!(error = %err, upload = %id, "an import's failure was not saved");
    }
}

/// The import ended: a dry run keeps the files (to import for real next),
/// an import deletes them and keeps its report for a day.
async fn done(db: &PgPool, id: Uuid, report: &ImportReport, dry_run: bool) -> sqlx::Result<()> {
    let report = serde_json::to_value(report).unwrap_or_default();
    let mut tx = db.begin().await?;
    if dry_run {
        sqlx::query!(
            "UPDATE import_uploads
             SET status = 'checked', outcome = $2, progress = NULL, updated_at = now(),
                 expires_at = now() + interval '1 hour'
             WHERE id = $1 AND status = 'importing'",
            id,
            report,
        )
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query!(
            "UPDATE import_uploads
             SET status = 'imported', outcome = $2, progress = NULL, updated_at = now(),
                 expires_at = now() + interval '1 day'
             WHERE id = $1 AND status = 'importing'",
            id,
            report,
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!("DELETE FROM import_upload_files WHERE upload_id = $1", id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

/// Follows an import to its end, into the upload: its progress as it goes,
/// then its report or its error.
async fn record(
    db: PgPool,
    id: Uuid,
    mut events: mpsc::UnboundedReceiver<ImportEvent>,
    mut progress: ImportProgress,
    dry_run: bool,
) {
    while let Some(event) = events.recv().await {
        match event {
            ImportEvent::Planned { .. } => continue,
            ImportEvent::Queued => progress.stage = ImportStage::Queued,
            ImportEvent::Checked => progress.stage = ImportStage::Replaying,
            ImportEvent::Replayed { .. } => progress.replayed += 1,
            ImportEvent::Done { report } => {
                if let Err(err) = done(&db, id, &report, dry_run).await {
                    tracing::warn!(error = %err, upload = %id, "an import's report was not saved");
                }
                return;
            }
            ImportEvent::Failed { error } => {
                failed(&db, id, &error).await;
                return;
            }
        }
        save_progress(&db, id, &progress).await;
    }
    // The job ended without a last event: it panicked.
    failed(&db, id, &lost("The import stopped unexpectedly")).await;
}

/// Why an import that will never report ended so.
fn lost(what: &str) -> ErrorBody {
    ErrorBody {
        code: "import_interrupted".into(),
        message: format!(
            "{what}. Its versions may or may not have been created: the database's history shows it; importing the same files again creates only what is missing."
        ),
        details: None,
    }
}

/// Imports lost with their process (no progress for 30 minutes) marked
/// failed; expired uploads' files deleted, and the uploads a day later.
/// Returns how many rows changed.
pub async fn sweep(db: &PgPool) -> Result<u64, sqlx::Error> {
    let lost = sqlx::query!(
        "UPDATE import_uploads
         SET status = 'failed', outcome = $1, progress = NULL,
             expires_at = now() + interval '1 hour'
         WHERE status = 'importing' AND updated_at < now() - make_interval(mins => $2)",
        serde_json::to_value(lost("romulus lost track of this import (it restarted)"))
            .unwrap_or_default(),
        LOST_AFTER_MINUTES,
    )
    .execute(db)
    .await?;
    let files = sqlx::query!(
        "DELETE FROM import_upload_files f USING import_uploads u
         WHERE f.upload_id = u.id AND u.expires_at < now() AND u.status <> 'importing'"
    )
    .execute(db)
    .await?;
    let uploads = sqlx::query!(
        "DELETE FROM import_uploads
         WHERE expires_at < now() - interval '1 day' AND status <> 'importing'"
    )
    .execute(db)
    .await?;
    Ok(lost.rows_affected() + files.rows_affected() + uploads.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_made_relative_or_refused() {
        assert_eq!(
            relative_path("./db\\migrate/V1__a.sql").unwrap(),
            "db/migrate/V1__a.sql"
        );
        assert_eq!(relative_path(" V1__a.sql ").unwrap(), "V1__a.sql");
        for bad in [
            "",
            "/home/ada/app/V1__a.sql",
            "C:\\app\\V1__a.sql",
            "~/app/V1__a.sql",
            "db/../V1__a.sql",
            "db//V1__a.sql",
            "db/./V1__a.sql",
            "a\nb.sql",
        ] {
            assert!(relative_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_path_and_part_sent_twice_keeps_the_last() {
        let file = |path: &str, part: Option<i32>, content: &str| UploadedFile {
            path: path.into(),
            content: content.into(),
            part,
        };
        let files = checked(vec![
            file("a.sql", None, "old"),
            file("./a.sql", Some(1), "new"),
            file("a.sql", Some(2), "tail"),
        ])
        .unwrap();
        assert_eq!(
            files,
            [
                Checked {
                    path: "a.sql".into(),
                    part: 1,
                    content: "new".into()
                },
                Checked {
                    path: "a.sql".into(),
                    part: 2,
                    content: "tail".into()
                },
            ]
        );
        assert!(checked(vec![file("a.sql", Some(0), "")]).is_err());
        assert!(checked(vec![file("a.sql", Some(101), "")]).is_err());
        assert!(checked(vec![file("a.sql", None, "a\0b")]).is_err());
    }

    #[test]
    fn parts_must_follow_each_other_from_one() {
        assert_eq!(missing_part(&[1]), None);
        assert_eq!(missing_part(&[1, 2, 3]), None);
        assert_eq!(missing_part(&[2]), Some(1));
        assert_eq!(missing_part(&[1, 3]), Some(2));
    }
}
