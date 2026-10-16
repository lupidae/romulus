//! Drafts: branches off a database's version line, edited together live
//! (the content is the Y.Doc `collab` serves).
//!
//! Endpoints (who may call them in brackets; see `auth::member`):
//! - `GET /orgs/{org}/databases/{db}/drafts` [member]: the open drafts, most
//!   recently edited first, with who has each open now.
//! - `POST /orgs/{org}/databases/{db}/drafts` [developer]: a new draft from
//!   the latest version, in one click: named "Draft from vN", its SQL the
//!   version's source (or `render_sql` of a pushed model), its layout and
//!   groups copied.
//! - `GET /orgs/{org}/databases/{db}/drafts/{id}` [member];
//!   `PATCH` [developer]: rename; `DELETE` [developer]: soft delete
//!   (`status = 'deleted'`; whoever has it open is told and let go).
//! - `GET …/drafts/{id}/publish` [developer]: what publishing would create
//!   (the diff against the base), and `POST …/drafts/{id}/publish`
//!   [developer]: freeze, compile, store as the next version ([`publish`]).
//!
//! - `GET …/drafts/{id}/migration?format=&concurrently=&name=` [member]:
//!   the migration from the draft's base version to what it holds now, as
//!   statements and as one migration tool's files ([`migration`]).
//! - `POST …/drafts/{id}/tidy` [developer]: rewrite the SQL in remus's
//!   canonical form, as a minimal edit ([`tidy`]).
//! - `POST …/drafts/{id}/update` [developer]: "Update draft", when someone
//!   published since the draft started: merges the latest version in
//!   ([`update`]); publishing a stale draft answers 409 `stale_base`.
//!
//! Tables: `drafts`, `draft_compiles` (with `collab`'s `draft_updates`,
//! `draft_snapshots`).

pub mod migration;
pub mod publish;
pub mod tidy;
pub mod update;
pub mod versions;

use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use sqlx::PgPool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    ApiError, ApiResult, AppState,
    auth::{Member, min},
    collab::{Hub, Present, compile, doc},
    databases::{self, DatabaseRef},
    domain::{DraftStatus, SourceKind},
    error::ErrorBody,
    extract::{Json, Path},
    orgs::invalid,
    versions::layout::VersionLayout,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get, rename, delete))
        .routes(routes!(publish::preview, publish::publish))
        .routes(routes!(migration::draft_migration))
        .routes(routes!(tidy::tidy))
        .routes(routes!(update::update))
}

/// A draft, as the database page and the editor show it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Draft {
    pub id: Uuid,
    pub name: String,
    pub status: DraftStatus,
    pub database: DraftDatabase,
    /// The version it started from; `null` when the database had none.
    pub base_version: Option<i32>,
    /// The database's newest version now.
    pub latest_version: Option<i32>,
    /// Someone published since the draft started: publishing it needs
    /// "Update draft" first.
    pub stale: bool,
    /// The version it became, once published.
    pub published_version: Option<i32>,
    /// `null` once their account is gone.
    pub created_by: Option<Person>,
    pub created_at: DateTime<Utc>,
    /// The last edit (or rename).
    pub updated_at: DateTime<Utc>,
    /// Who has it open right now.
    pub present: Vec<Present>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DraftDatabase {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Person {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RenameDraft {
    /// 1 to 200 characters.
    pub name: String,
}

struct Row {
    id: Uuid,
    name: String,
    status: DraftStatus,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    base_version: Option<i32>,
    published_version: Option<i32>,
    creator_id: Option<Uuid>,
    creator_name: Option<String>,
}

impl Row {
    fn into_draft(self, database: &DatabaseRef, latest: Option<i32>, hub: &Hub) -> Draft {
        let present = hub.present(&[self.id]).remove(&self.id).unwrap_or_default();
        Draft {
            id: self.id,
            name: self.name,
            status: self.status,
            database: DraftDatabase {
                id: database.id,
                slug: database.slug.clone(),
                name: database.name.clone(),
            },
            stale: self.status == DraftStatus::Open && self.base_version != latest,
            base_version: self.base_version,
            latest_version: latest,
            published_version: self.published_version,
            created_by: self
                .creator_id
                .zip(self.creator_name)
                .map(|(id, name)| Person { id, name }),
            created_at: self.created_at,
            updated_at: self.updated_at,
            present,
        }
    }
}

async fn latest_number(db: &PgPool, database_id: Uuid) -> ApiResult<Option<i32>> {
    Ok(sqlx::query_scalar!(
        "SELECT max(number) FROM versions WHERE database_id = $1",
        database_id
    )
    .fetch_one(db)
    .await?)
}

/// A draft of the database that is not deleted, or 404.
async fn find(db: &PgPool, database: &DatabaseRef, id: Uuid) -> ApiResult<Row> {
    sqlx::query_as!(
        Row,
        r#"SELECT d.id, d.name, d.status AS "status: DraftStatus", d.created_at, d.updated_at,
                  bv.number AS "base_version?", pv.number AS "published_version?",
                  u.id AS "creator_id?", u.name AS "creator_name?"
           FROM drafts d
           LEFT JOIN versions bv ON bv.id = d.base_version_id
           LEFT JOIN versions pv ON pv.id = d.published_version_id
           LEFT JOIN users u ON u.id = d.created_by
           WHERE d.id = $1 AND d.database_id = $2 AND d.status <> 'deleted'"#,
        id,
        database.id,
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(no_such_draft)
}

fn no_such_draft() -> ApiError {
    ApiError::not_found("No such draft. It may have been deleted.")
}

fn not_open(status: DraftStatus) -> ApiError {
    match status {
        DraftStatus::Published => ApiError::Conflict {
            code: "draft_published",
            message: "This draft was published. Its version keeps it.".into(),
        },
        DraftStatus::Open | DraftStatus::Deleted => no_such_draft(),
    }
}

/// The open drafts of a database, most recently edited first, with who has
/// each open.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/drafts",
    operation_id = "list_drafts",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path, description = "Database slug")),
    responses(
        (status = 200, body = Vec<Draft>),
        (status = 404, body = ErrorBody),
    )
)]
async fn list(
    member: Member,
    Path((_, db_slug)): Path<(String, String)>,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<Draft>>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let latest = latest_number(&state.db, database.id).await?;
    let rows = sqlx::query_as!(
        Row,
        r#"SELECT d.id, d.name, d.status AS "status: DraftStatus", d.created_at, d.updated_at,
                  bv.number AS "base_version?", pv.number AS "published_version?",
                  u.id AS "creator_id?", u.name AS "creator_name?"
           FROM drafts d
           LEFT JOIN versions bv ON bv.id = d.base_version_id
           LEFT JOIN versions pv ON pv.id = d.published_version_id
           LEFT JOIN users u ON u.id = d.created_by
           WHERE d.database_id = $1 AND d.status = 'open'
           ORDER BY d.updated_at DESC, d.id DESC"#,
        database.id,
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|row| row.into_draft(&database, latest, &state.collab))
            .collect(),
    ))
}

/// A new draft from the latest version, ready to edit: no form, renamed
/// later if wanted.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/drafts",
    operation_id = "create_draft",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path, description = "Database slug")),
    responses(
        (status = 201, body = Draft),
        (status = 403, description = "Reviewers cannot create drafts", body = ErrorBody),
        (status = 404, body = ErrorBody),
    )
)]
async fn create(
    member: Member<min::Developer>,
    Path((_, db_slug)): Path<(String, String)>,
    State(state): State<AppState>,
) -> ApiResult<(StatusCode, Json<Draft>)> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let base = versions::latest(&state.db, database.id).await?;

    let (name, sql, layout, model) = match &base {
        Some(version) => {
            // The version's model is what its SQL compiles to only when the
            // SQL is the source; SQL rendered from a pushed model is compiled
            // like any other text (the SQL is what counts).
            let model = (version.source_kind == SourceKind::Sql).then_some(&version.model);
            let sql = version.sql()?;
            (
                format!("Draft from v{}", version.number),
                sql,
                VersionLayout::from_value(&version.layout),
                model,
            )
        }
        None => (
            "New draft".to_owned(),
            String::new(),
            VersionLayout::default(),
            None,
        ),
    };

    let mut tx = state.db.begin().await?;
    let id = sqlx::query_scalar!(
        "INSERT INTO drafts (database_id, name, base_version_id, created_by)
         VALUES ($1, $2, $3, $4) RETURNING id",
        database.id,
        name,
        base.as_ref().map(|v| v.id),
        member.user.id,
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO draft_updates (draft_id, update) VALUES ($1, $2)",
        id,
        doc::initial_update(&sql, &layout),
    )
    .execute(&mut *tx)
    .await?;
    // The version's model is what its SQL compiles to: whoever opens the
    // draft sees the graph at once, before any sandbox run.
    if let Some(model) = model {
        let tables = serde_json::to_value(compile::created_at(&sql)).map_err(ApiError::internal)?;
        sqlx::query!(
            "INSERT INTO draft_compiles (draft_id, source_hash, model, good_model, good_tables)
             VALUES ($1, $2, $3, $3, $4)",
            id,
            sha2::Sha256::digest(sql.as_bytes()).to_vec(),
            model,
            tables,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    tracing::info!(draft = %id, database = %database.id, "draft created");

    let latest = base.as_ref().map(|v| v.number);
    let row = find(&state.db, &database, id).await?;
    Ok((
        StatusCode::CREATED,
        Json(row.into_draft(&database, latest, &state.collab)),
    ))
}

/// One draft (published ones too: the editor says what became of them).
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/drafts/{id}",
    operation_id = "get_draft",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = Draft), (status = 404, body = ErrorBody))
)]
async fn get(
    member: Member,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
) -> ApiResult<Json<Draft>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    let latest = latest_number(&state.db, database.id).await?;
    Ok(Json(row.into_draft(&database, latest, &state.collab)))
}

/// Renames an open draft; everyone in it sees the new name at once.
#[utoipa::path(
    patch,
    path = "/orgs/{org}/databases/{db}/drafts/{id}",
    operation_id = "rename_draft",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    request_body = RenameDraft,
    responses(
        (status = 200, body = Draft),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "Published drafts keep their name", body = ErrorBody),
        (status = 422, body = ErrorBody),
    )
)]
async fn rename(
    member: Member<min::Developer>,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
    Json(body): Json<RenameDraft>,
) -> ApiResult<Json<Draft>> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(invalid("name", "Give the draft a name."));
    }
    if name.chars().count() > 200 {
        return Err(invalid("name", "Keep the name to 200 characters or fewer."));
    }
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    if row.status != DraftStatus::Open {
        return Err(not_open(row.status));
    }
    sqlx::query!(
        "UPDATE drafts SET name = $2 WHERE id = $1 AND status = 'open'",
        id,
        name
    )
    .execute(&state.db)
    .await?;
    state.collab.renamed(id, name);
    let row = find(&state.db, &database, id).await?;
    let latest = latest_number(&state.db, database.id).await?;
    Ok(Json(row.into_draft(&database, latest, &state.collab)))
}

/// Deletes an open draft. Whoever has it open is told and let go.
#[utoipa::path(
    delete,
    path = "/orgs/{org}/databases/{db}/drafts/{id}",
    operation_id = "delete_draft",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "Published drafts stay with their version", body = ErrorBody),
    )
)]
async fn delete(
    member: Member<min::Developer>,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
) -> ApiResult<StatusCode> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    if row.status != DraftStatus::Open {
        return Err(not_open(row.status));
    }
    // The room first: it refuses while the draft is being published, and
    // takes no edit once it agreed.
    if state.collab.delete(id, &member.user.name).await.is_err() {
        return Err(ApiError::Conflict {
            code: "draft_busy",
            message: "This draft is being published or updated. Try again in a moment.".into(),
        });
    }
    let deleted = sqlx::query!(
        "UPDATE drafts SET status = 'deleted' WHERE id = $1 AND status = 'open'",
        id
    )
    .execute(&state.db)
    .await?;
    if deleted.rows_affected() != 1 {
        // Published just before (its room was not loaded when asked).
        return Err(not_open(DraftStatus::Published));
    }
    tracing::info!(draft = %id, "draft deleted");
    Ok(StatusCode::NO_CONTENT)
}
