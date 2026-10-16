//! Removing a version (docs/MVP.md "Review round 1"): an admin hides it,
//! saying why; it can be restored for [`RESTORE_DAYS`] days, then it is
//! purged.
//!
//! - A removed version is hidden from the history, diffs, migrations,
//!   imports (as if it had never been imported) and the latest version: a
//!   push is compared with the latest version left, a new draft starts from
//!   it, the database page shows it.
//! - Its number is never given again (`databases.last_version_number`).
//! - Drafts based on it are stale: they show "Update draft", which merges
//!   the latest version left into them, from the removed version's text
//!   (still there while it is only removed).
//! - Each removal, restore and purge is an audit event, written in its
//!   transaction.
//!
//! Endpoints (who may call them in brackets; see `auth::member`):
//! - `POST /orgs/{org}/databases/{db}/versions/{number}/remove` [admin],
//!   `{"reason"}`;
//! - `POST /orgs/{org}/databases/{db}/versions/{number}/restore` [admin],
//!   within [`RESTORE_DAYS`] days;
//! - `GET /orgs/{org}/databases/{db}/versions/removed` [developer]: the
//!   removed versions not purged yet, most recently removed first.
//!
//! The purge ([`purge`], every [`PURGE_EVERY`] by [`purger`]) deletes the
//! versions removed longer ago, and with them the drafts published as them.
//! A version an open draft is still based on waits for that draft to be
//! updated, published or deleted: the draft holds its text anyway, and
//! "Update draft" merges from it.

use std::time::Duration;

use axum::extract::State;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{Author, VersionSummary, summary};
use crate::{
    ApiError, ApiResult, AppState, audit,
    auth::{Member, min},
    databases::{self, no_such_database},
    db,
    domain::{AuditAction, SourceKind, VersionOrigin},
    drafts::Person,
    error::ErrorBody,
    extract::{Json, Path},
    orgs::invalid,
};

/// How long a removed version can be restored.
pub const RESTORE_DAYS: i32 = 30;
/// How often [`purger`] looks for versions to purge.
pub const PURGE_EVERY: Duration = Duration::from_secs(60 * 60);

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(remove))
        .routes(routes!(restore))
        .routes(routes!(removed))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RemoveVersion {
    /// Why, for the audit log and whoever restores it (1 to 1000
    /// characters).
    pub reason: String,
}

/// A removed version, until it is purged.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RemovedVersion {
    pub version: VersionSummary,
    pub removed_at: DateTime<Utc>,
    /// `null` once their account is gone.
    pub removed_by: Option<Person>,
    pub reason: String,
    /// It can be restored until then, and is purged soon after.
    pub restorable_until: DateTime<Utc>,
    /// It still can be (`restorable_until` is to come).
    pub restorable: bool,
}

/// Hides a version: see the module documentation. Admins.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/versions/{number}/remove",
    operation_id = "remove_version",
    tag = "versions",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        ("number" = i32, Path, description = "The version's number"),
    ),
    request_body = RemoveVersion,
    responses(
        (status = 200, body = RemovedVersion),
        (status = 403, description = "Only admins remove versions", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`version_removed`: it already is", body = ErrorBody),
        (status = 422, description = "`validation`: no reason, or too long", body = ErrorBody),
    )
)]
async fn remove(
    member: Member<min::Admin>,
    State(state): State<AppState>,
    Path((_, slug, number)): Path<(String, String, i32)>,
    Json(body): Json<RemoveVersion>,
) -> ApiResult<Json<RemovedVersion>> {
    let reason = body.reason.trim();
    if reason.is_empty() {
        return Err(invalid(
            "reason",
            "Say why it is removed: the history keeps it.",
        ));
    }
    if reason.chars().count() > 1000 {
        return Err(invalid(
            "reason",
            "Keep the reason to 1000 characters or fewer.",
        ));
    }
    let database = databases::find(&state.db, member.org.id, &slug).await?;
    let mut tx = state.db.begin().await?;
    // With the lock versions are created under: a publish or push in flight
    // compares with the latest version before or after, never half.
    lock(&mut tx, database.id).await?;
    let removed = sqlx::query!(
        "UPDATE versions SET removed_at = now(), removed_by = $3, removal_reason = $4
         WHERE database_id = $1 AND number = $2 AND removed_at IS NULL
         RETURNING message",
        database.id,
        number,
        member.user.id,
        reason,
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(removed) = removed else {
        return Err(match state_of(&mut tx, database.id, number).await? {
            Some(Some(_)) => ApiError::Conflict {
                code: "version_removed",
                message: format!("v{number} is removed already."),
            },
            _ => no_such_version(number),
        });
    };
    audit::record(
        &mut *tx,
        &audit::Entry {
            org_id: member.org.id,
            database_id: Some(database.id),
            action: AuditAction::VersionRemoved,
            actor_id: Some(member.user.id),
            version_number: Some(number),
            reason: Some(reason.to_owned()),
            details: audit::AuditDetails {
                message: removed.message,
                ..Default::default()
            },
        },
    )
    .await?;
    tx.commit().await?;
    // Drafts open on it hear it: those based on it need "Update draft".
    state
        .collab
        .version_removed(database.id, number, &member.user.name);
    tracing::info!(database = %database.id, number, "version removed");
    let version = list(&state.db, database.id, Some(number))
        .await?
        .pop()
        .ok_or_else(|| no_such_version(number))?;
    Ok(Json(version))
}

/// Brings a removed version back, within [`RESTORE_DAYS`] days. Admins.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/versions/{number}/restore",
    operation_id = "restore_version",
    tag = "versions",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        ("number" = i32, Path, description = "The version's number"),
    ),
    responses(
        (status = 200, body = VersionSummary),
        (status = 403, description = "Only admins restore versions", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`version_not_removed`, `restore_expired` (removed more than 30 days ago), `migration_imported_again` (its migration was imported again since)", body = ErrorBody),
    )
)]
async fn restore(
    member: Member<min::Admin>,
    State(state): State<AppState>,
    Path((_, slug, number)): Path<(String, String, i32)>,
) -> ApiResult<Json<VersionSummary>> {
    let database = databases::find(&state.db, member.org.id, &slug).await?;
    let mut tx = state.db.begin().await?;
    lock(&mut tx, database.id).await?;
    let restored = sqlx::query!(
        "UPDATE versions SET removed_at = NULL, removed_by = NULL, removal_reason = NULL
         WHERE database_id = $1 AND number = $2 AND removed_at IS NOT NULL
           AND removed_at > now() - make_interval(days => $3)
         RETURNING id, message",
        database.id,
        number,
        RESTORE_DAYS,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(|err| {
        if db::is_unique_violation(&err) {
            ApiError::Conflict {
                code: "migration_imported_again",
                message: format!(
                    "v{number}'s migration was imported again since it was removed. Remove that version first to restore this one."
                ),
            }
        } else {
            err.into()
        }
    })?;
    let Some(restored) = restored else {
        return Err(match state_of(&mut tx, database.id, number).await? {
            Some(None) => ApiError::Conflict {
                code: "version_not_removed",
                message: format!("v{number} is not removed."),
            },
            Some(Some(_)) => ApiError::Conflict {
                code: "restore_expired",
                message: format!(
                    "v{number} was removed more than {RESTORE_DAYS} days ago: it can no longer be restored."
                ),
            },
            None => no_such_version(number),
        });
    };
    audit::record(
        &mut *tx,
        &audit::Entry {
            org_id: member.org.id,
            database_id: Some(database.id),
            action: AuditAction::VersionRestored,
            actor_id: Some(member.user.id),
            version_number: Some(number),
            reason: None,
            details: audit::AuditDetails {
                message: restored.message,
                ..Default::default()
            },
        },
    )
    .await?;
    tx.commit().await?;
    state
        .collab
        .version_restored(database.id, number, &member.user.name);
    tracing::info!(database = %database.id, number, "version restored");
    Ok(Json(summary(&state.db, restored.id).await?))
}

/// The database's removed versions, most recently removed first. Developers
/// and admins.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/versions/removed",
    operation_id = "list_removed_versions",
    tag = "versions",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
    ),
    responses(
        (status = 200, body = Vec<RemovedVersion>),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
    )
)]
async fn removed(
    member: Member<min::Developer>,
    State(db): State<PgPool>,
    Path((_, slug)): Path<(String, String)>,
) -> ApiResult<Json<Vec<RemovedVersion>>> {
    let database = databases::find(&db, member.org.id, &slug).await?;
    Ok(Json(list(&db, database.id, None).await?))
}

async fn lock(tx: &mut sqlx::PgConnection, database_id: Uuid) -> ApiResult<()> {
    sqlx::query_scalar!(
        "SELECT id FROM databases WHERE id = $1 AND deleted_at IS NULL FOR NO KEY UPDATE",
        database_id,
    )
    .fetch_optional(tx)
    .await?
    .ok_or_else(no_such_database)?;
    Ok(())
}

/// Whether version `number` exists, and when it was removed if it was.
async fn state_of(
    tx: &mut sqlx::PgConnection,
    database_id: Uuid,
    number: i32,
) -> ApiResult<Option<Option<DateTime<Utc>>>> {
    Ok(sqlx::query_scalar!(
        "SELECT removed_at FROM versions WHERE database_id = $1 AND number = $2",
        database_id,
        number,
    )
    .fetch_optional(tx)
    .await?)
}

fn no_such_version(number: i32) -> ApiError {
    ApiError::not_found(format!("There is no version {number} of this database."))
}

/// The removed versions of a database (only `number`, when given).
async fn list(
    db: &PgPool,
    database_id: Uuid,
    number: Option<i32>,
) -> ApiResult<Vec<RemovedVersion>> {
    let rows = sqlx::query!(
        r#"SELECT v.id, v.number, v.origin AS "origin: VersionOrigin",
                  v.source_kind AS "source_kind: SourceKind", v.message, v.created_at,
                  v.removed_at AS "removed_at!", v.removal_reason AS "reason!",
                  v.removed_at + make_interval(days => $3) AS "restorable_until!",
                  v.removed_at > now() - make_interval(days => $3) AS "restorable!",
                  u.id AS "user_id?", u.name AS "user_name?", t.id AS "token_id?", t.name AS "token_name?",
                  r.id AS "remover_id?", r.name AS "remover_name?"
           FROM versions v
           LEFT JOIN users u ON u.id = v.created_by
           LEFT JOIN api_tokens t ON t.id = v.created_by_token
           LEFT JOIN users r ON r.id = v.removed_by
           WHERE v.database_id = $1 AND v.removed_at IS NOT NULL AND ($2::int IS NULL OR v.number = $2)
           ORDER BY v.removed_at DESC, v.number DESC"#,
        database_id,
        number,
        RESTORE_DAYS,
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| RemovedVersion {
            version: VersionSummary {
                id: r.id,
                number: r.number,
                origin: r.origin,
                source_kind: r.source_kind,
                message: r.message,
                created_at: r.created_at,
                author: Author::from_columns(r.user_id, r.user_name, r.token_id, r.token_name),
            },
            removed_at: r.removed_at,
            removed_by: r
                .remover_id
                .zip(r.remover_name)
                .map(|(id, name)| Person { id, name }),
            reason: r.reason,
            restorable_until: r.restorable_until,
            restorable: r.restorable,
        })
        .collect())
}

/// Purges what is due: versions removed more than [`RESTORE_DAYS`] days
/// ago that no open draft is based on, a hundred at most per call, each in
/// its own transaction. Returns how many.
pub async fn purge(db: &PgPool) -> Result<u64, sqlx::Error> {
    let due = sqlx::query_scalar!(
        "SELECT v.id FROM versions v
         WHERE v.removed_at < now() - make_interval(days => $1)
           AND NOT EXISTS (
               SELECT 1 FROM drafts d WHERE d.status = 'open'
                 AND (d.base_version_id = v.id OR d.pending_base_version_id = v.id)
           )
         ORDER BY v.removed_at LIMIT 100",
        RESTORE_DAYS,
    )
    .fetch_all(db)
    .await?;
    let mut purged = 0;
    for id in due {
        match purge_one(db, id).await {
            Ok(true) => purged += 1,
            Ok(false) => {}
            Err(err) => {
                tracing::warn!(version = %id, error = %err, "purging a removed version failed")
            }
        }
    }
    Ok(purged)
}

async fn purge_one(db: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    let Some(database) = sqlx::query!(
        "SELECT d.id, d.org_id FROM databases d JOIN versions v ON v.database_id = d.id
         WHERE v.id = $1 FOR NO KEY UPDATE OF d",
        id,
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(false);
    };
    // Still due under the lock: restored meanwhile, or a draft opened on it.
    let Some(version) = sqlx::query!(
        "SELECT number, message FROM versions v
         WHERE v.id = $1 AND v.removed_at < now() - make_interval(days => $2)
           AND NOT EXISTS (
               SELECT 1 FROM drafts d WHERE d.status = 'open'
                 AND (d.base_version_id = v.id OR d.pending_base_version_id = v.id)
           )",
        id,
        RESTORE_DAYS,
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(false);
    };
    // Closed drafts forget they started from it; a draft published as it
    // goes with it (its text is the version's).
    sqlx::query!(
        "UPDATE drafts SET base_version_id = NULL WHERE base_version_id = $1 AND status <> 'open'",
        id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE drafts SET pending_base_version_id = NULL, pending_base_sql = NULL
         WHERE pending_base_version_id = $1 AND status <> 'open'",
        id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM drafts WHERE published_version_id = $1", id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("DELETE FROM versions WHERE id = $1", id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        &audit::Entry {
            org_id: database.org_id,
            database_id: Some(database.id),
            action: AuditAction::VersionPurged,
            actor_id: None,
            version_number: Some(version.number),
            reason: None,
            details: audit::AuditDetails {
                message: version.message,
                ..Default::default()
            },
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(database = %database.id, number = version.number, "removed version purged");
    Ok(true)
}

/// Purges now, then every `every`.
pub fn purger(db: PgPool, every: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match purge(&db).await {
                Ok(0) => {}
                Ok(versions) => tracing::info!(versions, "purged removed versions"),
                Err(err) => tracing::warn!(error = %err, "purging removed versions failed"),
            }
            tokio::time::sleep(every).await;
        }
    })
}
