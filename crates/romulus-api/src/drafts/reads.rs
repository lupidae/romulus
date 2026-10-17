//! Unread markers for a draft's comment threads (`collab::comments`): up to
//! when each person read each thread. The threads themselves are in the
//! draft's doc; who read what is nobody else's business, so it is kept
//! here, per person, and carried over to the version when the draft is
//! published (`versions::discussion::freeze`).

use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{find, publish::hub_error};
use crate::{
    ApiError, ApiResult, AppState,
    auth::Member,
    databases,
    error::ErrorBody,
    extract::{Json, Path},
    versions::discussion::ReadUpTo,
};

/// Up to when the caller read a thread.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ThreadRead {
    /// The thread's id in the draft's doc.
    pub thread: Uuid,
    pub read_at: DateTime<Utc>,
}

/// Up to when the caller read each thread of the draft (threads never
/// opened are not listed).
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/drafts/{id}/reads",
    operation_id = "list_draft_reads",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    responses(
        (status = 200, body = Vec<ThreadRead>),
        (status = 404, body = ErrorBody),
    )
)]
pub(super) async fn list(
    member: Member,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<ThreadRead>>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    find(&state.db, &database, id).await?;
    Ok(Json(
        sqlx::query_as!(
            ThreadRead,
            "SELECT thread, read_at FROM draft_thread_reads WHERE draft_id = $1 AND user_id = $2",
            id,
            member.user.id,
        )
        .fetch_all(&state.db)
        .await?,
    ))
}

/// Marks a thread of the draft read up to `at` (never back).
#[utoipa::path(
    put,
    path = "/orgs/{org}/databases/{db}/drafts/{id}/reads/{thread}",
    operation_id = "read_draft_thread",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path), ("thread" = Uuid, Path)),
    request_body = ReadUpTo,
    responses(
        (status = 204, description = "Marked"),
        (status = 404, description = "No such draft, or no such thread in it", body = ErrorBody),
    )
)]
pub(super) async fn read(
    member: Member,
    Path((_, db_slug, id, thread)): Path<(String, String, Uuid, Uuid)>,
    State(state): State<AppState>,
    Json(body): Json<ReadUpTo>,
) -> ApiResult<StatusCode> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    find(&state.db, &database, id).await?;
    if !state
        .collab
        .thread_exists(id, thread)
        .await
        .map_err(hub_error)?
    {
        return Err(ApiError::not_found(
            "No such thread. It may have been deleted.",
        ));
    }
    sqlx::query!(
        // A draft holds 500 threads at most (`comments::MAX_THREADS`): marks
        // past twice that are of threads long gone, and not kept.
        "INSERT INTO draft_thread_reads (draft_id, user_id, thread, read_at)
         SELECT $1, $2, $3, $4
         WHERE (SELECT count(*) FROM draft_thread_reads WHERE draft_id = $1 AND user_id = $2) < 1000
            OR EXISTS (SELECT 1 FROM draft_thread_reads WHERE draft_id = $1 AND user_id = $2 AND thread = $3)
         ON CONFLICT (draft_id, user_id, thread) DO UPDATE
         SET read_at = greatest(draft_thread_reads.read_at, EXCLUDED.read_at)",
        id,
        member.user.id,
        thread,
        body.at.min(Utc::now()),
    )
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
