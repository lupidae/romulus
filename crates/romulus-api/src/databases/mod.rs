//! Databases inside an organization, and their API tokens.
//!
//! Endpoints (who may call them in brackets; see `auth::member`):
//! - `GET /orgs/{org}/databases` [member]: every live database, with its
//!   latest version, most recently active first. `POST /orgs/{org}/databases`
//!   [developer]: create one.
//! - `GET /orgs/{org}/databases/{db}` [member]; `PATCH` [developer]: name,
//!   slug, description; `DELETE` [developer]: soft delete (`deleted_at`; its
//!   versions stay, its slug is free again, its tokens are revoked).
//! - `GET/POST /orgs/{org}/databases/{db}/tokens`, `DELETE
//!   …/tokens/{id}` [developer]: API tokens (see [`tokens`]), and the
//!   [`tokens::Bearer`] extractor the machine API authenticates with.
//!
//! Tables: `databases`, `api_tokens`. Versions are the `versions` module's.

pub mod slug;
pub mod tokens;

use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    ApiError, ApiResult, AppState,
    auth::{Member, min},
    db,
    domain::{SourceKind, VersionOrigin},
    error::ErrorBody,
    extract::{Json, Path},
    orgs::invalid,
    versions::{Author, VersionSummary},
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get, update, delete))
        .merge(tokens::router())
}

/// A schema kept in romulus, as its organization's members see it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Database {
    pub id: Uuid,
    /// Its address: `/app/<org>/<slug>`.
    pub slug: String,
    pub name: String,
    /// Empty when none was given.
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The newest version (its `number` is also how many there are);
    /// `null` before the first upload or push.
    pub latest: Option<VersionSummary>,
    /// When something last happened: the newest version, or an edit.
    pub last_activity_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateDatabase {
    /// 1 to 100 characters.
    pub name: String,
    /// Lowercase letters, digits and single hyphens, at most 63 characters,
    /// not one of the organization's own pages.
    pub slug: String,
    /// At most 1000 characters.
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateDatabase {
    pub name: Option<String>,
    /// Changing it changes every link to the database (pushes use its id).
    pub slug: Option<String>,
    pub description: Option<String>,
}

/// The database a request is about, once found.
#[derive(Debug, Clone)]
pub struct DatabaseRef {
    pub id: Uuid,
    pub slug: String,
}

/// The live database `slug` of the organization `org_id`, or 404.
pub async fn find(db: &PgPool, org_id: Uuid, slug: &str) -> ApiResult<DatabaseRef> {
    sqlx::query_as!(
        DatabaseRef,
        "SELECT id, slug FROM databases WHERE org_id = $1 AND slug = $2 AND deleted_at IS NULL",
        org_id,
        slug,
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(no_such_database)
}

pub fn no_such_database() -> ApiError {
    ApiError::not_found("No such database. It may have been deleted or renamed.")
}

fn checked_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(invalid("name", "Give the database a name."));
    }
    if name.chars().count() > 100 {
        return Err(invalid("name", "Keep the name to 100 characters or fewer."));
    }
    Ok(name.to_owned())
}

fn checked_slug(value: &str) -> ApiResult<String> {
    let value = value.trim();
    match slug::problem(value) {
        Some(problem) => Err(invalid("slug", problem)),
        None => Ok(value.to_owned()),
    }
}

fn checked_description(description: &str) -> ApiResult<String> {
    let description = description.trim();
    if description.chars().count() > 1000 {
        return Err(invalid(
            "description",
            "Keep the description to 1000 characters or fewer.",
        ));
    }
    Ok(description.to_owned())
}

fn slug_taken(slug: &str) -> impl FnOnce(sqlx::Error) -> ApiError + '_ {
    move |err| {
        if db::is_unique_violation(&err) {
            ApiError::Conflict {
                code: "slug_taken",
                message: format!(
                    "This organization already has a database at {slug}. Try another URL."
                ),
            }
        } else {
            err.into()
        }
    }
}

/// The organization's live databases with their latest version: every one,
/// or only `slug`. One query for the list and the page, so they never
/// disagree.
async fn load(db: &PgPool, org_id: Uuid, slug: Option<&str>) -> ApiResult<Vec<Database>> {
    let rows = sqlx::query!(
        r#"SELECT d.id, d.slug, d.name, d.description, d.created_at, d.updated_at,
                  v.id AS "v_id?", v.number AS "v_number?",
                  v.origin AS "v_origin?: VersionOrigin",
                  v.source_kind AS "v_source_kind?: SourceKind",
                  v.message AS "v_message?", v.created_at AS "v_created_at?",
                  u.id AS "user_id?", u.name AS "user_name?",
                  t.id AS "token_id?", t.name AS "token_name?",
                  greatest(d.updated_at, v.created_at) AS "last_activity_at!"
           FROM databases d
           LEFT JOIN LATERAL (
               SELECT id, number, origin, source_kind, message, created_at, created_by, created_by_token
               FROM versions WHERE database_id = d.id ORDER BY number DESC LIMIT 1
           ) v ON true
           LEFT JOIN users u ON u.id = v.created_by
           LEFT JOIN api_tokens t ON t.id = v.created_by_token
           WHERE d.org_id = $1 AND d.deleted_at IS NULL AND ($2::text IS NULL OR d.slug = $2)
           ORDER BY greatest(d.updated_at, v.created_at) DESC, lower(d.name), d.slug"#,
        org_id,
        slug,
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let latest = match (
                r.v_id,
                r.v_number,
                r.v_origin,
                r.v_source_kind,
                r.v_created_at,
            ) {
                (Some(id), Some(number), Some(origin), Some(source_kind), Some(created_at)) => {
                    Some(VersionSummary {
                        id,
                        number,
                        origin,
                        source_kind,
                        message: r.v_message,
                        created_at,
                        author: Author::from_columns(
                            r.user_id,
                            r.user_name,
                            r.token_id,
                            r.token_name,
                        ),
                    })
                }
                _ => None,
            };
            Database {
                id: r.id,
                slug: r.slug,
                name: r.name,
                description: r.description,
                created_at: r.created_at,
                updated_at: r.updated_at,
                latest,
                last_activity_at: r.last_activity_at,
            }
        })
        .collect())
}

async fn load_one(db: &PgPool, org_id: Uuid, slug: &str) -> ApiResult<Database> {
    load(db, org_id, Some(slug))
        .await?
        .pop()
        .ok_or_else(no_such_database)
}

/// The organization's databases, most recently active first.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases",
    operation_id = "list_databases",
    tag = "databases",
    params(("org" = String, Path, description = "The organization's slug")),
    responses(
        (status = 200, body = Vec<Database>),
        (status = 404, description = "No such organization, or not a member", body = ErrorBody),
    )
)]
async fn list(member: Member, State(db): State<PgPool>) -> ApiResult<Json<Vec<Database>>> {
    Ok(Json(load(&db, member.org.id, None).await?))
}

/// Creates a database (with no version yet). Developers and admins.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases",
    operation_id = "create_database",
    tag = "databases",
    params(("org" = String, Path, description = "The organization's slug")),
    request_body = CreateDatabase,
    responses(
        (status = 201, body = Database),
        (status = 403, body = ErrorBody),
        (status = 409, description = "`slug_taken`", body = ErrorBody),
        (status = 422, description = "`validation`, `details` names the field", body = ErrorBody),
    )
)]
async fn create(
    member: Member<min::Developer>,
    State(db): State<PgPool>,
    Json(body): Json<CreateDatabase>,
) -> ApiResult<(StatusCode, Json<Database>)> {
    let name = checked_name(&body.name)?;
    let slug = checked_slug(&body.slug)?;
    let description = checked_description(&body.description)?;
    let row = sqlx::query!(
        "INSERT INTO databases (org_id, name, slug, description, created_by)
         VALUES ($1, $2, $3, $4, $5) RETURNING id, created_at, updated_at",
        member.org.id,
        name,
        slug,
        description,
        member.user.id,
    )
    .fetch_one(&db)
    .await
    .map_err(slug_taken(&slug))?;
    tracing::info!(org = %member.org.slug, database = %slug, "database created");
    Ok((
        StatusCode::CREATED,
        Json(Database {
            id: row.id,
            slug,
            name,
            description,
            created_at: row.created_at,
            updated_at: row.updated_at,
            latest: None,
            last_activity_at: row.updated_at,
        }),
    ))
}

/// One database.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}",
    operation_id = "get_database",
    tag = "databases",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
    ),
    responses(
        (status = 200, body = Database),
        (status = 404, description = "No such database (or organization)", body = ErrorBody),
    )
)]
async fn get(
    member: Member,
    State(db): State<PgPool>,
    Path((_, slug)): Path<(String, String)>,
) -> ApiResult<Json<Database>> {
    Ok(Json(load_one(&db, member.org.id, &slug).await?))
}

/// Renames a database, moves it to another slug, or changes its
/// description. Developers and admins.
#[utoipa::path(
    patch,
    path = "/orgs/{org}/databases/{db}",
    operation_id = "update_database",
    tag = "databases",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
    ),
    request_body = UpdateDatabase,
    responses(
        (status = 200, body = Database),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`slug_taken`", body = ErrorBody),
        (status = 422, body = ErrorBody),
    )
)]
async fn update(
    member: Member<min::Developer>,
    State(db): State<PgPool>,
    Path((_, slug)): Path<(String, String)>,
    Json(body): Json<UpdateDatabase>,
) -> ApiResult<Json<Database>> {
    let name = body.name.as_deref().map(checked_name).transpose()?;
    let new_slug = body.slug.as_deref().map(checked_slug).transpose()?;
    let description = body
        .description
        .as_deref()
        .map(checked_description)
        .transpose()?;
    let shown = new_slug.clone().unwrap_or_default();
    let updated = sqlx::query_scalar!(
        "UPDATE databases
         SET name = coalesce($3, name), slug = coalesce($4, slug), description = coalesce($5, description)
         WHERE org_id = $1 AND slug = $2 AND deleted_at IS NULL
         RETURNING slug",
        member.org.id,
        slug,
        name,
        new_slug,
        description,
    )
    .fetch_optional(&db)
    .await
    .map_err(slug_taken(&shown))?
    .ok_or_else(no_such_database)?;
    Ok(Json(load_one(&db, member.org.id, &updated).await?))
}

/// Deletes a database: it disappears from the organization, its versions
/// are kept but unreachable, its API tokens stop working, and its slug can be
/// used again. Developers and admins.
#[utoipa::path(
    delete,
    path = "/orgs/{org}/databases/{db}",
    operation_id = "delete_database",
    tag = "databases",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
    )
)]
async fn delete(
    member: Member<min::Developer>,
    State(db): State<PgPool>,
    Path((_, slug)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let mut tx = db.begin().await?;
    let id = sqlx::query_scalar!(
        "UPDATE databases SET deleted_at = now()
         WHERE org_id = $1 AND slug = $2 AND deleted_at IS NULL RETURNING id",
        member.org.id,
        slug,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(no_such_database)?;
    sqlx::query!(
        "UPDATE api_tokens SET revoked_at = now() WHERE database_id = $1 AND revoked_at IS NULL",
        id,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(org = %member.org.slug, database = %slug, "database deleted");
    Ok(StatusCode::NO_CONTENT)
}
