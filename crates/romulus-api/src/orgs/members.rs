//! `/orgs/{org}/members`: who is in an organization, with which role.

use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::events::MembershipChange;
use crate::{
    ApiError, ApiResult, AppState,
    auth::{Member, min, min::MinRole},
    db,
    domain::Role,
    error::ErrorBody,
    extract::{Json, Path},
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(change_role, remove))
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OrgMember {
    pub user_id: Uuid,
    pub name: String,
    pub email: String,
    pub role: Role,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ChangeRole {
    pub role: Role,
}

/// Everyone in the organization, by name. Any member may see who else is.
#[utoipa::path(
    get,
    path = "/orgs/{org}/members",
    operation_id = "list_members",
    tag = "orgs",
    params(("org" = String, Path, description = "The organization's slug")),
    responses(
        (status = 200, body = Vec<OrgMember>),
        (status = 404, body = ErrorBody),
    )
)]
async fn list(member: Member, State(state): State<AppState>) -> ApiResult<Json<Vec<OrgMember>>> {
    let members = sqlx::query_as!(
        OrgMember,
        r#"SELECT u.id AS user_id, u.name, u.email, m.role AS "role: Role", m.created_at AS joined_at
           FROM memberships m JOIN users u ON u.id = m.user_id
           WHERE m.org_id = $1
           ORDER BY lower(u.name), u.email"#,
        member.org.id,
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(members))
}

/// Changes a member's role. Admins only; the last admin cannot be demoted
/// (409 `last_admin`). Closes the member's live connections.
#[utoipa::path(
    patch,
    path = "/orgs/{org}/members/{user}",
    operation_id = "change_member_role",
    tag = "orgs",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("user" = Uuid, Path, description = "The member's user id"),
    ),
    request_body = ChangeRole,
    responses(
        (status = 200, body = OrgMember),
        (status = 403, body = ErrorBody),
        (status = 404, description = "No such member", body = ErrorBody),
        (status = 409, description = "`last_admin`", body = ErrorBody),
    )
)]
async fn change_role(
    admin: Member<min::Admin>,
    State(state): State<AppState>,
    Path((_, user_id)): Path<(String, Uuid)>,
    Json(body): Json<ChangeRole>,
) -> ApiResult<Json<OrgMember>> {
    let mut tx = db::begin_read_committed(&state.db).await?;
    let changed = sqlx::query!(
        r#"UPDATE memberships m SET role = $3
           FROM users u
           WHERE m.org_id = $1 AND m.user_id = $2 AND u.id = m.user_id
           RETURNING u.name, u.email, m.created_at, old.role AS "previous!: Role""#,
        admin.org.id,
        user_id,
        body.role as Role,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::not_found("No such member."))?;
    tx.commit().await?;

    if changed.previous != body.role {
        tracing::info!(org = %admin.org.slug, user = %user_id, role = ?body.role, "role changed");
        state.membership.publish(MembershipChange::RoleChanged {
            org_id: admin.org.id,
            user_id,
            role: body.role,
        });
    }
    Ok(Json(OrgMember {
        user_id,
        name: changed.name,
        email: changed.email,
        role: body.role,
        joined_at: changed.created_at,
    }))
}

/// Removes a member. Admins may remove anyone; any member may remove
/// themselves (leave). The last admin cannot go (409 `last_admin`). Closes
/// the member's live connections.
#[utoipa::path(
    delete,
    path = "/orgs/{org}/members/{user}",
    operation_id = "remove_member",
    tag = "orgs",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("user" = Uuid, Path, description = "The member's user id"),
    ),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, body = ErrorBody),
        (status = 404, description = "No such member", body = ErrorBody),
        (status = 409, description = "`last_admin`", body = ErrorBody),
    )
)]
async fn remove(
    member: Member,
    State(state): State<AppState>,
    Path((_, user_id)): Path<(String, Uuid)>,
) -> ApiResult<StatusCode> {
    let leaving = member.user.id == user_id;
    if !leaving && !member.role.can_manage_members() {
        return Err(ApiError::Forbidden(min::Admin::DENIED.to_owned()));
    }
    let mut tx = db::begin_read_committed(&state.db).await?;
    let removed = sqlx::query_scalar!(
        "DELETE FROM memberships WHERE org_id = $1 AND user_id = $2 RETURNING user_id",
        member.org.id,
        user_id,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(|err| match ApiError::from(err) {
        ApiError::Conflict { code: "last_admin", .. } if leaving => ApiError::Conflict {
            code: "last_admin",
            message: "You are this organization's only admin. Make another member an admin before you leave."
                .to_owned(),
        },
        other => other,
    })?;
    if removed.is_none() {
        return Err(ApiError::not_found("No such member."));
    }
    tx.commit().await?;

    tracing::info!(org = %member.org.slug, user = %user_id, leaving, "member removed");
    state.membership.publish(MembershipChange::Removed {
        org_id: member.org.id,
        user_id,
    });
    Ok(StatusCode::NO_CONTENT)
}
