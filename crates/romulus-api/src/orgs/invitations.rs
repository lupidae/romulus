//! Copy-link invitations: an admin invites an email address with a role and
//! gets a one-time link; whoever opens it signed in with that verified email
//! joins with that role.
//!
//! The token in the link is shown once and stored as its sha256. Even a
//! leaked link (a log, a forwarded message) lets no one else in: accepting
//! requires the signed-in user's verified email to equal the invited one. A
//! link lasts [`TTL`]; inviting the same address again replaces it.

use std::time::Duration;

use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{Org, invalid};
use crate::{
    ApiError, ApiResult, AppState,
    auth::{CurrentUser, Member, min},
    db,
    domain::Role,
    error::ErrorBody,
    extract::{Json, Path},
    token,
};

/// How long an invitation link works.
pub const TTL: Duration = Duration::from_hours(7 * 24);

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(revoke))
        .routes(routes!(preview))
        .routes(routes!(accept))
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Person {
    pub id: Uuid,
    pub name: String,
}

/// A pending invitation (its link is never shown again).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Invitation {
    pub id: Uuid,
    pub email: String,
    pub role: Role,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// The link no longer works; inviting the address again makes a new one.
    pub expired: bool,
    /// `None` once that admin's account is deleted.
    pub invited_by: Option<Person>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreatedInvitation {
    pub invitation: Invitation,
    /// The one-time link to send to the invitee. Shown only now.
    pub url: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateInvitation {
    pub email: String,
    pub role: Role,
}

/// Pending invitations, newest first, expired ones included. Admins only.
#[utoipa::path(
    get,
    path = "/orgs/{org}/invitations",
    operation_id = "list_invitations",
    tag = "invitations",
    params(("org" = String, Path, description = "The organization's slug")),
    responses(
        (status = 200, body = Vec<Invitation>),
        (status = 403, body = ErrorBody),
    )
)]
async fn list(
    admin: Member<min::Admin>,
    State(db): State<PgPool>,
) -> ApiResult<Json<Vec<Invitation>>> {
    // Scalar subqueries, not LEFT JOINs: sqlx infers a joined column's
    // nullability from the query plan, which varies with the data, and the
    // offline query data (`.sqlx`) must not.
    let rows = sqlx::query!(
        r#"SELECT i.id, i.email, i.role AS "role: Role", i.created_at, i.expires_at,
                  i.expires_at <= now() AS "expired!", i.created_by AS "invited_by_id?",
                  (SELECT name FROM users WHERE id = i.created_by) AS "invited_by_name?"
           FROM invitations i
           WHERE i.org_id = $1 AND i.accepted_at IS NULL
           ORDER BY i.created_at DESC"#,
        admin.org.id,
    )
    .fetch_all(&db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|row| Invitation {
                id: row.id,
                email: row.email,
                role: row.role,
                created_at: row.created_at,
                expires_at: row.expires_at,
                expired: row.expired,
                invited_by: row
                    .invited_by_id
                    .zip(row.invited_by_name)
                    .map(|(id, name)| Person { id, name }),
            })
            .collect(),
    ))
}

/// A plausible address: something@something, no spaces, at most 254 bytes.
/// Deliverability is not our problem (nothing is sent); matching is.
fn checked_email(email: &str) -> ApiResult<String> {
    let email = &crate::email::normalize(email);
    let plausible = email.len() <= 254
        && !email.chars().any(char::is_whitespace)
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && !domain.is_empty() && !domain.contains('@')
        });
    if plausible {
        Ok(email.to_owned())
    } else {
        Err(invalid(
            "email",
            "Enter an email address, like name@company.com.",
        ))
    }
}

/// Invites an email address with a role and answers the one-time link.
/// Inviting an address that already has a pending invitation replaces it
/// (the old link stops working). Admins only.
#[utoipa::path(
    post,
    path = "/orgs/{org}/invitations",
    operation_id = "create_invitation",
    tag = "invitations",
    params(("org" = String, Path, description = "The organization's slug")),
    request_body = CreateInvitation,
    responses(
        (status = 201, body = CreatedInvitation),
        (status = 403, body = ErrorBody),
        (status = 409, description = "`already_member`", body = ErrorBody),
        (status = 422, body = ErrorBody),
    )
)]
async fn create(
    admin: Member<min::Admin>,
    State(state): State<AppState>,
    Json(body): Json<CreateInvitation>,
) -> ApiResult<(StatusCode, Json<CreatedInvitation>)> {
    let email = checked_email(&body.email)?;
    let member = sqlx::query_scalar!(
        "SELECT u.name FROM memberships m JOIN users u ON u.id = m.user_id
         WHERE m.org_id = $1
           AND lower(u.email COLLATE pg_unicode_fast) = lower($2 COLLATE pg_unicode_fast) LIMIT 1",
        admin.org.id,
        email,
    )
    .fetch_optional(&state.db)
    .await?;
    if let Some(name) = member {
        return Err(ApiError::Conflict {
            code: "already_member",
            message: format!(
                "{name} ({email}) is already a member of {}.",
                admin.org.name
            ),
        });
    }

    let link = token::generate();
    let row = sqlx::query!(
        r#"INSERT INTO invitations (org_id, email, role, token_hash, expires_at, created_by)
           VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5), $6)
           ON CONFLICT (org_id, lower(email COLLATE pg_unicode_fast)) WHERE accepted_at IS NULL DO UPDATE
           SET email = EXCLUDED.email, role = EXCLUDED.role, token_hash = EXCLUDED.token_hash,
               expires_at = EXCLUDED.expires_at, created_by = EXCLUDED.created_by, created_at = now()
           RETURNING id, created_at, expires_at"#,
        admin.org.id,
        email,
        body.role as Role,
        token::hash(&link),
        TTL.as_secs_f64(),
        admin.user.id,
    )
    .fetch_one(&state.db)
    .await?;
    tracing::info!(org = %admin.org.slug, role = ?body.role, "invitation created");

    let url = state
        .config
        .public_url
        .join(&format!("/app/invite/{link}"))
        .expect("a path joins any base URL");
    Ok((
        StatusCode::CREATED,
        Json(CreatedInvitation {
            invitation: Invitation {
                id: row.id,
                email,
                role: body.role,
                created_at: row.created_at,
                expires_at: row.expires_at,
                expired: false,
                invited_by: Some(Person {
                    id: admin.user.id,
                    name: admin.user.name,
                }),
            },
            url: url.into(),
        }),
    ))
}

/// Revokes a pending invitation: its link stops working. Admins only.
#[utoipa::path(
    delete,
    path = "/orgs/{org}/invitations/{id}",
    operation_id = "revoke_invitation",
    tag = "invitations",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("id" = Uuid, Path, description = "The invitation's id"),
    ),
    responses(
        (status = 204, description = "Revoked"),
        (status = 403, body = ErrorBody),
        (status = 404, description = "No such pending invitation", body = ErrorBody),
    )
)]
async fn revoke(
    admin: Member<min::Admin>,
    State(db): State<PgPool>,
    Path((_, id)): Path<(String, Uuid)>,
) -> ApiResult<StatusCode> {
    let revoked = sqlx::query!(
        "DELETE FROM invitations WHERE id = $1 AND org_id = $2 AND accepted_at IS NULL",
        id,
        admin.org.id,
    )
    .execute(&db)
    .await?
    .rows_affected();
    if revoked == 0 {
        return Err(ApiError::not_found("No such pending invitation."));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Where the viewer of an invitation link stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InvitationStatus {
    /// Signed out: sign in (then come back) to accept.
    SignInRequired,
    /// Signed in as the invitee: accepting works.
    Ready,
    /// Signed in with another email than the invited one.
    WrongAccount,
    /// Signed in as the invitee, but the IdP has not verified the email.
    EmailUnverified,
    /// The viewer is already a member of the organization.
    AlreadyMember,
    /// The link is older than seven days.
    Expired,
    /// Someone already accepted it.
    Used,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OrgSummary {
    pub name: String,
    pub slug: String,
}

/// What an invitation link invites to, for its landing page.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InvitationPreview {
    pub org: OrgSummary,
    /// The invited address: who has to be signed in to accept. Only while
    /// the link can still be used; a used or expired link no longer says.
    pub email: Option<String>,
    pub role: Role,
    pub invited_by: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub status: InvitationStatus,
    /// The viewer's role when `status` is `already_member`.
    pub member_role: Option<Role>,
}

struct Found {
    id: Uuid,
    org_id: Uuid,
    /// The viewer is signed in with the invited address (compared by
    /// Postgres, like every other email match: `lower()` on both sides).
    viewer_is_invitee: bool,
    preview: InvitationPreview,
}

/// The invitation a link names, judged for `viewer`; `None` for an unknown
/// (or revoked, or replaced) link, or one whose inviter is no longer an admin
/// of the organization: an invitation carries its author's rights, so it
/// dies with them (demoting or removing an admin also deletes their pending
/// invitations; this is the backstop).
async fn find(db: &PgPool, link: &str, viewer: Option<&CurrentUser>) -> ApiResult<Option<Found>> {
    if !token::is_well_formed(link) {
        return Ok(None);
    }
    let Some(row) = sqlx::query!(
        r#"SELECT i.id, i.org_id, i.email, i.role AS "role: Role", i.expires_at,
                  i.accepted_at IS NOT NULL AS "used!", i.expires_at <= now() AS "expired!",
                  o.name AS org_name, o.slug AS org_slug,
                  (SELECT name FROM users WHERE id = i.created_by) AS "invited_by?",
                  (SELECT role FROM memberships WHERE org_id = i.org_id AND user_id = $2)
                    AS "member_role?: Role",
                  coalesce(lower(i.email COLLATE pg_unicode_fast) = lower($3 COLLATE pg_unicode_fast), false)
                    AS "viewer_is_invitee!"
           FROM invitations i
           JOIN organizations o ON o.id = i.org_id
           WHERE i.token_hash = $1
             AND EXISTS (SELECT 1 FROM memberships author
                         WHERE author.org_id = i.org_id AND author.user_id = i.created_by
                           AND author.role = 'admin')"#,
        token::hash(link),
        viewer.map(|viewer| viewer.id),
        viewer.map(|viewer| viewer.email.as_str()),
    )
    .fetch_optional(db)
    .await?
    else {
        return Ok(None);
    };

    let status = match viewer {
        _ if row.member_role.is_some() => InvitationStatus::AlreadyMember,
        _ if row.used => InvitationStatus::Used,
        _ if row.expired => InvitationStatus::Expired,
        None => InvitationStatus::SignInRequired,
        Some(_) if !row.viewer_is_invitee => InvitationStatus::WrongAccount,
        Some(viewer) if !viewer.email_verified => InvitationStatus::EmailUnverified,
        Some(_) => InvitationStatus::Ready,
    };
    let live = !(row.used || row.expired);
    Ok(Some(Found {
        id: row.id,
        org_id: row.org_id,
        viewer_is_invitee: row.viewer_is_invitee,
        preview: InvitationPreview {
            org: OrgSummary {
                name: row.org_name,
                slug: row.org_slug,
            },
            email: live.then_some(row.email),
            role: row.role,
            invited_by: row.invited_by,
            expires_at: row.expires_at,
            status,
            member_role: row.member_role,
        },
    }))
}

fn not_found() -> ApiError {
    ApiError::not_found(
        "This invitation link does not work. It may have been revoked or replaced by a newer one.",
    )
}

/// What an invitation link invites to, and whether the viewer (signed in or
/// not) can accept it. Anyone holding the link may look.
#[utoipa::path(
    get,
    path = "/invitations/{token}",
    operation_id = "preview_invitation",
    tag = "invitations",
    params(("token" = String, Path, description = "The token from the invitation link")),
    responses(
        (status = 200, body = InvitationPreview),
        (status = 404, description = "Unknown, revoked or replaced link", body = ErrorBody),
    )
)]
async fn preview(
    viewer: Option<CurrentUser>,
    State(db): State<PgPool>,
    Path(link): Path<String>,
) -> ApiResult<Json<InvitationPreview>> {
    let found = find(&db, &link, viewer.as_ref())
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(found.preview))
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct Accepted {
    pub org: Org,
    /// The caller was a member already; nothing changed.
    pub already_member: bool,
}

/// Joins the organization with the invited role. The caller must be signed
/// in with the invited email, verified by the IdP.
#[utoipa::path(
    post,
    path = "/invitations/{token}/accept",
    operation_id = "accept_invitation",
    tag = "invitations",
    params(("token" = String, Path, description = "The token from the invitation link")),
    responses(
        (status = 200, body = Accepted),
        (status = 401, body = ErrorBody),
        (status = 403, description = "`wrong_account` or `email_unverified`", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`invitation_expired` or `invitation_used`", body = ErrorBody),
    )
)]
async fn accept(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(link): Path<String>,
) -> ApiResult<Json<Accepted>> {
    let found = find(&state.db, &link, Some(&user))
        .await?
        .ok_or_else(not_found)?;
    let preview = found.preview;
    let org = |role| Org {
        id: found.org_id,
        slug: preview.org.slug.clone(),
        name: preview.org.name.clone(),
        role,
    };
    match preview.status {
        InvitationStatus::AlreadyMember => {
            let role = preview.member_role.unwrap_or(preview.role);
            // The invitee joined some other way: this link has served.
            if found.viewer_is_invitee {
                sqlx::query!(
                    "UPDATE invitations SET accepted_at = now() WHERE id = $1 AND accepted_at IS NULL",
                    found.id
                )
                .execute(&state.db)
                .await?;
            }
            return Ok(Json(Accepted {
                org: org(role),
                already_member: true,
            }));
        }
        InvitationStatus::Ready => {}
        InvitationStatus::Used => return Err(used()),
        InvitationStatus::Expired => {
            return Err(ApiError::Conflict {
                code: "invitation_expired",
                message:
                    "This invitation expired. Ask an admin of the organization for a new link."
                        .to_owned(),
            });
        }
        InvitationStatus::WrongAccount => {
            return Err(ApiError::Denied {
                code: "wrong_account",
                message: format!(
                    "This invitation is for {}, and you are signed in as {}. Switch account to accept it.",
                    preview.email.as_deref().unwrap_or("another address"),
                    user.email
                ),
            });
        }
        InvitationStatus::EmailUnverified => {
            return Err(ApiError::Denied {
                code: "email_unverified",
                message: "Verify your email address with your identity provider, then try again."
                    .to_owned(),
            });
        }
        InvitationStatus::SignInRequired => return Err(ApiError::Unauthorized),
    }

    let mut tx = db::begin_read_committed(&state.db).await?;
    let role = claim(&mut tx, found.id, &link).await?.ok_or_else(used)?;
    sqlx::query!(
        "INSERT INTO memberships (org_id, user_id, role) VALUES ($1, $2, $3)
         ON CONFLICT (org_id, user_id) DO NOTHING",
        found.org_id,
        user.id,
        role as Role,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(org = %preview.org.slug, ?role, "invitation accepted");
    Ok(Json(Accepted {
        org: org(role),
        already_member: false,
    }))
}

/// Marks the invitation accepted and answers its role, or `None` when the
/// link can no longer be used. Of two concurrent accepts, one gets `None`.
/// Bound to the link's token, not just the row: inviting the address again
/// (a new link, perhaps another role) between the lookup and this claim
/// voids the old link; and still from an admin.
async fn claim(
    tx: &mut sqlx::PgConnection,
    id: Uuid,
    link: &str,
) -> Result<Option<Role>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"UPDATE invitations i SET accepted_at = now()
           WHERE i.id = $1 AND i.token_hash = $2 AND i.accepted_at IS NULL AND i.expires_at > now()
             AND EXISTS (SELECT 1 FROM memberships author
                         WHERE author.org_id = i.org_id AND author.user_id = i.created_by
                           AND author.role = 'admin')
           RETURNING i.role AS "role: Role""#,
        id,
        token::hash(link),
    )
    .fetch_optional(tx)
    .await
}

/// Deletes the pending invitations `author` sent in `org_id`: run in the
/// transaction that removes them or takes their admin role away, since an
/// invitation carries its author's rights.
pub(super) async fn revoke_sent_by(
    tx: &mut sqlx::PgConnection,
    org_id: Uuid,
    author: Uuid,
) -> Result<u64, sqlx::Error> {
    sqlx::query!(
        "DELETE FROM invitations WHERE org_id = $1 AND created_by = $2 AND accepted_at IS NULL",
        org_id,
        author,
    )
    .execute(tx)
    .await
    .map(|done| done.rows_affected())
}

fn used() -> ApiError {
    ApiError::Conflict {
        code: "invitation_used",
        message:
            "This invitation was already used. Ask an admin of the organization for a new link."
                .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    /// An invitation for `email` from a fresh admin; answers its id.
    async fn invitation(db: &PgPool, link: &str) -> Uuid {
        let org = sqlx::query_scalar!(
            "INSERT INTO organizations (name, slug) VALUES ('A', 'a') RETURNING id"
        )
        .fetch_one(db)
        .await
        .unwrap();
        let admin = sqlx::query_scalar!(
            "INSERT INTO users (oidc_subject, email, email_verified, name)
             VALUES ('s', 'admin@a.test', true, 'Admin') RETURNING id"
        )
        .fetch_one(db)
        .await
        .unwrap();
        sqlx::query!(
            "INSERT INTO memberships (org_id, user_id, role) VALUES ($1, $2, 'admin')",
            org,
            admin
        )
        .execute(db)
        .await
        .unwrap();
        sqlx::query_scalar!(
            "INSERT INTO invitations (org_id, email, role, token_hash, expires_at, created_by)
             VALUES ($1, 'bob@a.test', 'developer', $2, now() + interval '1 day', $3) RETURNING id",
            org,
            token::hash(link),
            admin,
        )
        .fetch_one(db)
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn a_link_replaced_after_the_lookup_cannot_be_claimed(db: PgPool) {
        let old = token::generate();
        let id = invitation(&db, &old).await;
        // Between the lookup of `old` and its claim, the admin invites the
        // address again: same row, new token, another role.
        let new = token::generate();
        sqlx::query!(
            "UPDATE invitations SET token_hash = $2, role = 'admin' WHERE id = $1",
            id,
            token::hash(&new),
        )
        .execute(&db)
        .await
        .unwrap();

        let mut conn = db.acquire().await.unwrap();
        assert_eq!(claim(&mut conn, id, &old).await.unwrap(), None);
        assert_eq!(claim(&mut conn, id, &new).await.unwrap(), Some(Role::Admin));
        assert_eq!(claim(&mut conn, id, &new).await.unwrap(), None, "once only");
    }
}
