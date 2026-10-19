//! Organizations, memberships, roles and copy-link invitations.
//!
//! Endpoints (who may call them in brackets; see `auth::member`):
//! - `GET /orgs` [signed in]: mine, with my role. `POST /orgs` [signed in]:
//!   create one; its creator is its admin. Refused (403 `single_org`) on a
//!   single-organization install (`ROMULUS_SINGLE_ORG`), whose one
//!   organization `romulus-api setup` creates.
//! - `GET /orgs/{org}` [member], `PATCH /orgs/{org}` [admin]: name, slug,
//!   and how many approvals publishing a draft needs (`drafts::approvals`).
//! - `GET /orgs/{org}/members` [member]; `PATCH /orgs/{org}/members/{user}`
//!   [admin]: change a role; `DELETE /orgs/{org}/members/{user}` [admin, or
//!   the member themselves: leaving].
//! - `GET/POST /orgs/{org}/invitations`, `DELETE
//!   /orgs/{org}/invitations/{id}` [admin]: pending invitations, inviting
//!   (answers the one-time link), revoking.
//! - `GET /invitations/{token}` [anyone with the link]: what it invites to,
//!   and whether the viewer can accept; `POST /invitations/{token}/accept`
//!   [the invitee, signed in with that verified email].
//!
//! Tables: `organizations`, `memberships`, `invitations`. The last-admin rule
//! is a database trigger: a refused change surfaces as 409 `last_admin`
//! through `?`. Membership writes run at READ COMMITTED
//! (`db::begin_read_committed`), and every committed role change or removal
//! is published on [`events::MembershipEvents`], which closes that member's
//! websockets (`collab`).

pub mod events;
pub(crate) mod invitations;
mod members;
pub mod slug;

use axum::{extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    ApiError, ApiResult, AppState, audit,
    auth::{CurrentUser, Member, min},
    db,
    domain::{AuditAction, Role},
    error::ErrorBody,
    extract::Json,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get, update))
        .merge(members::router())
        .merge(invitations::router())
}

/// An organization, as one of its members sees it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Org {
    pub id: Uuid,
    /// Its address: `/app/<slug>`.
    pub slug: String,
    pub name: String,
    /// The caller's role in it.
    pub role: Role,
    /// Approvals publishing a draft needs, from members who did not edit
    /// it; 0: none (pushes and imports are never gated).
    pub approvals_required: i32,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateOrg {
    /// 1 to 100 characters.
    pub name: String,
    /// Lowercase letters, digits and single hyphens, at most 63 characters,
    /// not one of the app's own routes.
    pub slug: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateOrg {
    pub name: Option<String>,
    /// Changing it changes every link to the organization.
    pub slug: Option<String>,
    /// 0 (none) to 10.
    pub approvals_required: Option<i32>,
}

/// A 422 naming the field, in the shape the web app's forms read.
pub(crate) fn invalid(field: &str, message: &str) -> ApiError {
    ApiError::Validation {
        message: message.to_owned(),
        details: Some(json!({ field: message })),
    }
}

fn checked_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(invalid("name", "Give the organization a name."));
    }
    if name.chars().count() > 100 {
        return Err(invalid("name", "Keep the name to 100 characters or fewer."));
    }
    Ok(name.to_owned())
}

fn checked_slug(slug: &str) -> ApiResult<String> {
    let slug = slug.trim();
    match slug::problem(slug) {
        Some(problem) => Err(invalid("slug", problem)),
        None => Ok(slug.to_owned()),
    }
}

fn slug_taken(slug: &str) -> impl FnOnce(sqlx::Error) -> ApiError + '_ {
    move |err| {
        if db::is_unique_violation(&err) {
            ApiError::Conflict {
                code: "slug_taken",
                message: format!("{slug} is already taken. Try another URL."),
            }
        } else {
            err.into()
        }
    }
}

/// The organizations the caller belongs to, by name.
#[utoipa::path(
    get,
    path = "/orgs",
    operation_id = "list_orgs",
    tag = "orgs",
    responses(
        (status = 200, body = Vec<Org>),
        (status = 401, body = ErrorBody),
    )
)]
async fn list(user: CurrentUser, State(db): State<PgPool>) -> ApiResult<Json<Vec<Org>>> {
    let orgs = sqlx::query_as!(
        Org,
        r#"SELECT o.id, o.slug, o.name, m.role AS "role: Role", o.approvals_required::int AS "approvals_required!"
           FROM memberships m JOIN organizations o ON o.id = m.org_id
           WHERE m.user_id = $1
           ORDER BY lower(o.name), o.slug"#,
        user.id,
    )
    .fetch_all(&db)
    .await?;
    Ok(Json(orgs))
}

/// Creates an organization; the caller becomes its admin.
#[utoipa::path(
    post,
    path = "/orgs",
    operation_id = "create_org",
    tag = "orgs",
    request_body = CreateOrg,
    responses(
        (status = 201, body = Org),
        (status = 403, description = "`single_org`: this install keeps one organization", body = ErrorBody),
        (status = 409, description = "`slug_taken`", body = ErrorBody),
        (status = 422, description = "`validation`, `details` names the field", body = ErrorBody),
    )
)]
async fn create(
    user: CurrentUser,
    State(db): State<PgPool>,
    State(config): State<std::sync::Arc<crate::Config>>,
    Json(body): Json<CreateOrg>,
) -> ApiResult<(StatusCode, Json<Org>)> {
    if config.single_org {
        return Err(ApiError::Denied {
            code: "single_org",
            message: "This server keeps one organization. Ask its admin to invite you.".to_owned(),
        });
    }
    let name = checked_name(&body.name)?;
    let slug = checked_slug(&body.slug)?;
    let mut tx = db::begin_read_committed(&db).await?;
    let id = sqlx::query_scalar!(
        "INSERT INTO organizations (name, slug) VALUES ($1, $2) RETURNING id",
        name,
        slug,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(slug_taken(&slug))?;
    sqlx::query!(
        "INSERT INTO memberships (org_id, user_id, role) VALUES ($1, $2, 'admin')",
        id,
        user.id,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(org = %slug, "organization created");
    Ok((
        StatusCode::CREATED,
        Json(Org {
            id,
            slug,
            name,
            role: Role::Admin,
            approvals_required: 0,
        }),
    ))
}

/// One organization the caller belongs to.
#[utoipa::path(
    get,
    path = "/orgs/{org}",
    operation_id = "get_org",
    tag = "orgs",
    params(("org" = String, Path, description = "The organization's slug")),
    responses(
        (status = 200, body = Org),
        (status = 404, description = "No such organization, or not a member", body = ErrorBody),
    )
)]
async fn get(member: Member) -> Json<Org> {
    Json(Org {
        id: member.org.id,
        slug: member.org.slug,
        name: member.org.name,
        role: member.role,
        approvals_required: member.org.approvals_required.into(),
    })
}

/// Renames an organization, moves it to another slug, or changes how many
/// approvals publishing needs. Admins only.
#[utoipa::path(
    patch,
    path = "/orgs/{org}",
    operation_id = "update_org",
    tag = "orgs",
    params(("org" = String, Path, description = "The organization's slug")),
    request_body = UpdateOrg,
    responses(
        (status = 200, body = Org),
        (status = 403, body = ErrorBody),
        (status = 409, description = "`slug_taken`", body = ErrorBody),
        (status = 422, body = ErrorBody),
    )
)]
async fn update(
    member: Member<min::Admin>,
    State(db): State<PgPool>,
    State(collab): State<crate::collab::Hub>,
    Json(body): Json<UpdateOrg>,
) -> ApiResult<Json<Org>> {
    let name = body.name.as_deref().map(checked_name).transpose()?;
    let slug = body.slug.as_deref().map(checked_slug).transpose()?;
    let approvals = body
        .approvals_required
        .map(|n| {
            i16::try_from(n)
                .ok()
                .filter(|n| (0..=MAX_APPROVALS).contains(n))
                .ok_or_else(|| {
                    invalid(
                        "approvals_required",
                        "Between 0 (no approvals) and 10 approvals.",
                    )
                })
        })
        .transpose()?;
    let shown = slug.clone().unwrap_or_default();
    let mut tx = db.begin().await?;
    let before = sqlx::query_scalar!(
        "SELECT approvals_required FROM organizations WHERE id = $1 FOR UPDATE",
        member.org.id,
    )
    .fetch_one(&mut *tx)
    .await?;
    let org = sqlx::query!(
        "UPDATE organizations
         SET name = coalesce($2, name), slug = coalesce($3, slug),
             approvals_required = coalesce($4, approvals_required)
         WHERE id = $1 RETURNING slug, name, approvals_required",
        member.org.id,
        name,
        slug,
        approvals,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(slug_taken(&shown))?;
    let changed = org.approvals_required != before;
    if changed {
        // Who changed what publishing needs, from what: in the audit log.
        audit::record(
            &mut *tx,
            &audit::Entry {
                org_id: member.org.id,
                database_id: None,
                action: AuditAction::ApprovalsRequiredChanged,
                actor_id: Some(member.user.id),
                actor_name: Some(member.user.name.clone()),
                version_number: None,
                reason: None,
                details: audit::AuditDetails {
                    from: Some(before.into()),
                    to: Some(org.approvals_required.into()),
                    ..Default::default()
                },
            },
        )
        .await?;
    }
    tx.commit().await?;
    if changed {
        // Its drafts open now show the new count at once.
        collab.approvals_required(member.org.id, org.approvals_required.into());
        tracing::info!(org = %org.slug, approvals = org.approvals_required, "approvals required changed");
    }
    Ok(Json(Org {
        id: member.org.id,
        slug: org.slug,
        name: org.name,
        role: member.role,
        approvals_required: org.approvals_required.into(),
    }))
}

/// Approvals an organization may require, at most.
const MAX_APPROVALS: i16 = 10;
