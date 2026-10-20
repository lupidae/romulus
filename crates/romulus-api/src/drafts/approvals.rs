//! Approvals before publishing, and review requests (docs/MVP.md "Review
//! round 1").
//!
//! An organization may require N approvals (`organizations.approvals_required`,
//! 0: off) before a draft is published. An approval is a member's, of one
//! text: it counts while the SQL hashes as it did and its author has not
//! edited the draft (the collab room keeps both, see `collab::review`), so
//! any edit resets the approvals, live. Developers and reviewers approve
//! (admins too); whoever edited the SQL cannot, nor may developers and
//! admins approve a draft whose editors are unknown (`editors_unknown`: open
//! before editors were recorded). An admin may publish without the
//! approvals, saying why. Pushes and imports are never gated: only
//! publishing a draft is.
//!
//! Publishing freezes them ([`freeze`], in the transaction storing the
//! version, the draft row locked): who approved what is checked again
//! (withdrawn meanwhile? left the organization?), stored with the version
//! (`version_approvals`) and in the audit log, which a purge never touches.
//! Approving and withdrawing lock the draft row too, and only an open
//! draft's approvals change.
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

use super::{
    Person, find, no_such_draft, not_open,
    publish::{PublishDraft, hub_error},
};
use crate::{
    ApiError, ApiResult, AppState, audit,
    auth::{Member, min},
    collab::{
        Hub,
        protocol::ReviewRequest,
        review::{Approval, ApprovalError},
        room::Content,
    },
    databases,
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
    /// The renames turned on or off in the migration as the person saw them
    /// (the draft's, `SchemaMigration.renames[].key` → on or off): an
    /// approval is of those too, and refused (409 `draft_changed`) when the
    /// draft's are others now. Absent: none switched (`{}`).
    #[serde(default)]
    #[schema(value_type = std::collections::BTreeMap<String, bool>)]
    pub renames: std::collections::BTreeMap<String, bool>,
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
        (status = 403, description = "`edited_draft`: they edited it, someone else approves; `editors_unknown`: a developer or admin, on a draft whose editors are unknown", body = ErrorBody),
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
    if unhex(&sha256).is_none() {
        return Err(invalid("sha256", "The SHA-256 of the SQL, in hex."));
    }
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
    let editors_unknown =
        sqlx::query_scalar!("SELECT editors_unknown FROM drafts WHERE id = $1", id)
            .fetch_one(&state.db)
            .await?;
    if editors_unknown && member.role.can_write() {
        return Err(unknown_editors());
    }
    // What the approval is of: the text the person saw, on its base, and
    // the renames they saw.
    let (sha256, digest) = match state
        .collab
        .check_approval(id, member.user.id, sha256.clone(), body.renames)
        .await
        .map_err(hub_error)?
    {
        Ok(reviewed) => {
            let digest = unhex(&reviewed)
                .ok_or_else(|| ApiError::Internal("a reviewed hash that is not hex".into()))?;
            (reviewed, digest)
        }
        Err(ApprovalError::Editor) => {
            return Err(ApiError::Denied {
                code: "edited_draft",
                message: "You edited this draft: someone who did not approves it.".into(),
            });
        }
        Err(ApprovalError::Changed) => {
            return Err(ApiError::Conflict {
                code: "draft_changed",
                message:
                    "The draft changed while you were reviewing it. Look at it again, then approve."
                        .into(),
            });
        }
        Err(ApprovalError::NotOpen) => {
            return Err(ApiError::Conflict {
                code: "draft_busy",
                message: "This draft is being published or updated. Try again in a moment.".into(),
            });
        }
    };
    // An edit landing between the check and here leaves an approval of the
    // text before it, which simply does not count: it is of another text.
    // The draft row is locked as a publish locks it: never an approval of a
    // draft published meanwhile.
    let mut tx = state.db.begin().await?;
    lock_open(&mut tx, id).await?;
    let at = sqlx::query_scalar!(
        "INSERT INTO draft_approvals (draft_id, user_id, sql_sha256) VALUES ($1, $2, $3)
         ON CONFLICT (draft_id, user_id) DO UPDATE SET sql_sha256 = EXCLUDED.sql_sha256, approved_at = now()
         RETURNING approved_at",
        id,
        member.user.id,
        digest,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
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
        (status = 409, description = "`draft_published`: its approvals are the version's now", body = ErrorBody),
    )
)]
pub(super) async fn withdraw(
    member: Member,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
) -> ApiResult<Json<DraftReview>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    find(&state.db, &database, id).await?;
    let mut tx = state.db.begin().await?;
    lock_open(&mut tx, id).await?;
    sqlx::query!(
        "DELETE FROM draft_approvals WHERE draft_id = $1 AND user_id = $2",
        id,
        member.user.id,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
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

/// Locks the draft's row (as publishing does, `FOR UPDATE` there): 409 when
/// it is no longer open. Approvals of a published draft are its version's.
async fn lock_open(tx: &mut sqlx::PgConnection, draft: Uuid) -> ApiResult<()> {
    let status = sqlx::query_scalar!(
        r#"SELECT status AS "status: DraftStatus" FROM drafts WHERE id = $1 FOR SHARE"#,
        draft
    )
    .fetch_optional(tx)
    .await?
    .ok_or_else(no_such_draft)?;
    match status {
        DraftStatus::Open => Ok(()),
        other => Err(not_open(other)),
    }
}

fn unknown_editors() -> ApiError {
    ApiError::Denied {
        code: "editors_unknown",
        message: "This draft was edited before romulus recorded who edits drafts: a reviewer approves it, or start a new draft."
            .into(),
    }
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
    // Open drafts: what counts now. Published ones: what their version keeps.
    // The review request (one per draft) in scalar subqueries, not outer
    // joins: sqlx infers the same nullability whatever plan Postgres picks.
    let rows = sqlx::query!(
        r#"SELECT d.id AS "id!", d.status AS "status!: DraftStatus",
                  (SELECT r.requested_at FROM draft_review_requests r
                   WHERE r.draft_id = d.id) AS "requested_at?",
                  (SELECT u.id FROM draft_review_requests r JOIN users u ON u.id = r.requested_by
                   WHERE r.draft_id = d.id) AS "user_id?",
                  (SELECT u.name FROM draft_review_requests r JOIN users u ON u.id = r.requested_by
                   WHERE r.draft_id = d.id) AS "name?",
                  (SELECT count(*) FROM draft_approvals a
                   JOIN draft_compiles c ON c.draft_id = a.draft_id AND c.source_hash = a.sql_sha256
                   JOIN memberships m ON m.user_id = a.user_id AND m.org_id = $2
                   WHERE a.draft_id = d.id
                     AND NOT EXISTS (
                         SELECT 1 FROM draft_editors e WHERE e.draft_id = d.id AND e.user_id = a.user_id
                     )) AS "approvals!",
                  (SELECT count(*) FROM version_approvals va
                   WHERE va.version_id = d.published_version_id) AS "frozen!"
           FROM drafts d
           WHERE d.id = ANY($1)"#,
        drafts,
        org_id,
    )
    .fetch_all(db)
    .await?;
    // A loaded room knows exactly who approved the text as it is; only its
    // members count (as for publishing).
    let live = hub.approved_by(drafts);
    let everyone: Vec<Uuid> = live.values().flatten().copied().collect();
    let members: std::collections::HashSet<Uuid> = sqlx::query_scalar!(
        "SELECT user_id FROM memberships WHERE org_id = $1 AND user_id = ANY($2)",
        org_id,
        &everyone,
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .collect();
    Ok(rows
        .into_iter()
        .map(|r| {
            let approvals = match (r.status, live.get(&r.id)) {
                (DraftStatus::Open, Some(ids)) => {
                    i64::try_from(ids.iter().filter(|id| members.contains(id)).count())
                        .unwrap_or(i64::MAX)
                }
                (DraftStatus::Open, None) => r.approvals,
                (DraftStatus::Published, _) => r.frozen,
                (DraftStatus::Deleted, _) => 0,
            };
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

/// The approvals a publish stores with its version: checked again under
/// the locks the version is stored under ([`freeze`]).
#[derive(Debug, Clone)]
pub struct Freeze {
    pub org_id: Uuid,
    pub draft: Uuid,
    pub draft_name: Option<String>,
    /// The frozen text's SHA-256.
    pub sha256: Vec<u8>,
    /// Who the room says approved it without having edited it.
    pub approvers: Vec<Uuid>,
    pub required: i32,
    /// An admin publishes without enough approvals, saying why.
    pub override_reason: Option<String>,
    pub actor_id: Uuid,
    pub actor_name: String,
}

/// Whether the frozen `content` of `draft` may be published: approved
/// enough, or an admin publishing without the approvals, saying why
/// (`body.override_reason`). 409 `approvals_required` otherwise. What it
/// answers goes to the version's store, which checks it again and keeps
/// it ([`freeze`]); `None` when the organization requires no approval.
pub(super) async fn gate(
    state: &AppState,
    member: &Member<min::Developer>,
    draft: Uuid,
    content: &Content,
    body: &PublishDraft,
) -> ApiResult<Option<Freeze>> {
    let db = &state.db;
    let required = required(db, member.org.id).await?;
    if required == 0 {
        return Ok(None);
    }
    let approvals =
        i32::try_from(approvers(db, member.org.id, content).await?.len()).unwrap_or(i32::MAX);
    let mut override_reason = None;
    if approvals < required {
        let reason = body
            .override_reason
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .ok_or_else(|| missing(required, approvals))?;
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
        override_reason = Some(reason.to_owned());
    }
    let draft_name = sqlx::query_scalar!("SELECT name FROM drafts WHERE id = $1", draft)
        .fetch_optional(db)
        .await?;
    Ok(Some(Freeze {
        org_id: member.org.id,
        draft,
        draft_name,
        sha256: unhex(&super::publish::reviewed(content)).unwrap_or_default(),
        approvers: content.approved_by.clone(),
        required,
        override_reason,
        actor_id: member.user.id,
        actor_name: member.user.name.clone(),
    }))
}

fn missing(required: i32, approvals: i32) -> ApiError {
    ApiError::Conflict {
        code: "approvals_required",
        message: format!(
            "Publishing this draft needs {required} {} from members who did not edit it; it has {approvals}.",
            if required == 1 {
                "approval"
            } else {
                "approvals"
            },
        ),
    }
}

/// In the transaction storing version `number` (`version_id`) of
/// `database_id`, the draft row locked: who approved the frozen text and
/// is still a member, again (an approval withdrawn, or its author gone,
/// since the gate); refused (409 `approvals_required`) when that is too
/// few and no admin overrides. Then kept with the version and in the audit
/// log: published with these approvals, or without them, and why.
pub(crate) async fn freeze(
    tx: &mut sqlx::PgConnection,
    freeze: &Freeze,
    database_id: Uuid,
    version_id: Uuid,
    number: i32,
) -> ApiResult<()> {
    let rows = sqlx::query!(
        "SELECT a.user_id, u.name, a.approved_at FROM draft_approvals a
         JOIN users u ON u.id = a.user_id
         JOIN memberships m ON m.user_id = a.user_id AND m.org_id = $4
         WHERE a.draft_id = $1 AND a.sql_sha256 = $2 AND a.user_id = ANY($3)
         ORDER BY a.approved_at",
        freeze.draft,
        freeze.sha256,
        &freeze.approvers,
        freeze.org_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    let approvals = i32::try_from(rows.len()).unwrap_or(i32::MAX);
    let overridden = approvals < freeze.required;
    if overridden && freeze.override_reason.is_none() {
        return Err(missing(freeze.required, approvals));
    }
    for row in &rows {
        sqlx::query!(
            "INSERT INTO version_approvals (version_id, database_id, user_id, name, approved_at, sql_sha256)
             VALUES ($1, $2, $3, $4, $5, $6)",
            version_id,
            database_id,
            row.user_id,
            row.name,
            row.approved_at,
            freeze.sha256,
        )
        .execute(&mut *tx)
        .await?;
    }
    audit::record(
        &mut *tx,
        &audit::Entry {
            org_id: freeze.org_id,
            database_id: Some(database_id),
            action: if overridden {
                AuditAction::ApprovalsOverridden
            } else {
                AuditAction::VersionPublished
            },
            actor_id: Some(freeze.actor_id),
            actor_name: Some(freeze.actor_name.clone()),
            version_number: Some(number),
            reason: freeze.override_reason.clone().filter(|_| overridden),
            details: audit::AuditDetails {
                draft_id: Some(freeze.draft),
                draft_name: freeze.draft_name.clone(),
                approvals: Some(approvals),
                required: Some(freeze.required),
                approved_by: Some(
                    rows.into_iter()
                        .map(|r| audit::AuditApprover {
                            user_id: Some(r.user_id),
                            name: r.name,
                            at: r.approved_at,
                        })
                        .collect(),
                ),
                ..Default::default()
            },
        },
    )
    .await?;
    Ok(())
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
