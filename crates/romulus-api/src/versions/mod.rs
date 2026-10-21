//! Immutable schema versions, numbered 1, 2, 3… per database.
//!
//! Browser endpoints (who may call them in brackets; see `auth::member`):
//! - `GET /orgs/{org}/databases/{db}/versions?before=&limit=` [member]:
//!   newest first, a page at a time (`next_before` continues).
//! - `POST /orgs/{org}/databases/{db}/versions` [developer]: upload SQL. It is
//!   compiled (`compile`) and stored with its source verbatim (`source_kind
//!   = 'sql'`, origin `upload`); a text that does not compile answers 422
//!   `compile_failed` with the diagnostics, and nothing is stored.
//! - `POST /orgs/{org}/databases/{db}/compile` [developer]: the same compile
//!   as a dry run, with what would change against the latest version.
//! - `GET …/versions/{number}` [member]: the model, source, layout and
//!   author; `GET …/versions/{number}/texts` [member]: the model as SQL
//!   (the source itself for SQL versions), JSON, DBML and Mermaid.
//! - `GET …/versions/{number}/migration` [member]: the migration it was
//!   written as, for a version published from a draft in migration mode
//!   (`versions.migration_sql`; 404 for others).
//!
//! Machine API, authenticated by an API token ([`push`]):
//! - `POST /databases/{database_id}/versions`: remus push, JSON or SQL; an
//!   unchanged model creates nothing (`200 {"status": "unchanged"}`).
//! - `GET /databases/{database_id}/versions/{version}` [`full` scope]: a
//!   version by number, or `latest`.
//!
//! Diffs ([`diff`]):
//! - `GET /orgs/{org}/databases/{db}/diff?from=&to=` [member]: two versions
//!   compared (`romulus-diff`), with both models, their SQL and the layout
//!   of their union; cached by version ids.
//!
//! Migrations ([`migration`]):
//! - `GET /orgs/{org}/databases/{db}/migration?from=&to=&format=&concurrently=&name=`
//!   [member]: the `ALTER …` statements from one version to another
//!   (`romulus-migration`) and the files of one migration tool.
//!
//! Discussion ([`discussion`]): `GET/POST …/versions/{number}/threads`
//! [member] and their comments, resolve, reopen and read marks: the review
//! of the draft it was published from (frozen) and threads started since.
//!
//! The time machine ([`timeline`]):
//! - `GET /orgs/{org}/databases/{db}/timeline?removed=` [member;
//!   `removed=true` admins]: every version, oldest first, and the history of
//!   every object across them (when each appeared, changed and went, with
//!   its value in each state), from romulus-diff between consecutive
//!   versions, cached per pair of versions.
//!
//! Removing ([`removal`]):
//! - `POST …/versions/{number}/remove` [admin], with a reason: hidden from
//!   the history, diffs, imports and the latest version; `POST
//!   …/versions/{number}/restore` [admin] within 30 days; `GET
//!   …/versions/removed` [developer]. Then the purge deletes it; its number
//!   is never given again.
//!
//! Table: `versions` (immutable by trigger, but for its removal). Numbers are
//! assigned under a lock on the `databases` row ([`store`]), after the
//! highest ever given (`databases.last_version_number`). Every read here is
//! of live versions (`removed_at IS NULL`).

pub mod diff;
pub mod discussion;
pub mod layout;
pub mod migration;
pub mod push;
pub mod removal;
pub mod store;
pub mod timeline;

use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use remus_core::Schema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use self::{
    layout::VersionLayout,
    store::{NewVersion, Stored, store_capped},
};
use crate::{
    ApiError, ApiResult, AppState,
    auth::{Member, min},
    compile::{self, Diagnostic, SqlCompiler},
    databases,
    domain::{MigrationFormat, PgMajor, SourceKind, VersionOrigin},
    error::ErrorBody,
    extract::{Json, Path, Query},
    orgs::invalid,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, upload))
        .routes(routes!(check))
        .routes(routes!(get))
        .routes(routes!(texts))
        .routes(routes!(written_migration))
        .merge(diff::router())
        .merge(discussion::router())
        .merge(migration::router())
        .merge(push::router())
        .merge(removal::router())
        .merge(timeline::router())
}

/// Who made a version: a person (uploads, drafts) or an API token (pushes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Author {
    User { id: Uuid, name: String },
    Token { id: Uuid, name: String },
}

impl Author {
    /// From a version's joined `users` and `api_tokens` columns.
    pub fn from_columns(
        user_id: Option<Uuid>,
        user_name: Option<String>,
        token_id: Option<Uuid>,
        token_name: Option<String>,
    ) -> Option<Self> {
        match (user_id, user_name, token_id, token_name) {
            (Some(id), Some(name), _, _) => Some(Self::User { id, name }),
            (_, _, Some(id), Some(name)) => Some(Self::Token { id, name }),
            _ => None,
        }
    }
}

/// A version in a list.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VersionSummary {
    pub id: Uuid,
    /// 1, 2, 3… per database, never given twice (a removed version leaves
    /// its number unused).
    pub number: i32,
    pub origin: VersionOrigin,
    pub source_kind: SourceKind,
    /// What changed, in a line; `null` when none was given.
    pub message: Option<String>,
    pub created_at: DateTime<Utc>,
    /// `null` once the person's account or the token is gone.
    pub author: Option<Author>,
    /// Published from a draft in migration mode: the migration it was
    /// written as is at `…/versions/{number}/migration`.
    pub written_migration: bool,
    pub postgres: VersionPostgres,
}

/// The Postgres a version was compiled on (uploads, drafts, imports: the
/// sandbox of its database's major) or pushed from (remus JSON: the real
/// server). Versions keep it when their database moves to another major.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct VersionPostgres {
    /// As remus read it: `17.11 (Debian 17.11-1.pgdg13+1)`.
    pub server_version: String,
    /// `server_version`'s major; `null` when it does not start with one.
    pub major: Option<i32>,
    /// The major its database targeted when it was created.
    pub database_major: i32,
    /// Pushed from a server on another major than its database targeted
    /// then: the history flags it, and offers to move the database.
    pub other_major: bool,
}

impl VersionPostgres {
    /// From a version's `server_version`, `pg_major` and `database_major`.
    pub fn new(
        origin: VersionOrigin,
        server_version: Option<String>,
        major: Option<i16>,
        database_major: i16,
    ) -> Self {
        Self {
            server_version: server_version.unwrap_or_default(),
            major: major.map(i32::from),
            database_major: i32::from(database_major),
            other_major: origin == VersionOrigin::Push
                && major.is_some_and(|m| m != database_major),
        }
    }
}

/// One version, whole.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VersionDetail {
    pub id: Uuid,
    pub number: i32,
    pub origin: VersionOrigin,
    pub source_kind: SourceKind,
    pub message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub author: Option<Author>,
    /// The remus model (`remus_core::Schema` as JSON), canonical.
    #[schema(value_type = Object)]
    pub model: serde_json::Value,
    /// What was uploaded or pushed, verbatim: SQL, remus JSON, or the
    /// migration's up SQL for imports.
    pub source: String,
    /// Table positions and groups, keyed by qualified name (empty until a
    /// draft arranges them).
    pub layout: VersionLayout,
    /// The migration file it was imported from (origin `import` only).
    pub migration: Option<VersionMigration>,
    /// Who approved the draft it was published from, when the organization
    /// required approvals (`drafts::approvals::freeze`); oldest first.
    pub approvals: Vec<VersionApproval>,
    /// Published from a draft in migration mode: the migration it was
    /// written as is at `…/versions/{number}/migration`.
    pub written_migration: bool,
    pub postgres: VersionPostgres,
}

/// An approval of the draft a version was published from, as it was then.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VersionApproval {
    /// `null` once their account is gone.
    pub user_id: Option<Uuid>,
    pub name: String,
    pub at: DateTime<Utc>,
}

/// The migration a version was written as, in a draft in migration mode:
/// from the version before it (nothing, for a first version) to it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WrittenMigration {
    pub number: i32,
    /// The version it migrates from; `null` for a first version.
    pub from: Option<i32>,
    /// The migration, as written (data statements included).
    pub sql: String,
    /// What it downloads as: `v5_add_invoices.sql`.
    pub file_name: String,
}

/// The migration an imported version came from.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VersionMigration {
    /// The tool's version of it, or its file name for plain folders.
    pub id: String,
    /// The file, relative to the folder imported.
    pub path: String,
    pub format: MigrationFormat,
    /// What was not replayed (`SkippedStatement[]`, lines in the version's
    /// `source`: the migration's own text).
    #[schema(value_type = Vec<crate::imports::SkippedStatement>)]
    pub skipped: serde_json::Value,
}

/// A version's model in every format the viewer shows.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VersionTexts {
    /// The source for SQL versions; rendered from the model for pushed JSON
    /// and imported migrations.
    pub sql: String,
    /// Pretty-printed remus JSON.
    pub json: String,
    pub dbml: String,
    pub mermaid: String,
}

/// A page of versions, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VersionPage {
    pub versions: Vec<VersionSummary>,
    /// Pass as `before` for the next page; `null` on the last one.
    pub next_before: Option<i32>,
    /// Older versions kept but not listed: past the plan's history window
    /// (hosted billing). They come back with a plan that keeps them; an
    /// export holds them all.
    pub hidden: i64,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListVersions {
    /// Versions numbered below this (from `next_before`).
    pub before: Option<i32>,
    /// 1 to 100; 50 by default.
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UploadSql {
    /// The schema as SQL DDL, at most 1 MiB.
    pub sql: String,
    /// What changed, in a line (at most 1000 characters).
    pub message: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CompileSql {
    pub sql: String,
}

/// A dry run: whether the SQL compiles, and what it would change.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CompileReport {
    /// Errors (when it does not compile), then notes, located in the text.
    pub diagnostics: Vec<Diagnostic>,
    /// When it compiles: the difference with the latest version.
    pub changes: Option<Changes>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Changes {
    /// The version it was compared with; `null` when there is none yet (the
    /// summary then counts what the SQL creates).
    pub against: Option<i32>,
    /// `"3 tables added, 1 changed; 1 enum changed"`, or `"no changes"`.
    pub summary: String,
    /// The model is the latest version's: uploading it would add a version
    /// identical to it.
    pub unchanged: bool,
}

#[derive(sqlx::FromRow)]
struct SummaryRow {
    id: Uuid,
    number: i32,
    origin: VersionOrigin,
    source_kind: SourceKind,
    message: Option<String>,
    created_at: DateTime<Utc>,
    user_id: Option<Uuid>,
    user_name: Option<String>,
    token_id: Option<Uuid>,
    token_name: Option<String>,
    written_migration: bool,
    server_version: Option<String>,
    pg_major: Option<i16>,
    database_major: i16,
}

impl From<SummaryRow> for VersionSummary {
    fn from(r: SummaryRow) -> Self {
        Self {
            id: r.id,
            number: r.number,
            origin: r.origin,
            source_kind: r.source_kind,
            message: r.message,
            created_at: r.created_at,
            author: Author::from_columns(r.user_id, r.user_name, r.token_id, r.token_name),
            written_migration: r.written_migration,
            postgres: VersionPostgres::new(
                r.origin,
                r.server_version,
                r.pg_major,
                r.database_major,
            ),
        }
    }
}

/// Up to `limit` versions numbered below `before` (any, when `None`), newest
/// first, as summaries: the history's pages, and a diff's two versions.
/// With a `cutoff` (the plan's history window), older versions are left
/// out but the latest and those open drafts are based on.
async fn page(
    db: &PgPool,
    database_id: Uuid,
    before: Option<i32>,
    limit: i64,
    cutoff: Option<DateTime<Utc>>,
) -> Result<Vec<SummaryRow>, sqlx::Error> {
    sqlx::query_as!(
        SummaryRow,
        r#"SELECT v.id, v.number, v.origin AS "origin: VersionOrigin",
                  v.source_kind AS "source_kind: SourceKind", v.message, v.created_at,
                  u.id AS "user_id?", u.name AS "user_name?", t.id AS "token_id?", t.name AS "token_name?",
                  v.migration_sql IS NOT NULL AS "written_migration!",
                  v.server_version, v.pg_major, v.database_major
           FROM versions v
           LEFT JOIN users u ON u.id = v.created_by
           LEFT JOIN api_tokens t ON t.id = v.created_by_token
           WHERE v.database_id = $1 AND v.removed_at IS NULL AND ($2::int IS NULL OR v.number < $2)
             AND ($4::timestamptz IS NULL OR v.created_at >= $4 OR version_kept_visible(v.id, v.database_id))
           ORDER BY v.number DESC LIMIT $3"#,
        database_id,
        before,
        limit,
        cutoff,
    )
    .fetch_all(db)
    .await
}

/// One version's summary, by id.
async fn summary(db: &PgPool, id: Uuid) -> ApiResult<VersionSummary> {
    let row = sqlx::query_as!(
        SummaryRow,
        r#"SELECT v.id, v.number, v.origin AS "origin: VersionOrigin",
                  v.source_kind AS "source_kind: SourceKind", v.message, v.created_at,
                  u.id AS "user_id?", u.name AS "user_name?", t.id AS "token_id?", t.name AS "token_name?",
                  v.migration_sql IS NOT NULL AS "written_migration!",
                  v.server_version, v.pg_major, v.database_major
           FROM versions v
           LEFT JOIN users u ON u.id = v.created_by
           LEFT JOIN api_tokens t ON t.id = v.created_by_token
           WHERE v.id = $1"#,
        id,
    )
    .fetch_one(db)
    .await?;
    Ok(row.into())
}

/// A live version by number (`None`: the latest), whole.
pub(crate) async fn detail(
    db: &PgPool,
    database_id: Uuid,
    number: Option<i32>,
) -> ApiResult<VersionDetail> {
    let row = sqlx::query!(
        r#"SELECT v.id, v.number, v.origin AS "origin: VersionOrigin",
                  v.source_kind AS "source_kind: SourceKind", v.message, v.created_at,
                  v.model, v.source, v.layout,
                  u.id AS "user_id?", u.name AS "user_name?", t.id AS "token_id?", t.name AS "token_name?",
                  m.migration_id AS "migration_id?", m.path AS "migration_path?",
                  m.format AS "migration_format?: MigrationFormat", m.skipped AS "migration_skipped?",
                  v.migration_sql IS NOT NULL AS "written_migration!",
                  v.server_version, v.pg_major, v.database_major
           FROM versions v
           LEFT JOIN users u ON u.id = v.created_by
           LEFT JOIN api_tokens t ON t.id = v.created_by_token
           LEFT JOIN version_migrations m ON m.version_id = v.id
           WHERE v.database_id = $1 AND v.removed_at IS NULL AND ($2::int IS NULL OR v.number = $2)
           ORDER BY v.number DESC LIMIT 1"#,
        database_id,
        number,
    )
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Err(missing(db, database_id, number).await);
    };
    let approvals = sqlx::query_as!(
        VersionApproval,
        "SELECT user_id, name, approved_at AS at FROM version_approvals
         WHERE version_id = $1 ORDER BY approved_at",
        row.id,
    )
    .fetch_all(db)
    .await?;
    Ok(VersionDetail {
        id: row.id,
        number: row.number,
        origin: row.origin,
        source_kind: row.source_kind,
        message: row.message,
        created_at: row.created_at,
        author: Author::from_columns(row.user_id, row.user_name, row.token_id, row.token_name),
        model: row.model,
        source: row.source,
        layout: VersionLayout::from_value(&row.layout),
        migration: match (row.migration_id, row.migration_path, row.migration_format) {
            (Some(id), Some(path), Some(format)) => Some(VersionMigration {
                id,
                path,
                format,
                skipped: row.migration_skipped.unwrap_or_default(),
            }),
            _ => None,
        },
        approvals,
        written_migration: row.written_migration,
        postgres: VersionPostgres::new(
            row.origin,
            row.server_version,
            row.pg_major,
            row.database_major,
        ),
    })
}

/// The 404 for a version that is not there: removed, or never was.
pub(crate) async fn missing(db: &PgPool, database_id: Uuid, number: Option<i32>) -> ApiError {
    let Some(n) = number else {
        return ApiError::not_found("This database has no version yet.");
    };
    let removed = sqlx::query_scalar!(
        r#"SELECT removed_at IS NOT NULL AS "removed!" FROM versions WHERE database_id = $1 AND number = $2"#,
        database_id,
        n,
    )
    .fetch_optional(db)
    .await;
    match removed {
        Ok(Some(true)) => ApiError::not_found(format!("v{n} was removed.")),
        Ok(Some(false) | None) => {
            ApiError::not_found(format!("There is no version {n} of this database."))
        }
        Err(err) => err.into(),
    }
}

/// The number the database's next version gets: after the highest ever
/// given, removed and purged ones included.
pub(crate) async fn next_number(db: &PgPool, database_id: Uuid) -> ApiResult<i32> {
    Ok(sqlx::query_scalar!(
        "SELECT last_version_number + 1 AS \"next!\" FROM databases WHERE id = $1",
        database_id
    )
    .fetch_optional(db)
    .await?
    .unwrap_or(1))
}

/// A message as stored: trimmed, `None` when empty, at most 1000 characters.
fn checked_message(message: Option<&str>) -> ApiResult<Option<String>> {
    let message = message.map(str::trim).filter(|m| !m.is_empty());
    if message.is_some_and(|m| m.chars().count() > 1000) {
        return Err(invalid(
            "message",
            "Keep the message to 1000 characters or fewer.",
        ));
    }
    Ok(message.map(str::to_owned))
}

/// Compiles `sql` on the database's major for the database `slug`: its
/// model (named after the database), or 422 `compile_failed` with the
/// diagnostics.
async fn compiled_model(
    compiler: &SqlCompiler,
    major: PgMajor,
    sql: &str,
    slug: &str,
) -> ApiResult<Schema> {
    let outcome = compiler.compile(major, sql).await?;
    match outcome.model {
        Some(mut model) => {
            slug.clone_into(&mut model.database);
            Ok(model)
        }
        None => Err(compile_failed(&outcome.diagnostics)),
    }
}

fn compile_failed(diagnostics: &[Diagnostic]) -> ApiError {
    let errors = diagnostics.iter().filter(|d| d.is_error()).count();
    let message = match errors {
        0 | 1 => "The SQL does not compile. Fix the error and upload it again.".to_owned(),
        n => format!("The SQL does not compile: {n} errors. Fix them and upload it again."),
    };
    ApiError::Unprocessable {
        code: "compile_failed",
        message,
        details: Some(json!({ "diagnostics": diagnostics })),
    }
}

/// The database's versions, newest first.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/versions",
    operation_id = "list_versions",
    tag = "versions",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        ListVersions,
    ),
    responses(
        (status = 200, body = VersionPage),
        (status = 404, body = ErrorBody),
    )
)]
async fn list(
    member: Member,
    State(db): State<PgPool>,
    State(billing): State<crate::billing::Billing>,
    Path((_, slug)): Path<(String, String)>,
    Query(query): Query<ListVersions>,
) -> ApiResult<Json<VersionPage>> {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let database = databases::find(&db, member.org.id, &slug).await?;
    let cutoff = billing.history(&db, member.org.id).await?.cutoff;
    let mut rows = page(&db, database.id, query.before, limit + 1, cutoff).await?;
    let more = rows.len() > usize::try_from(limit).unwrap_or(usize::MAX);
    rows.truncate(rows.len().min(usize::try_from(limit).unwrap_or(usize::MAX)));
    let next_before = more.then(|| rows.last().map(|r| r.number)).flatten();
    let hidden = match cutoff {
        None => 0,
        Some(cutoff) => hidden_versions(&db, database.id, cutoff).await?,
    };
    Ok(Json(VersionPage {
        versions: rows.into_iter().map(Into::into).collect(),
        next_before,
        hidden,
    }))
}

/// Live versions of a database past the history window `cutoff`.
pub(crate) async fn hidden_versions(
    db: &PgPool,
    database_id: Uuid,
    cutoff: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM versions v
           WHERE v.database_id = $1 AND v.removed_at IS NULL AND v.created_at < $2
             AND NOT version_kept_visible(v.id, v.database_id)"#,
        database_id,
        cutoff,
    )
    .fetch_one(db)
    .await
}

/// Uploads SQL as the next version. Developers and admins.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/versions",
    operation_id = "upload_version",
    tag = "versions",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
    ),
    request_body = UploadSql,
    responses(
        (status = 201, body = VersionSummary),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 422, description = "`compile_failed` (`details.diagnostics`: `Diagnostic[]`), or `validation`", body = ErrorBody),
        (status = 429, description = "`rate_limited`", body = ErrorBody),
        (status = 503, description = "`unavailable`: the compiler is down; nothing was stored", body = ErrorBody),
    )
)]
async fn upload(
    member: Member<min::Developer>,
    State(state): State<AppState>,
    Path((_, slug)): Path<(String, String)>,
    Json(body): Json<UploadSql>,
) -> ApiResult<(StatusCode, Json<VersionSummary>)> {
    let message = checked_message(body.message.as_deref())?;
    if body.sql.trim().is_empty() {
        return Err(invalid(
            "sql",
            "Paste the SQL of the schema, or drop a .sql file.",
        ));
    }
    let database = databases::find(&state.db, member.org.id, &slug).await?;
    state.limits.compile.check(member.user.id)?;
    state.limits.database_compiles.check(database.id)?;
    let model = compiled_model(
        &state.compiler,
        database.pg_major,
        &body.sql,
        &database.slug,
    )
    .await?;
    let new = NewVersion {
        database_id: database.id,
        model,
        source: body.sql,
        source_kind: SourceKind::Sql,
        origin: VersionOrigin::Upload,
        message,
        created_by: Some(member.user.id),
        created_by_token: None,
        layout: None,
        expected_base: None,
        publishes_draft: None,
        approvals: None,
        test_override: None,
        migration_sql: None,
        compiled_on: Some(database.pg_major),
        discussion: Vec::new(),
        renames: None,
    };
    let limits = state.billing.version_limits(&state.db, database.id).await?;
    // Uploads always create a version (`skip_unchanged` is false): a person
    // asked for it, and the source may differ even when the model does not.
    let Stored::Created(version) = store_capped(&state.db, new, false, limits).await? else {
        unreachable!("store skips an unchanged model only when asked to");
    };
    // Open drafts of the database learn their base moved.
    state.collab.version_created(database.id, version.number);
    Ok((StatusCode::CREATED, Json(version)))
}

/// Compiles SQL without storing anything: its errors, or what it would
/// change against the latest version. Developers and admins.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/compile",
    operation_id = "compile_sql",
    tag = "versions",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
    ),
    request_body = CompileSql,
    responses(
        (status = 200, body = CompileReport),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 429, description = "`rate_limited`", body = ErrorBody),
        (status = 503, description = "`unavailable`: the compiler is down", body = ErrorBody),
    )
)]
async fn check(
    member: Member<min::Developer>,
    State(state): State<AppState>,
    Path((_, slug)): Path<(String, String)>,
    Json(body): Json<CompileSql>,
) -> ApiResult<Json<CompileReport>> {
    let database = databases::find(&state.db, member.org.id, &slug).await?;
    state.limits.compile.check(member.user.id)?;
    state.limits.database_compiles.check(database.id)?;
    let compiled = state.compiler.compile(database.pg_major, &body.sql).await?;
    let Some(model) = compiled.model else {
        return Ok(Json(CompileReport {
            diagnostics: compiled.diagnostics,
            changes: None,
        }));
    };
    let latest = sqlx::query!(
        "SELECT number, model FROM versions WHERE database_id = $1 AND removed_at IS NULL
         ORDER BY number DESC LIMIT 1",
        database.id,
    )
    .fetch_optional(&state.db)
    .await?;
    // Parsing the latest model and diffing: off the async workers.
    let changes = tokio::task::spawn_blocking(move || {
        let (against, previous) = match latest {
            Some(latest) => (
                Some(latest.number),
                Schema::deserialize(&latest.model).unwrap_or_else(|_| empty(&model)),
            ),
            None => (None, empty(&model)),
        };
        let diff = romulus_diff::diff(&previous, &model);
        Changes {
            against,
            unchanged: against.is_some() && diff.is_empty(),
            summary: diff.summary.text,
        }
    })
    .await
    .map_err(ApiError::internal)?;
    Ok(Json(CompileReport {
        diagnostics: compiled.diagnostics,
        changes: Some(changes),
    }))
}

/// A model with nothing in it: what a first version is compared with.
pub(crate) fn empty(like: &Schema) -> Schema {
    let mut empty = like.clone();
    empty.entities.clear();
    empty.enums.clear();
    empty.domains.clear();
    empty.composites.clear();
    empty.extensions.clear();
    empty.partitions.clear();
    empty
}

/// One version: its model, source, layout and author.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/versions/{number}",
    operation_id = "get_version",
    tag = "versions",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        ("number" = i32, Path, description = "The version's number"),
    ),
    responses(
        (status = 200, body = VersionDetail),
        (status = 404, body = ErrorBody),
    )
)]
async fn get(
    member: Member,
    State(db): State<PgPool>,
    State(billing): State<crate::billing::Billing>,
    Path((_, slug, number)): Path<(String, String, i32)>,
) -> ApiResult<Json<VersionDetail>> {
    let database = databases::find(&db, member.org.id, &slug).await?;
    let version = detail(&db, database.id, Some(number)).await?;
    billing
        .shown(
            &db,
            member.org.id,
            database.id,
            (version.id, version.number, version.created_at),
        )
        .await?;
    Ok(Json(version))
}

/// One version's model as SQL, JSON, DBML and Mermaid.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/versions/{number}/texts",
    operation_id = "get_version_texts",
    tag = "versions",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        ("number" = i32, Path, description = "The version's number"),
    ),
    responses(
        (status = 200, body = VersionTexts),
        (status = 404, body = ErrorBody),
    )
)]
async fn texts(
    member: Member,
    State(db): State<PgPool>,
    State(billing): State<crate::billing::Billing>,
    Path((_, slug, number)): Path<(String, String, i32)>,
) -> ApiResult<Json<VersionTexts>> {
    let database = databases::find(&db, member.org.id, &slug).await?;
    let row = sqlx::query!(
        r#"SELECT id, created_at, model, source, source_kind AS "source_kind: SourceKind"
           FROM versions WHERE database_id = $1 AND number = $2 AND removed_at IS NULL"#,
        database.id,
        number,
    )
    .fetch_optional(&db)
    .await?;
    let Some(row) = row else {
        return Err(missing(&db, database.id, Some(number)).await);
    };
    billing
        .shown(
            &db,
            member.org.id,
            database.id,
            (row.id, number, row.created_at),
        )
        .await?;
    let model = Schema::deserialize(&row.model).map_err(ApiError::internal)?;
    // Rendering a few hundred tables takes milliseconds; off the async
    // workers all the same.
    let texts = tokio::task::spawn_blocking(move || VersionTexts {
        sql: match row.source_kind {
            SourceKind::Sql => row.source,
            SourceKind::RemusJson | SourceKind::Migration => compile::render_sql(&model),
        },
        json: compile::render::json(&model),
        dbml: compile::render::dbml(&model),
        mermaid: compile::render::mermaid(&model),
    })
    .await
    .map_err(ApiError::internal)?;
    Ok(Json(texts))
}

/// The migration a version was written as (a draft in migration mode
/// published it), to read again or download.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/versions/{number}/migration",
    operation_id = "get_written_migration",
    tag = "versions",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        ("number" = i32, Path, description = "The version's number"),
    ),
    responses(
        (status = 200, body = WrittenMigration),
        (status = 404, description = "No such version, or it was not written as a migration", body = ErrorBody),
    )
)]
async fn written_migration(
    member: Member,
    State(db): State<PgPool>,
    State(billing): State<crate::billing::Billing>,
    Path((_, slug, number)): Path<(String, String, i32)>,
) -> ApiResult<Json<WrittenMigration>> {
    let database = databases::find(&db, member.org.id, &slug).await?;
    let row = sqlx::query!(
        "SELECT id, created_at, migration_sql, message FROM versions WHERE database_id = $1 AND number = $2",
        database.id,
        number,
    )
    .fetch_optional(&db)
    .await?
    .ok_or_else(|| {
        ApiError::not_found(format!("There is no version {number} of this database."))
    })?;
    billing
        .shown(
            &db,
            member.org.id,
            database.id,
            (row.id, number, row.created_at),
        )
        .await?;
    let sql = row.migration_sql.ok_or_else(|| {
        ApiError::not_found(format!(
            "v{number} was not written as a migration: compare it with v{} for the migration romulus generates.",
            number - 1
        ))
    })?;
    let name = row
        .message
        .as_deref()
        .map_or_else(|| "migration".to_owned(), romulus_migration::slug);
    Ok(Json(WrittenMigration {
        number,
        from: (number > 1).then_some(number - 1),
        sql,
        file_name: format!("v{number}_{name}.sql"),
    }))
}
