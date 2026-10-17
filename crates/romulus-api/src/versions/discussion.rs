//! A version's discussion (docs/MVP.md "Review round 1", item 6): the
//! comment threads of the draft it was published from, frozen as they were
//! (review history, read-only), and threads started on the version's page
//! since (its schema is read-only; comments are not). Everyone in the
//! organization comments, reviewers included. Never `COMMENT ON`.
//!
//! Endpoints [member]:
//! - `GET …/versions/{number}/threads`: oldest first, each with its comments
//!   and where the caller read it up to (unread markers).
//! - `POST …/versions/{number}/threads`: a new thread with its first comment.
//! - `POST …/threads/{thread}/comments`: a reply.
//! - `PATCH …/threads/{thread}/comments/{comment}`: its author's only (403
//!   `not_yours`).
//! - `DELETE …/threads/{thread}/comments/{comment}`: its author's, or anyone's
//!   for developers and admins (in the audit log, `comment_deleted`);
//!   deleting a thread's last comment deletes it.
//! - `POST …/threads/{thread}/resolve`, `…/reopen`.
//! - `PUT …/threads/{thread}/read`: read up to `at`.
//!
//! A frozen thread answers 409 `thread_frozen` to every change but reading
//! (a trigger refuses it too). As in a draft, what the page's own
//! discussion weighs is capped, in all and per person (409 `too_much`,
//! `discussion_full`: `collab::comments::MAX_DISCUSSION_BYTES`,
//! `MAX_PERSON_BYTES`, of comment text), and changes are rate limited per
//! person (`rate::Limits::version_comments`, 429). [`freeze`] writes a draft's threads in the
//! transaction publishing it, with where everyone had read them up to.
//!
//! Tables: `version_threads`, `version_comments`, `version_thread_reads`.

use std::{collections::HashMap, sync::Arc};

use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    ApiError, ApiResult, AppState, audit,
    auth::Member,
    collab::comments::{
        Anchor, DocThread, MAX_BODY_CHARS, MAX_DISCUSSION_BYTES, MAX_MENTIONS, MAX_PERSON_BYTES,
        excerpt,
    },
    databases,
    domain::AuditAction,
    error::ErrorBody,
    extract::{Json, Path},
    orgs::invalid,
    rate::Limits,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(reply))
        .routes(routes!(edit, delete))
        .routes(routes!(resolve))
        .routes(routes!(reopen))
        .routes(routes!(read))
}

/// Threads a version takes on its page, at most.
const MAX_THREADS: i64 = 500;
/// Comments one thread takes, at most.
const MAX_COMMENTS_PER_THREAD: i64 = 200;

/// A thread of a version's discussion.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VersionThread {
    pub id: Uuid,
    pub anchor: Anchor,
    /// From the review of the draft the version was published from, as it
    /// was then: read-only.
    pub frozen: bool,
    /// Who started it (`null` once their account is gone).
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub resolved: Option<ThreadResolution>,
    /// Oldest first.
    pub comments: Vec<VersionComment>,
    /// Up to when the caller read it (`null`: never opened).
    pub read_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ThreadResolution {
    /// `null` once their account is gone.
    pub by: Option<Uuid>,
    /// Their name then.
    pub name: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VersionComment {
    pub id: Uuid,
    /// `null` once their account is gone.
    pub author: Option<Uuid>,
    /// Their name when they wrote it.
    pub author_name: String,
    pub body: String,
    /// Members @mentioned.
    pub mentions: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewThread {
    pub anchor: Anchor,
    /// The first comment: 1 to 4000 characters.
    pub body: String,
    /// Members @mentioned (others are dropped), 20 at most.
    #[serde(default)]
    pub mentions: Vec<Uuid>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CommentText {
    /// 1 to 4000 characters.
    pub body: String,
    #[serde(default)]
    pub mentions: Vec<Uuid>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ReadUpTo {
    /// The newest comment the person saw.
    pub at: DateTime<Utc>,
}

/// The version's id, a live one of a database of the caller's organization.
async fn version_id(db: &PgPool, member: &Member, slug: &str, number: i32) -> ApiResult<Uuid> {
    let database = databases::find(db, member.org.id, slug).await?;
    match sqlx::query_scalar!(
        "SELECT id FROM versions WHERE database_id = $1 AND number = $2 AND removed_at IS NULL",
        database.id,
        number,
    )
    .fetch_optional(db)
    .await?
    {
        Some(id) => Ok(id),
        None => Err(super::missing(db, database.id, Some(number)).await),
    }
}

/// A thread of that version, whether it is frozen.
async fn thread_of(db: &PgPool, version: Uuid, thread: Uuid) -> ApiResult<bool> {
    sqlx::query_scalar!(
        "SELECT frozen FROM version_threads WHERE id = $1 AND version_id = $2",
        thread,
        version,
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| ApiError::not_found("No such thread. It may have been deleted."))
}

fn frozen() -> ApiError {
    ApiError::Conflict {
        code: "thread_frozen",
        message: "This thread is from the draft's review and is kept as it was. Start a new thread to discuss it further."
            .into(),
    }
}

fn checked_body(body: &str) -> ApiResult<String> {
    if body.trim().is_empty() {
        return Err(invalid("body", "Write something first."));
    }
    if body.chars().count() > MAX_BODY_CHARS {
        return Err(invalid(
            "body",
            "Keep a comment to 4000 characters or fewer.",
        ));
    }
    Ok(body.to_owned())
}

/// Locks the version's discussion (one change at a time: the weights below
/// hold) and refuses `bytes` more of text by `person` past its caps.
async fn room_for(
    tx: &mut Transaction<'_, Postgres>,
    version: Uuid,
    person: Uuid,
    bytes: usize,
) -> ApiResult<()> {
    sqlx::query_scalar!(
        "SELECT id FROM versions WHERE id = $1 FOR NO KEY UPDATE",
        version
    )
    .fetch_one(&mut **tx)
    .await?;
    let weights = sqlx::query!(
        r#"SELECT coalesce(sum(octet_length(c.body)), 0)::bigint AS "total!",
                  coalesce(sum(octet_length(c.body)) FILTER (WHERE c.author_id = $2), 0)::bigint AS "mine!"
           FROM version_comments c JOIN version_threads t ON t.id = c.thread_id
           WHERE t.version_id = $1 AND NOT t.frozen"#,
        version,
        person,
    )
    .fetch_one(&mut **tx)
    .await?;
    let weight = |n: i64| usize::try_from(n).unwrap_or(usize::MAX);
    if weight(weights.mine).saturating_add(bytes) > MAX_PERSON_BYTES {
        return Err(ApiError::Conflict {
            code: "too_much",
            message: "You have written as much as one person may in this version's discussion. Delete some of your comments to make room.".into(),
        });
    }
    if weight(weights.total).saturating_add(bytes) > MAX_DISCUSSION_BYTES {
        return Err(ApiError::Conflict {
            code: "discussion_full",
            message: "This version's discussion is as long as it may be. Delete threads that are done to make room.".into(),
        });
    }
    Ok(())
}

/// The mentions that are members of the organization, each once, 20 at most
/// (a version's comments).
pub(crate) async fn members_only(
    db: &PgPool,
    org: Uuid,
    mentions: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    let members = members_among(db, org, mentions).await?;
    let mut out = Vec::new();
    for id in mentions {
        if members.contains(id) && !out.contains(id) && out.len() < MAX_MENTIONS {
            out.push(*id);
        }
    }
    Ok(out)
}

/// Which of `people` are members of the organization (one query; a draft's
/// operations ask for a whole frame's at once: `collab::socket`).
pub(crate) async fn members_among(
    db: &PgPool,
    org: Uuid,
    people: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    if people.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar!(
        "SELECT user_id FROM memberships WHERE org_id = $1 AND user_id = ANY($2)",
        org,
        people,
    )
    .fetch_all(db)
    .await
}

/// The version's threads, oldest first, with their comments and the
/// caller's read marks.
async fn threads(db: &PgPool, version: Uuid, user: Uuid) -> ApiResult<Vec<VersionThread>> {
    let rows = sqlx::query!(
        r#"SELECT t.id, t.anchor, t.frozen, t.created_by, t.created_at,
                  t.resolved_at, t.resolved_by, t.resolved_by_name, r.read_at AS "read_at?"
           FROM version_threads t
           LEFT JOIN version_thread_reads r ON r.thread_id = t.id AND r.user_id = $2
           WHERE t.version_id = $1
           ORDER BY t.created_at, t.id"#,
        version,
        user,
    )
    .fetch_all(db)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let comments = sqlx::query!(
        "SELECT id, thread_id, author_id, author_name, body, mentions, created_at, edited_at
         FROM version_comments WHERE thread_id = ANY($1)
         ORDER BY created_at, id",
        &ids,
    )
    .fetch_all(db)
    .await?;
    let mut by_thread: HashMap<Uuid, Vec<VersionComment>> = HashMap::new();
    for c in comments {
        by_thread
            .entry(c.thread_id)
            .or_default()
            .push(VersionComment {
                id: c.id,
                author: c.author_id,
                author_name: c.author_name,
                body: c.body,
                mentions: c.mentions,
                created_at: c.created_at,
                edited_at: c.edited_at,
            });
    }
    rows.into_iter()
        .map(|r| {
            Ok(VersionThread {
                id: r.id,
                anchor: serde_json::from_value(r.anchor).map_err(ApiError::internal)?,
                frozen: r.frozen,
                created_by: r.created_by,
                created_at: r.created_at,
                resolved: match (r.resolved_at, r.resolved_by_name) {
                    (Some(at), Some(name)) => Some(ThreadResolution {
                        by: r.resolved_by,
                        name,
                        at,
                    }),
                    _ => None,
                },
                comments: by_thread.remove(&r.id).unwrap_or_default(),
                read_at: r.read_at,
            })
        })
        .collect()
}

async fn one(db: &PgPool, version: Uuid, user: Uuid, thread: Uuid) -> ApiResult<VersionThread> {
    threads(db, version, user)
        .await?
        .into_iter()
        .find(|t| t.id == thread)
        .ok_or_else(|| ApiError::not_found("No such thread. It may have been deleted."))
}

/// The version's discussion: its draft's review, frozen, then the threads
/// started on its page; oldest first.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/versions/{number}/threads",
    operation_id = "list_version_threads",
    tag = "versions",
    params(("org" = String, Path), ("db" = String, Path), ("number" = i32, Path)),
    responses(
        (status = 200, body = Vec<VersionThread>),
        (status = 404, body = ErrorBody),
    )
)]
async fn list(
    member: Member,
    State(db): State<PgPool>,
    Path((_, slug, number)): Path<(String, String, i32)>,
) -> ApiResult<Json<Vec<VersionThread>>> {
    let version = version_id(&db, &member, &slug, number).await?;
    Ok(Json(threads(&db, version, member.user.id).await?))
}

/// Starts a thread on the version, pinned to a table, a column, a point of
/// the graph or a statement.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/versions/{number}/threads",
    operation_id = "create_version_thread",
    tag = "versions",
    params(("org" = String, Path), ("db" = String, Path), ("number" = i32, Path)),
    request_body = NewThread,
    responses(
        (status = 201, body = VersionThread),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`too_many_threads`, `too_much`, `discussion_full`", body = ErrorBody),
        (status = 422, body = ErrorBody),
        (status = 429, description = "`rate_limited`", body = ErrorBody),
    )
)]
async fn create(
    member: Member,
    State(db): State<PgPool>,
    State(limits): State<Arc<Limits>>,
    Path((_, slug, number)): Path<(String, String, i32)>,
    Json(body): Json<NewThread>,
) -> ApiResult<(StatusCode, Json<VersionThread>)> {
    limits.version_comments.check(member.user.id)?;
    let anchor = body.anchor.normalized().ok_or_else(|| {
        invalid(
            "anchor",
            "Pin the comment to a table, a column or the canvas.",
        )
    })?;
    let text = checked_body(&body.body)?;
    let version = version_id(&db, &member, &slug, number).await?;
    let mentions = members_only(&db, member.org.id, &body.mentions).await?;
    let anchor = serde_json::to_value(&anchor).map_err(ApiError::internal)?;
    let mut tx = db.begin().await?;
    // One at a time per version: the counts below hold.
    room_for(&mut tx, version, member.user.id, text.len()).await?;
    let count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM version_threads WHERE version_id = $1 AND NOT frozen"#,
        version,
    )
    .fetch_one(&mut *tx)
    .await?;
    if count >= MAX_THREADS {
        return Err(ApiError::Conflict {
            code: "too_many_threads",
            message:
                "This version has as many threads as it can take. Resolve or delete some first."
                    .into(),
        });
    }
    let thread = sqlx::query_scalar!(
        "INSERT INTO version_threads (version_id, frozen, anchor, created_by)
         VALUES ($1, false, $2, $3) RETURNING id",
        version,
        anchor,
        member.user.id,
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO version_comments (thread_id, author_id, author_name, body, mentions)
         VALUES ($1, $2, $3, $4, $5)",
        thread,
        member.user.id,
        member.user.name,
        text,
        &mentions,
    )
    .execute(&mut *tx)
    .await?;
    mark_read(&mut tx, thread, member.user.id, Utc::now()).await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(one(&db, version, member.user.id, thread).await?),
    ))
}

/// Replies in a thread.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/versions/{number}/threads/{thread}/comments",
    operation_id = "reply_version_thread",
    tag = "versions",
    params(("org" = String, Path), ("db" = String, Path), ("number" = i32, Path), ("thread" = Uuid, Path)),
    request_body = CommentText,
    responses(
        (status = 201, body = VersionThread),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`thread_frozen`, `too_many_comments`, `too_much`, `discussion_full`", body = ErrorBody),
        (status = 422, body = ErrorBody),
        (status = 429, description = "`rate_limited`", body = ErrorBody),
    )
)]
async fn reply(
    member: Member,
    State(db): State<PgPool>,
    State(limits): State<Arc<Limits>>,
    Path((_, slug, number, thread)): Path<(String, String, i32, Uuid)>,
    Json(body): Json<CommentText>,
) -> ApiResult<(StatusCode, Json<VersionThread>)> {
    limits.version_comments.check(member.user.id)?;
    let text = checked_body(&body.body)?;
    let version = version_id(&db, &member, &slug, number).await?;
    if thread_of(&db, version, thread).await? {
        return Err(frozen());
    }
    let mentions = members_only(&db, member.org.id, &body.mentions).await?;
    let mut tx = db.begin().await?;
    room_for(&mut tx, version, member.user.id, text.len()).await?;
    let locked = sqlx::query_scalar!(
        "SELECT id FROM version_threads WHERE id = $1 FOR UPDATE",
        thread
    )
    .fetch_optional(&mut *tx)
    .await?;
    if locked.is_none() {
        return Err(ApiError::not_found(
            "No such thread. It may have been deleted.",
        ));
    }
    let count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM version_comments WHERE thread_id = $1"#,
        thread,
    )
    .fetch_one(&mut *tx)
    .await?;
    if count >= MAX_COMMENTS_PER_THREAD {
        return Err(ApiError::Conflict {
            code: "too_many_comments",
            message: "This thread has as many comments as it can take. Start a new one.".into(),
        });
    }
    sqlx::query!(
        "INSERT INTO version_comments (thread_id, author_id, author_name, body, mentions)
         VALUES ($1, $2, $3, $4, $5)",
        thread,
        member.user.id,
        member.user.name,
        text,
        &mentions,
    )
    .execute(&mut *tx)
    .await?;
    mark_read(&mut tx, thread, member.user.id, Utc::now()).await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(one(&db, version, member.user.id, thread).await?),
    ))
}

/// A comment of a thread of the version that is not frozen (404, 409
/// `thread_frozen`).
struct Written {
    /// `None` once their account is gone.
    author: Option<Uuid>,
    author_name: String,
    body: String,
}

async fn comment_in(db: &PgPool, version: Uuid, thread: Uuid, comment: Uuid) -> ApiResult<Written> {
    if thread_of(db, version, thread).await? {
        return Err(frozen());
    }
    sqlx::query_as!(
        Written,
        r#"SELECT author_id AS author, author_name, body
           FROM version_comments WHERE id = $1 AND thread_id = $2"#,
        comment,
        thread,
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| ApiError::not_found("No such comment. It may have been deleted."))
}

fn not_yours(message: &str) -> ApiError {
    ApiError::Denied {
        code: "not_yours",
        message: message.into(),
    }
}

/// Edits one of the caller's comments.
#[utoipa::path(
    patch,
    path = "/orgs/{org}/databases/{db}/versions/{number}/threads/{thread}/comments/{comment}",
    operation_id = "edit_version_comment",
    tag = "versions",
    params(("org" = String, Path), ("db" = String, Path), ("number" = i32, Path), ("thread" = Uuid, Path), ("comment" = Uuid, Path)),
    request_body = CommentText,
    responses(
        (status = 200, body = VersionThread),
        (status = 403, description = "`not_yours`", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`thread_frozen`, `too_much`, `discussion_full`", body = ErrorBody),
        (status = 422, body = ErrorBody),
        (status = 429, description = "`rate_limited`", body = ErrorBody),
    )
)]
async fn edit(
    member: Member,
    State(db): State<PgPool>,
    State(limits): State<Arc<Limits>>,
    Path((_, slug, number, thread, comment)): Path<(String, String, i32, Uuid, Uuid)>,
    Json(body): Json<CommentText>,
) -> ApiResult<Json<VersionThread>> {
    limits.version_comments.check(member.user.id)?;
    let text = checked_body(&body.body)?;
    let version = version_id(&db, &member, &slug, number).await?;
    let written = comment_in(&db, version, thread, comment).await?;
    if written.author != Some(member.user.id) {
        return Err(not_yours("Only its author can change a comment."));
    }
    let mentions = members_only(&db, member.org.id, &body.mentions).await?;
    let mut tx = db.begin().await?;
    room_for(
        &mut tx,
        version,
        member.user.id,
        text.len().saturating_sub(written.body.len()),
    )
    .await?;
    sqlx::query!(
        "UPDATE version_comments SET body = $2, mentions = $3, edited_at = now()
         WHERE id = $1 AND (body, mentions) IS DISTINCT FROM ($2, $3)",
        comment,
        text,
        &mentions,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(one(&db, version, member.user.id, thread).await?))
}

/// Deletes a comment: the caller's, or anyone's for developers and admins
/// (recorded in the audit log); the thread goes with its last one.
#[utoipa::path(
    delete,
    path = "/orgs/{org}/databases/{db}/versions/{number}/threads/{thread}/comments/{comment}",
    operation_id = "delete_version_comment",
    tag = "versions",
    params(("org" = String, Path), ("db" = String, Path), ("number" = i32, Path), ("thread" = Uuid, Path), ("comment" = Uuid, Path)),
    responses(
        (status = 204, description = "Deleted (with the thread, when it was its last comment)"),
        (status = 403, description = "`not_yours`", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`thread_frozen`", body = ErrorBody),
        (status = 429, description = "`rate_limited`", body = ErrorBody),
    )
)]
async fn delete(
    member: Member,
    State(db): State<PgPool>,
    State(limits): State<Arc<Limits>>,
    Path((_, slug, number, thread, comment)): Path<(String, String, i32, Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    limits.version_comments.check(member.user.id)?;
    let version = version_id(&db, &member, &slug, number).await?;
    let written = comment_in(&db, version, thread, comment).await?;
    let theirs = written.author != Some(member.user.id);
    if theirs && !member.role.can_write() {
        return Err(not_yours(
            "Only its author, a developer or an admin can delete a comment.",
        ));
    }
    let mut tx = db.begin().await?;
    if theirs {
        let database = databases::find(&db, member.org.id, &slug).await?;
        audit::record(
            &mut *tx,
            &audit::Entry {
                org_id: member.org.id,
                database_id: Some(database.id),
                action: AuditAction::CommentDeleted,
                actor_id: Some(member.user.id),
                actor_name: Some(member.user.name.clone()),
                version_number: Some(number),
                reason: None,
                details: audit::AuditDetails {
                    thread_id: Some(thread),
                    comments: Some(1),
                    authors: Some(vec![written.author_name]),
                    excerpt: Some(excerpt(&written.body)),
                    ..audit::AuditDetails::default()
                },
            },
        )
        .await?;
    }
    sqlx::query_scalar!(
        "SELECT id FROM version_threads WHERE id = $1 FOR UPDATE",
        thread
    )
    .fetch_optional(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM version_comments WHERE id = $1", comment)
        .execute(&mut *tx)
        .await?;
    sqlx::query!(
        "DELETE FROM version_threads t WHERE id = $1 AND NOT frozen
         AND NOT EXISTS (SELECT 1 FROM version_comments c WHERE c.thread_id = t.id)",
        thread,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Resolves a thread (anyone in the organization).
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/versions/{number}/threads/{thread}/resolve",
    operation_id = "resolve_version_thread",
    tag = "versions",
    params(("org" = String, Path), ("db" = String, Path), ("number" = i32, Path), ("thread" = Uuid, Path)),
    responses(
        (status = 200, body = VersionThread),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`thread_frozen`", body = ErrorBody),
        (status = 429, description = "`rate_limited`", body = ErrorBody),
    )
)]
async fn resolve(
    member: Member,
    State(db): State<PgPool>,
    State(limits): State<Arc<Limits>>,
    Path((_, slug, number, thread)): Path<(String, String, i32, Uuid)>,
) -> ApiResult<Json<VersionThread>> {
    limits.version_comments.check(member.user.id)?;
    let version = version_id(&db, &member, &slug, number).await?;
    if thread_of(&db, version, thread).await? {
        return Err(frozen());
    }
    sqlx::query!(
        "UPDATE version_threads SET resolved_at = now(), resolved_by = $2, resolved_by_name = $3
         WHERE id = $1 AND resolved_at IS NULL",
        thread,
        member.user.id,
        member.user.name,
    )
    .execute(&db)
    .await?;
    Ok(Json(one(&db, version, member.user.id, thread).await?))
}

/// Reopens a resolved thread (anyone in the organization).
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/versions/{number}/threads/{thread}/reopen",
    operation_id = "reopen_version_thread",
    tag = "versions",
    params(("org" = String, Path), ("db" = String, Path), ("number" = i32, Path), ("thread" = Uuid, Path)),
    responses(
        (status = 200, body = VersionThread),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`thread_frozen`", body = ErrorBody),
        (status = 429, description = "`rate_limited`", body = ErrorBody),
    )
)]
async fn reopen(
    member: Member,
    State(db): State<PgPool>,
    State(limits): State<Arc<Limits>>,
    Path((_, slug, number, thread)): Path<(String, String, i32, Uuid)>,
) -> ApiResult<Json<VersionThread>> {
    limits.version_comments.check(member.user.id)?;
    let version = version_id(&db, &member, &slug, number).await?;
    if thread_of(&db, version, thread).await? {
        return Err(frozen());
    }
    sqlx::query!(
        "UPDATE version_threads SET resolved_at = NULL, resolved_by = NULL, resolved_by_name = NULL
         WHERE id = $1",
        thread,
    )
    .execute(&db)
    .await?;
    Ok(Json(one(&db, version, member.user.id, thread).await?))
}

/// Marks a thread read up to `at` (never back: an older `at` changes nothing).
#[utoipa::path(
    put,
    path = "/orgs/{org}/databases/{db}/versions/{number}/threads/{thread}/read",
    operation_id = "read_version_thread",
    tag = "versions",
    params(("org" = String, Path), ("db" = String, Path), ("number" = i32, Path), ("thread" = Uuid, Path)),
    request_body = ReadUpTo,
    responses(
        (status = 204, description = "Marked"),
        (status = 404, body = ErrorBody),
    )
)]
async fn read(
    member: Member,
    State(db): State<PgPool>,
    Path((_, slug, number, thread)): Path<(String, String, i32, Uuid)>,
    Json(body): Json<ReadUpTo>,
) -> ApiResult<StatusCode> {
    let version = version_id(&db, &member, &slug, number).await?;
    thread_of(&db, version, thread).await?;
    let mut tx = db.begin().await?;
    mark_read(&mut tx, thread, member.user.id, body.at.min(Utc::now())).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mark_read(
    tx: &mut Transaction<'_, Postgres>,
    thread: Uuid,
    user: Uuid,
    at: DateTime<Utc>,
) -> ApiResult<()> {
    sqlx::query!(
        "INSERT INTO version_thread_reads (thread_id, user_id, read_at) VALUES ($1, $2, $3)
         ON CONFLICT (thread_id, user_id) DO UPDATE
         SET read_at = greatest(version_thread_reads.read_at, EXCLUDED.read_at)",
        thread,
        user,
        at,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Keeps a draft's threads with the version it became, frozen, in the
/// transaction publishing it (with where everyone had read them up to):
/// written, then frozen, so a frozen thread's comments are never added to.
/// Entries a check let through but the tables refuse (a body over the
/// limit) are left out rather than fail the publish.
pub async fn freeze(
    tx: &mut Transaction<'_, Postgres>,
    version: Uuid,
    draft: Uuid,
    discussion: &[DocThread],
) -> ApiResult<()> {
    if discussion.is_empty() {
        return Ok(());
    }
    let mut ids = Vec::new();
    let mut anchors = Vec::new();
    let mut created_by = Vec::new();
    let mut created_at = Vec::new();
    let mut resolved_at = Vec::new();
    let mut resolved_by = Vec::new();
    let mut resolved_name = Vec::new();
    let mut c_thread = Vec::new();
    let mut c_author = Vec::new();
    let mut c_name = Vec::new();
    let mut c_body = Vec::new();
    let mut c_mentions = Vec::new();
    let mut c_at = Vec::new();
    let mut c_edited = Vec::new();
    for t in discussion {
        let Some(anchor) = t.thread.anchor.normalized() else {
            continue;
        };
        ids.push(t.id);
        anchors.push(serde_json::to_value(&anchor).map_err(ApiError::internal)?);
        created_by.push(t.thread.by);
        created_at.push(t.thread.at);
        resolved_at.push(t.thread.resolved.as_ref().map(|r| r.at));
        resolved_by.push(t.thread.resolved.as_ref().map(|r| r.by));
        resolved_name.push(t.thread.resolved.as_ref().map(|r| r.name.clone()));
        for (_, c) in &t.comments {
            if c.body.trim().is_empty() || c.body.chars().count() > MAX_BODY_CHARS {
                continue;
            }
            c_thread.push(t.id);
            c_author.push(c.author.id);
            c_name.push(c.author.name.clone());
            c_body.push(c.body.clone());
            let mentions: Vec<Uuid> = c.mentions.iter().copied().take(MAX_MENTIONS).collect();
            c_mentions.push(serde_json::to_value(mentions).map_err(ApiError::internal)?);
            c_at.push(c.at);
            c_edited.push(c.edited);
        }
    }
    sqlx::query!(
        r#"INSERT INTO version_threads (version_id, draft_id, draft_thread, frozen, anchor, created_by,
                                        created_at, resolved_at, resolved_by, resolved_by_name)
           SELECT $1, $2, t.draft_thread, false, t.anchor, u.id, t.created_at,
                  t.resolved_at, r.id, CASE WHEN t.resolved_at IS NULL THEN NULL ELSE t.resolved_name END
           FROM unnest($3::uuid[], $4::jsonb[], $5::uuid[], $6::timestamptz[], $7::timestamptz[],
                       $8::uuid[], $9::text[])
                AS t(draft_thread, anchor, created_by, created_at, resolved_at, resolved_by, resolved_name)
           LEFT JOIN users u ON u.id = t.created_by
           LEFT JOIN users r ON r.id = t.resolved_by"#,
        version,
        draft,
        &ids,
        &anchors,
        &created_by,
        &created_at,
        &resolved_at as &[Option<DateTime<Utc>>],
        &resolved_by as &[Option<Uuid>],
        &resolved_name as &[Option<String>],
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        r#"INSERT INTO version_comments (thread_id, author_id, author_name, body, mentions, created_at, edited_at)
           SELECT t.id, u.id, c.author_name, c.body,
                  ARRAY(SELECT jsonb_array_elements_text(c.mentions)::uuid), c.created_at, c.edited_at
           FROM unnest($2::uuid[], $3::uuid[], $4::text[], $5::text[], $6::jsonb[], $7::timestamptz[],
                       $8::timestamptz[])
                AS c(draft_thread, author_id, author_name, body, mentions, created_at, edited_at)
           JOIN version_threads t ON t.version_id = $1 AND t.draft_thread = c.draft_thread
           LEFT JOIN users u ON u.id = c.author_id"#,
        version,
        &c_thread,
        &c_author,
        &c_name,
        &c_body,
        &c_mentions,
        &c_at,
        &c_edited as &[Option<DateTime<Utc>>],
    )
    .execute(&mut **tx)
    .await?;
    keep_reads_and_freeze(tx, version, draft).await
}

/// [`freeze`]'s end: everyone's read marks carried over, then the threads
/// frozen.
async fn keep_reads_and_freeze(
    tx: &mut Transaction<'_, Postgres>,
    version: Uuid,
    draft: Uuid,
) -> ApiResult<()> {
    sqlx::query!(
        "INSERT INTO version_thread_reads (thread_id, user_id, read_at)
         SELECT t.id, r.user_id, r.read_at
         FROM draft_thread_reads r
         JOIN version_threads t ON t.version_id = $1 AND t.draft_thread = r.thread
         WHERE r.draft_id = $2",
        version,
        draft,
    )
    .execute(&mut **tx)
    .await?;
    // Frozen last: from now on, nothing about them changes (a trigger).
    sqlx::query!(
        "UPDATE version_threads SET frozen = true WHERE version_id = $1 AND draft_thread IS NOT NULL",
        version,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}
