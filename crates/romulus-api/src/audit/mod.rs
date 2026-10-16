//! The audit log: what an organization may need to account for, who did it
//! and why (docs/MVP.md "Review round 1"). Append only (a trigger refuses
//! updates and deletes); each event is written in the transaction of what it
//! records, so neither exists without the other.
//!
//! Endpoint (who may call it in brackets; see `auth::member`):
//! - `GET /orgs/{org}/databases/{db}/activity?before=&limit=` [developer]:
//!   the database's events, and the organization's own (the approvals
//!   required), newest first (its settings page).
//!
//! Table: `audit_events` (no update, delete or truncate but with the
//! organization or database). Written by `versions::removal` (a version
//! removed, restored, purged), `drafts::approvals` (a draft published, with
//! who approved it, or without the approvals it needed) and `orgs` (the
//! approvals required changed). The actor's name is kept as it was.

use axum::extract::State;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    ApiResult, AppState,
    auth::{Member, min},
    databases,
    domain::AuditAction,
    error::ErrorBody,
    extract::{Json, Path, Query},
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(activity))
}

/// What an event says beyond who, when and which version; the fields its
/// action has.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditDetails {
    /// The version's message (versions removed, restored, purged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The draft published (approvals overridden).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_name: Option<String>,
    /// The approvals it had, and how many the organization required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvals: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<i32>,
    /// Who approved the text published, when.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<Vec<AuditApprover>>,
    /// The approvals required before and after a change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<i32>,
}

/// One approval, as it was when the draft was published.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditApprover {
    /// `null` once their account is gone.
    pub user_id: Option<Uuid>,
    pub name: String,
    pub at: DateTime<Utc>,
}

/// Who did something: their name now, or as it was once their account is
/// gone.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuditActor {
    /// `null` once their account is gone.
    pub id: Option<Uuid>,
    pub name: String,
}

/// An event about to be recorded.
#[derive(Debug, Clone)]
pub struct Entry {
    pub org_id: Uuid,
    pub database_id: Option<Uuid>,
    pub action: AuditAction,
    /// `None`: romulus itself (the purge).
    pub actor_id: Option<Uuid>,
    /// Their name now: kept with the event.
    pub actor_name: Option<String>,
    pub version_number: Option<i32>,
    pub reason: Option<String>,
    pub details: AuditDetails,
}

/// Records `entry`, in the caller's transaction.
pub async fn record(db: impl PgExecutor<'_>, entry: &Entry) -> Result<(), sqlx::Error> {
    let details = serde_json::to_value(&entry.details).unwrap_or_default();
    sqlx::query!(
        "INSERT INTO audit_events (org_id, database_id, action, actor_id, actor_name, version_number, reason, details)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        entry.org_id,
        entry.database_id,
        entry.action as AuditAction,
        entry.actor_id,
        entry.actor_name,
        entry.version_number,
        entry.reason,
        details,
    )
    .execute(db)
    .await?;
    Ok(())
}

/// One event of the log.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuditEvent {
    pub id: Uuid,
    pub action: AuditAction,
    /// Who did it; `null` for what romulus did itself (the purge).
    pub actor: Option<AuditActor>,
    pub version_number: Option<i32>,
    /// Why, as they wrote it (removals, overrides).
    pub reason: Option<String>,
    pub details: AuditDetails,
    pub created_at: DateTime<Utc>,
}

/// A page of events, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ActivityPage {
    pub events: Vec<AuditEvent>,
    /// Pass as `before` for the next page; `null` on the last one.
    pub next_before: Option<Uuid>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListActivity {
    /// Events older than this one (from `next_before`).
    pub before: Option<Uuid>,
    /// 1 to 100; 50 by default.
    pub limit: Option<i64>,
}

/// The database's audit events, newest first. Developers and admins.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/activity",
    operation_id = "list_activity",
    tag = "audit",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        ListActivity,
    ),
    responses(
        (status = 200, body = ActivityPage),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
    )
)]
async fn activity(
    member: Member<min::Developer>,
    State(db): State<PgPool>,
    Path((_, slug)): Path<(String, String)>,
    Query(query): Query<ListActivity>,
) -> ApiResult<Json<ActivityPage>> {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let database = databases::find(&db, member.org.id, &slug).await?;
    let rows = sqlx::query!(
        r#"SELECT e.id, e.action AS "action: AuditAction", e.version_number, e.reason, e.details,
                  e.created_at, u.id AS "actor_id?", coalesce(u.name, e.actor_name) AS actor_name
           FROM audit_events e
           LEFT JOIN users u ON u.id = e.actor_id
           WHERE (e.database_id = $1 OR (e.database_id IS NULL AND e.org_id = $4))
             AND ($2::uuid IS NULL OR e.id < $2)
           ORDER BY e.id DESC LIMIT $3"#,
        database.id,
        query.before,
        limit + 1,
        member.org.id,
    )
    .fetch_all(&db)
    .await?;
    let page = usize::try_from(limit).unwrap_or(usize::MAX);
    let more = rows.len() > page;
    let events: Vec<AuditEvent> = rows
        .into_iter()
        .take(page)
        .map(|r| AuditEvent {
            id: r.id,
            action: r.action,
            actor: r.actor_name.map(|name| AuditActor {
                id: r.actor_id,
                name,
            }),
            version_number: r.version_number,
            reason: r.reason,
            details: serde_json::from_value(r.details).unwrap_or_default(),
            created_at: r.created_at,
        })
        .collect();
    let next_before = more.then(|| events.last().map(|e| e.id)).flatten();
    Ok(Json(ActivityPage {
        events,
        next_before,
    }))
}
