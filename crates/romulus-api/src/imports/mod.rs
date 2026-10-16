//! Importing migration files as a database's version history
//! (docs/MVP.md, "Next: migration files").
//!
//! A folder of migrations becomes one version per migration (origin
//! `import`, its message the file name, its source the migration's up SQL),
//! replayed from an empty database in the sandbox
//! (`romulus_sandbox::Compiler::replay`). The format is detected from the
//! files unless given; data, code and session statements are skipped and
//! reported per file. All or nothing: a migration that fails stops the
//! import, and nothing is stored.
//!
//! Endpoints (who may call them in brackets):
//! - `POST /orgs/{org}/databases/{db}/imports/preview` [developer]: the
//!   files read and planned, nothing run: the format, the ordered
//!   migrations (new, already imported, changed), what each one skips.
//! - `POST /orgs/{org}/databases/{db}/imports` [developer]: the import. With
//!   `Accept: application/x-ndjson` the answer streams its progress, one
//!   [`ImportEvent`] per line; otherwise it is the [`ImportReport`].
//! - `POST /databases/{database_id}/imports` [API token, `push` scope]: the
//!   same for CI. CI should ask for the progress (NDJSON): its headers come
//!   at once and a line (or a blank keep-alive) at least every 15 s, where
//!   the plain answer comes only at the end, after a proxy may have given
//!   up (Cloudflare: 524 after about 100 s; the import still lands). The
//!   last line is `done` or `failed`; the page's CI command and
//!   `scripts/export-migrations/README.md` turn it into an exit status. A
//!   retry while it runs answers 409 `concurrent_import` ("still being
//!   imported").
//!
//! Bodies: `multipart/form-data` (files named by their path in the folder,
//! or a zip), `application/zip`, or one SQL text (`application/sql`, what an
//! exporter in `scripts/export-migrations/` printed). Query: `format`,
//! `append`, `dry_run`, `filename` ([`ImportOptions`]).
//!
//! Imports are idempotent: a migration is matched by its id (the tool's
//! version, or the file name for plain folders) with the ones the database
//! already imported (`version_migrations`); importing the same files again
//! creates nothing, new migrations after them are appended, a changed one
//! (another checksum) or a new one sorted before an imported one is
//! refused. A database with versions that were not imported refuses an
//! import unless `append=true`.
//!
//! One import replays at a time; organizations take turns ([`Turns`]). A
//! queued import keeps its place when its client leaves (the job runs on
//! its own, as once it replays).
//!
//! What a push-scope token learns (kept to about what a push answers):
//! what it sent, read back (the plan, what is skipped, Postgres's errors
//! replaying it on top of the imported history, which may name the
//! history's objects), the version numbers its migrations get or got,
//! whether the database has versions that were not imported
//! (`has_versions`), and each new version's change counts against the one
//! before. Never the history's SQL or models, nor other versions'
//! messages.
//!
//! Tables: `versions`, `version_migrations` (one row per imported version).

mod plan;
mod run;
pub mod staging;
mod turns;
pub mod upload;

use axum::{
    extract::State,
    http::{HeaderMap, header},
    response::Response,
};
use romulus_sandbox::{
    Code,
    import::{IgnoreReason, Ignored, SkipReason},
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

pub use self::{
    plan::{ImportPreview, MigrationStatus, PlannedMigration},
    run::{ImportEvent, ImportReport, ImportStatus, ImportedMigration},
    turns::Turns,
};
use crate::{
    ApiResult, AppState,
    auth::{Member, min},
    databases::{self, tokens::Bearer},
    domain::MigrationFormat,
    error::ErrorBody,
    extract::{Json, Path, Query},
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(preview))
        .routes(routes!(import))
        .routes(routes!(import_with_token))
}

/// How to read and import the files.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ImportOptions {
    /// Read the files as this tool's; detected from them when absent.
    pub format: Option<MigrationFormat>,
    /// Import even though the database has versions that were not
    /// imported: the imported ones continue the numbering.
    #[serde(default)]
    pub append: bool,
    /// Replay without storing anything (a CI check).
    #[serde(default)]
    pub dry_run: bool,
    /// The name of a single SQL text sent as the body (`alembic.sql`).
    pub filename: Option<String>,
}

/// A statement not replayed, located in its file.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SkippedStatement {
    /// 1-based, in the file (`path`).
    pub line: u32,
    /// `INSERT`, `DO`, `GRANT`, `\connect`…
    pub kind: String,
    pub reason: SkipReason,
    /// Its first line.
    pub text: String,
}

/// An error, located in its file.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportError {
    pub code: Code,
    /// Postgres's message, or the allowlist's.
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub sqlstate: Option<String>,
    /// Where in the file (`path`), 1-based; `null` when unlocated.
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub end_line: Option<u32>,
    pub end_column: Option<u32>,
}

/// A file left out, and why.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct IgnoredFile {
    pub path: String,
    pub reason: IgnoreReason,
}

impl From<Ignored> for IgnoredFile {
    fn from(ignored: Ignored) -> Self {
        Self {
            path: ignored.path,
            reason: ignored.reason,
        }
    }
}

/// Where a failed import stopped (`details` of 422 `import_failed`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportFailure {
    /// Its position among the planned migrations.
    pub index: usize,
    pub id: String,
    pub name: String,
    pub path: String,
    pub errors: Vec<ImportError>,
}

/// Reads and plans the files, runs nothing: what an import of them would
/// do. Developers and admins.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/imports/preview",
    operation_id = "preview_import",
    tag = "imports",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        ImportOptions,
    ),
    request_body(
        description = "The migration files: a multipart form of files (named by their path), a zip, or one SQL text",
        content(
            (String = "multipart/form-data"),
            (String = "application/zip"),
            (String = "application/sql"),
        ),
    ),
    responses(
        (status = 200, body = ImportPreview),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 413, description = "`payload_too_large`", body = ErrorBody),
        (status = 415, description = "`unsupported_media_type`", body = ErrorBody),
        (status = 422, description = "`invalid_migrations` (no migration found, two with one version, a malformed file), `invalid_file`, `validation`", body = ErrorBody),
        (status = 429, description = "`rate_limited`", body = ErrorBody),
    )
)]
async fn preview(
    member: Member<min::Developer>,
    State(state): State<AppState>,
    Path((_, slug)): Path<(String, String)>,
    Query(options): Query<ImportOptions>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> ApiResult<Json<ImportPreview>> {
    let database = databases::find(&state.db, member.org.id, &slug).await?;
    state.limits.compile.check(member.user.id)?;
    state.limits.import_previews.check(database.id)?;
    let files = upload::files(&headers, body, options.filename.as_deref()).await?;
    let planned = plan::plan(&state.db, database.id, files, &options).await?;
    Ok(Json(planned.preview))
}

/// Imports migration files as versions. Developers and admins. See the
/// module documentation.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/imports",
    operation_id = "import_migrations",
    tag = "imports",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        ImportOptions,
    ),
    request_body(
        description = "The migration files: a multipart form of files (named by their path), a zip, or one SQL text",
        content(
            (String = "multipart/form-data"),
            (String = "application/zip"),
            (String = "application/sql"),
        ),
    ),
    responses(
        (status = 201, description = "Imported: the versions were created", body = ImportReport),
        (status = 200, description = "Nothing new (`unchanged`), or a dry run that replayed (`checked`); with `Accept: application/x-ndjson` (recommended for CI: no proxy times out), the progress, one `ImportEvent` per line, blank lines keeping the connection alive, the last one `done` or `failed`", content(
            (ImportReport = "application/json"),
            (ImportEvent = "application/x-ndjson"),
        )),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`has_versions` (pass `append=true`), `migration_changed`, `migration_out_of_order`, `concurrent_import` (another import of the database runs, or this one still does)", body = ErrorBody),
        (status = 413, description = "`payload_too_large`", body = ErrorBody),
        (status = 415, description = "`unsupported_media_type`", body = ErrorBody),
        (status = 422, description = "`import_failed` (`details`: `ImportFailure`), `invalid_migrations`, `invalid_file`, `too_many_migrations`, `models_too_large`, `validation`", body = ErrorBody),
        (status = 429, description = "`rate_limited`", body = ErrorBody),
        (status = 503, description = "`unavailable`: the sandbox is down or busy; nothing stored", body = ErrorBody),
    )
)]
async fn import(
    member: Member<min::Developer>,
    State(state): State<AppState>,
    Path((_, slug)): Path<(String, String)>,
    Query(options): Query<ImportOptions>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> ApiResult<Response> {
    let database = databases::find(&state.db, member.org.id, &slug).await?;
    state.limits.compile.check(member.user.id)?;
    state.limits.imports.check(database.id)?;
    let files = upload::files(&headers, body, options.filename.as_deref()).await?;
    let author = run::Author::User(member.user.id);
    let target = run::Target {
        org_id: member.org.id,
        database_id: database.id,
        slug: database.slug,
    };
    run::start(state, target, author, files, options, streams(&headers)).await
}

/// Imports migration files as versions, from CI: the API token's database.
/// See the module documentation.
#[utoipa::path(
    post,
    path = "/databases/{database_id}/imports",
    operation_id = "push_migrations",
    tag = "push",
    params(
        ("database_id" = uuid::Uuid, Path, description = "The database's id (its settings show it)"),
        ImportOptions,
    ),
    request_body(
        description = "The migration files: a multipart form of files (named by their path), a zip, or one SQL text",
        content(
            (String = "multipart/form-data"),
            (String = "application/zip"),
            (String = "application/sql"),
        ),
    ),
    responses(
        (status = 201, description = "Imported: the versions were created", body = ImportReport),
        (status = 200, description = "Nothing new (`unchanged`), or a dry run that replayed (`checked`); with `Accept: application/x-ndjson` (recommended for CI: no proxy times out), the progress, one `ImportEvent` per line, blank lines keeping the connection alive, the last one `done` or `failed`", content(
            (ImportReport = "application/json"),
            (ImportEvent = "application/x-ndjson"),
        )),
        (status = 401, description = "`invalid_token`", body = ErrorBody),
        (status = 403, description = "`wrong_database`", body = ErrorBody),
        (status = 409, description = "`has_versions` (pass `append=true`), `migration_changed`, `migration_out_of_order`, `concurrent_import` (another import of the database runs, or this one still does)", body = ErrorBody),
        (status = 413, description = "`payload_too_large`", body = ErrorBody),
        (status = 415, description = "`unsupported_media_type`", body = ErrorBody),
        (status = 422, description = "`import_failed` (`details`: `ImportFailure`), `invalid_migrations`, `invalid_file`, `too_many_migrations`, `models_too_large`, `validation`", body = ErrorBody),
        (status = 429, description = "`rate_limited`, with `Retry-After`", body = ErrorBody),
        (status = 503, description = "`unavailable`: the sandbox is down or busy; nothing stored", body = ErrorBody),
    )
)]
async fn import_with_token(
    token: Bearer,
    State(state): State<AppState>,
    Query(options): Query<ImportOptions>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> ApiResult<Response> {
    // Before reading up to 32 MiB of it.
    state.limits.push.check(token.token_id)?;
    state.limits.imports.check(token.database_id)?;
    let files = upload::files(&headers, body, options.filename.as_deref()).await?;
    let org_id = sqlx::query_scalar!(
        "SELECT org_id FROM databases WHERE id = $1",
        token.database_id
    )
    .fetch_one(&state.db)
    .await?;
    let author = run::Author::Token(token.token_id);
    let target = run::Target {
        org_id,
        database_id: token.database_id,
        slug: token.database_slug,
    };
    run::start(state, target, author, files, options, streams(&headers)).await
}

/// The client asked for the progress as it happens.
fn streams(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| value.contains("application/x-ndjson"))
}
