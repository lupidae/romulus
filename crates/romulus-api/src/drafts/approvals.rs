//! Approvals before publishing, and review requests (docs/MVP.md "Review
//! round 1").
//!
//! An organization may require N approvals (`organizations.approvals_required`,
//! 0: off) before a draft is published. An approval is a member's, of one
//! text: it counts while the SQL hashes as it did and its author has not
//! edited the draft (the collab room keeps both, see `collab::review`), so
//! any edit resets the approvals, live. Developers and reviewers approve
//! (admins too); whoever edited the SQL cannot. An admin may publish without
//! the approvals, saying why: the audit log keeps it. Pushes and imports are
//! never gated: only publishing a draft is.
//!
//! Endpoints (who may call them in brackets; see `auth::member`):
//! - `POST …/drafts/{id}/approvals` [member], `{"sha256"}` of the SQL the
//!   person reviewed: approves it (409 `draft_changed` if the text moved
//!   since, 403 `edited_draft` for an editor, 409 `approvals_off` when the
//!   organization requires none); `DELETE …/drafts/{id}/approvals`
//!   [member]: takes one's approval back.
//! - `POST …/drafts/{id}/review-request` [developer]: "Request review", a
//!   state the drafts list shows (no email yet); `DELETE` [developer]: no
//!   longer requested.
//!
//! Publishing ([`gate`]) checks the approvals of the frozen text.
//!
//! Tables: `draft_approvals`, `draft_review_requests` (and the room's
//! `draft_editors`).

use std::collections::HashMap;

use axum::extract::State;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{Person, find, no_such_draft, not_open, publish::hub_error};
use crate::{
    ApiError, ApiResult, AppState, audit,
    auth::{Member, min},
    collab::{
        Hub,
        protocol::ReviewRequest,
        review::{Approval, ApprovalError},
        room::Content,
    },
    databases::{self, DatabaseRef},
    domain::{AuditAction, DraftStatus, Role},
    error::ErrorBody,
    extract::{Json, Path},
    orgs::invalid,
};

/// Where a draft's review stands, as the drafts list shows it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DraftReview {
    /// Approvals publishing needs (the organization's setting); 0: none.
    pub required: i32,
    /// Approvals that count now: of the SQL as it is, by members who did
    /// not edit it.
    pub approvals: i32,
    /// The editors asked for a review.
    pub requested: Option<ReviewRequest>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ApproveDraft {
    /// SHA-256 (hex) of the SQL the person reviewed: an approval is of one
    /// text.
    pub sha256: String,
}

/// What publishing would need, for the publish dialog.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublishApprovals {
    /// Approvals publishing needs; 0: none.
    pub required: i32,
    /// Who approved the SQL previewed, without having edited it.
    pub approved_by: Vec<Person>,
    /// The caller may publish without them, saying why (admins).
    pub can_override: bool,
}

/// The organization's setting.
async fn required(db: &PgPool, org_id: Uuid) -> ApiResult<i32> {
    Ok(sqlx::query_scalar!(
        "SELECT approvals_required FROM organizations WHERE id = $1",
        org_id
    )
    .fetch_one(db)
    .await?
    .into())
}

/// Approves the draft's SQL as the caller reviewed it.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/drafts/{id}/approvals",
    operation_id = "approve_draft",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    request_body = ApproveDraft,
    responses(
        (status = 200, body = DraftReview),
        (status = 403, description = "`edited_draft`: they edited it, someone else approves", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`draft_changed` (review again), `approvals_off`, `draft_busy` (being published or updated), `draft_published`", body = ErrorBody),
        (status = 503, description = "The draft could not be loaded", body = ErrorBody),
    )
)]
pub(super) async fn approve(
    member: Member,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
    Json(body): Json<ApproveDraft>,
) -> ApiResult<Json<DraftReview>> {
    let sha256 = body.sha256.trim().to_ascii_lowercase();
    let digest = unhex(&sha256)
        .ok_or_else(|| invalid("sha256", "The SHA-256 of the SQL, in hex."))?;
    if required(&state.db, member.org.id).await? == 0 {
        return Err(ApiError::Conflict {
            code: "approvals_off",
            message: "This organization does not ask for approvals before publishing.".into(),
        });
    }
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    if row.status != DraftStatus::Open {
        return Err(not_open(row.status));
    }
    match state
        .collab
        .check_approval(id, member.user.id, sha256.clone())
        .await
        .map_err(hub_error)?
    {
        Ok(()) => {}
        Err(ApprovalError::Editor) => {
            return Err(ApiError::Denied {
                code: "edited_draft",
                message: "You edited this draft: someone who did not approves it.".into(),
            });
        }
        Err(ApprovalError::Changed) => {
            return Err(ApiError::Conflict {
                code: "draft_changed",
                message: "The draft changed while you were reviewing it. Look at it again, then approve."
                    .into(),
            });
        }
        Err(ApprovalError::NotOpen) => {
            return Err(ApiError::Conflict {
                code: "draft_busy",
                message: "This draft is being published or updated. Try again in a moment.".into(),
            });
        }
    }
    // An edit landing between the check and here leaves an approval of the
    // text before it, which simply does not count: it is of another text.
    let at = sqlx::query_scalar!(
        "INSERT INTO draft_approvals (draft_id, user_id, sql_sha256) VALUES ($1, $2, $3)
         ON CONFLICT (draft_id, user_id) DO UPDATE SET sql_sha256 = EXCLUDED.sql_sha256, approved_at = now()
         RETURNING approved_at",
        id,
        member.user.id,
        digest,
    )
    .fetch_one(&state.db)
    .await?;
    state.collab.approved(
        id,
        Approval {
            user_id: member.user.id,
            name: member.user.name.clone(),
            sha256,
            at,
        },
    );
    tracing::info!(draft = %id, user = %member.user.id, "draft approved");
    Ok(Json(review_of(&state, member.org.id, id).await?))
}

/// Takes the caller's approval back.
#[utoipa::path(
    delete,
    path = "/orgs/{org}/databases/{db}/drafts/{id}/approvals",
    operation_id = "withdraw_approval",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    responses(
        (status = 200, body = DraftReview),
        (status = 404, body = ErrorBody),
    )
)]
pub(super) async fn withdraw(
    member: Member,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
) -> ApiResult<Json<DraftReview>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    find(&state.db, &database, id).await?;
    sqlx::query!(
        "DELETE FROM draft_approvals WHERE draft_id = $1 AND user_id = $2",
        id,
        member.user.id,
    )
    .execute(&state.db)
    .await?;
    state.collab.withdrawn(id, member.user.id);
    Ok(Json(review_of(&state, member.org.id, id).await?))
}

/// "Request review": the drafts list shows the draft waits for one.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/drafts/{id}/review-request",
    operation_id = "request_review",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    responses(
        (status = 200, body = DraftReview),
        (status = 403, description = "Reviewers do not request reviews", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`draft_published`", body = ErrorBody),
    )
)]
pub(super) async fn request_review(
    member: Member<min::Developer>,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
) -> ApiResult<Json<DraftReview>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    if row.status != DraftStatus::Open {
        return Err(not_open(row.status));
    }
    let at = sqlx::query_scalar!(
        "INSERT INTO draft_review_requests (draft_id, requested_by) VALUES ($1, $2)
         ON CONFLICT (draft_id) DO UPDATE SET requested_by = EXCLUDED.requested_by, requested_at = now()
         RETURNING requested_at",
        id,
        member.user.id,
    )
    .fetch_one(&state.db)
    .await?;
    state.collab.review_requested(
        id,
        Some(ReviewRequest {
            user_id: Some(member.user.id),
            name: Some(member.user.name.clone()),
            at,
        }),
    );
    Ok(Json(review_of(&state, member.org.id, id).await?))
}

/// The draft no longer waits for a review.
#[utoipa::path(
    delete,
    path = "/orgs/{org}/databases/{db}/drafts/{id}/review-request",
    operation_id = "cancel_review_request",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    responses(
        (status = 200, body = DraftReview),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
    )
)]
pub(super) async fn cancel_review(
    member: Member<min::Developer>,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
) -> ApiResult<Json<DraftReview>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    find(&state.db, &database, id).await?;
    sqlx::query!("DELETE FROM draft_review_requests WHERE draft_id = $1", id)
        .execute(&state.db)
        .await?;
    state.collab.review_requested(id, None);
    Ok(Json(review_of(&state, member.org.id, id).await?))
}

/// One draft's review.
async fn review_of(state: &AppState, org_id: Uuid, draft: Uuid) -> ApiResult<DraftReview> {
    let mut all = reviews(&state.db, &state.collab, org_id, &[draft]).await?;
    all.remove(&draft).ok_or_else(no_such_draft)
}

/// The review of each of these drafts of the organization `org_id`. The
/// approvals that count come from the room when it is loaded (exact); for
/// the others, from the text the room last compiled (what it held when it
/// unloaded).
pub(super) async fn reviews(
    db: &PgPool,
    hub: &Hub,
    org_id: Uuid,
    drafts: &[Uuid],
) -> ApiResult<HashMap<Uuid, DraftReview>> {
    let required = required(db, org_id).await?;
    let rows = sqlx::query!(
        r#"SELECT d.id AS "id!", r.requested_at AS "requested_at?", u.id AS "user_id?", u.name AS "name?",
                  (SELECT count(*) FROM draft_approvals a
                   JOIN draft_compiles c ON c.draft_id = a.draft_id AND c.source_hash = a.sql_sha256
                   JOIN memberships m ON m.user_id = a.user_id AND m.org_id = $2
                   WHERE a.draft_id = d.id
                     AND NOT EXISTS (
                         SELECT 1 FROM draft_editors e WHERE e.draft_id = d.id AND e.user_id = a.user_id
                     )) AS "approvals!"
           FROM drafts d
           LEFT JOIN draft_review_requests r ON r.draft_id = d.id
           LEFT JOIN users u ON u.id = r.requested_by
           WHERE d.id = ANY($1)"#,
        drafts,
        org_id,
    )
    .fetch_all(db)
    .await?;
    let live = hub.approvals(drafts);
    Ok(rows
        .into_iter()
        .map(|r| {
            let approvals = live
                .get(&r.id)
                .map_or(r.approvals, |&n| i64::try_from(n).unwrap_or(i64::MAX));
            (
                r.id,
                DraftReview {
                    required,
                    approvals: i32::try_from(approvals).unwrap_or(i32::MAX),
                    requested: r.requested_at.map(|at| ReviewRequest {
                        user_id: r.user_id,
                        name: r.name,
                        at,
                    }),
                },
            )
        })
        .collect())
}

/// The members among the room's approvers of `content` (someone who left
/// the organization since no longer counts), in the order they approved.
async fn approvers(db: &PgPool, org_id: Uuid, content: &Content) -> ApiResult<Vec<Person>> {
    let rows = sqlx::query!(
        "SELECT u.id, u.name FROM users u
         JOIN memberships m ON m.user_id = u.id AND m.org_id = $1
         WHERE u.id = ANY($2)",
        org_id,
        &content.approved_by,
    )
    .fetch_all(db)
    .await?;
    let mut people: HashMap<Uuid, String> = rows.into_iter().map(|r| (r.id, r.name)).collect();
    Ok(content
        .approved_by
        .iter()
        .filter_map(|id| people.remove(id).map(|name| Person { id: *id, name }))
        .collect())
}

/// What the publish dialog shows of the approvals of `content`.
pub(super) async fn status(
    db: &PgPool,
    org_id: Uuid,
    role: Role,
    content: &Content,
) -> ApiResult<PublishApprovals> {
    let required = required(db, org_id).await?;
    Ok(PublishApprovals {
        required,
        approved_by: if required == 0 {
            Vec::new()
        } else {
            approvers(db, org_id, content).await?
        },
        can_override: role.can_manage_members(),
    })
}

/// Whether the frozen `content` may be published: approved enough, or an
/// admin publishing without the approvals, saying why (`override_reason`),
/// which answers the audit event to store with the version. 409
/// `approvals_required` otherwise (`details`: `required`, `approvals`).
pub(super) async fn gate(
    db: &PgPool,
    member: &Member<min::Developer>,
    database: &DatabaseRef,
    draft: Uuid,
    content: &Content,
    override_reason: Option<&str>,
) -> ApiResult<Option<audit::Entry>> {
    let required = required(db, member.org.id).await?;
    if required == 0 {
        return Ok(None);
    }
    let approvals = i32::try_from(approvers(db, member.org.id, content).await?.len())
        .unwrap_or(i32::MAX);
    if approvals >= required {
        return Ok(None);
    }
    let Some(reason) = override_reason.map(str::trim).filter(|r| !r.is_empty()) else {
        return Err(ApiError::Conflict {
            code: "approvals_required",
            message: format!(
                "Publishing this draft needs {required} {} from members who did not edit it; it has {approvals}.",
                if required == 1 {
                    "approval"
                } else {
                    "approvals"
                },
            ),
        });
    };
    if !member.role.can_manage_members() {
        return Err(ApiError::Denied {
            code: "override_admins_only",
            message: "Only admins can publish a draft without its approvals.".into(),
        });
    }
    if reason.chars().count() > 1000 {
        return Err(invalid(
            "override_reason",
            "Keep the reason to 1000 characters or fewer.",
        ));
    }
    let name = sqlx::query_scalar!("SELECT name FROM drafts WHERE id = $1", draft)
        .fetch_optional(db)
        .await?;
    Ok(Some(audit::Entry {
        org_id: member.org.id,
        database_id: Some(database.id),
        action: AuditAction::ApprovalsOverridden,
        actor_id: Some(member.user.id),
        // The store sets it: the new version's.
        version_number: None,
        reason: Some(reason.to_owned()),
        details: audit::AuditDetails {
            draft_id: Some(draft),
            draft_name: name,
            approvals: Some(approvals),
            required: Some(required),
            ..Default::default()
        },
    }))
}

/// A SHA-256 written in hex, as bytes.
fn unhex(hex: &str) -> Option<Vec<u8>> {
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}
