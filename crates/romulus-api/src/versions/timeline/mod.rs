//! The time machine's data: `GET /orgs/{org}/databases/{db}/timeline`
//! [member; `?removed=true` admins]. Every version of the database, oldest
//! first, and the history of every object any of them had ([`history`]):
//! when each table, column, constraint, index, policy, type, partition and
//! extension appeared, changed (and how) and went, with its value in each
//! state, so the web app can draw any version, or the union of a range of
//! them, without fetching a model. Plus where the tables sit: the newest
//! version's layout, older ones' positions for tables it does not have.
//!
//! Removed versions are left out, as everywhere; an admin can ask for them
//! (`removed=true`): they are then in the timeline, flagged, and the
//! history runs through them.
//!
//! Computed from romulus-diff between consecutive versions. A diff of two
//! versions never changes, so each step is cached by the ids of its two
//! versions ([`cache`]): a version created, removed or restored makes new
//! pairs, whose steps are computed (two models loaded and compared), and
//! the rest of the history comes from the cache. The models are streamed
//! from the database to a blocking task that diffs them, two at a time in
//! memory. The history built from the steps is cached too, by the database
//! and its list of versions; a build takes its organization's turn first.

pub mod cache;
pub mod history;

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use axum::{
    body::Bytes,
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use futures_util::TryStreamExt;
use remus_core::Schema;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use self::{
    cache::{Pair, history_key},
    history::{History, Step},
};
use super::{SummaryRow, VersionSummary, empty, layout::VersionLayout};
use crate::{
    ApiError, ApiResult, AppState,
    auth::Member,
    databases,
    domain::{Role, SourceKind, VersionOrigin},
    error::ErrorBody,
    extract::{Path, Query},
};

/// At most this many versions, the newest: older ones are counted, not
/// drawn.
pub const MAX_VERSIONS: i64 = 1000;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(timeline))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TimelineQuery {
    /// Include the versions removed and not purged yet, flagged (admins).
    #[serde(default)]
    pub removed: bool,
}

/// A database's history, object by object.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Timeline {
    /// Oldest first: the states' `from` and `to` are indexes in this list.
    pub versions: Vec<TimelineVersion>,
    /// Versions older than the first one listed, left out (the timeline
    /// holds the newest 1000).
    pub earlier: i64,
    pub history: History,
    /// The newest version's positions and groups, then older versions'
    /// positions for tables it does not have (each table where it last
    /// was), and their groups for tables only older versions had.
    pub layout: VersionLayout,
}

/// A version in the timeline.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TimelineVersion {
    #[serde(flatten)]
    pub summary: VersionSummary,
    /// Removed (only with `removed=true`): when, and why.
    #[schema(required = true)]
    pub removed_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    pub removal_reason: Option<String>,
}

/// The history of the database's objects, version by version.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/timeline",
    operation_id = "get_timeline",
    tag = "versions",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        TimelineQuery,
    ),
    responses(
        (status = 200, body = Timeline),
        (status = 400, description = "`bad_request`: `removed` is not a boolean", body = ErrorBody),
        (status = 403, description = "`forbidden`: removed versions are for admins", body = ErrorBody),
        (status = 404, description = "`not_found`: no such database", body = ErrorBody),
        (status = 503, description = "`timeout`: a very long history took too long; try again (what was computed is kept)", body = ErrorBody),
    )
)]
async fn timeline(
    member: Member,
    State(state): State<AppState>,
    Path((_, slug)): Path<(String, String)>,
    Query(query): Query<TimelineQuery>,
) -> ApiResult<Response> {
    let database = databases::find(&state.db, member.org.id, &slug).await?;
    if query.removed && !member.role.at_least(Role::Admin) {
        return Err(ApiError::Forbidden(
            "Only admins see removed versions.".into(),
        ));
    }
    // Versions past the plan's history window are not part of it.
    let cutoff = state
        .billing
        .history(&state.db, member.org.id)
        .await?
        .cutoff;
    let rows = versions(&state.db, database.id, query.removed, cutoff).await?;
    let earlier = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM versions v
           WHERE v.database_id = $1 AND (v.removed_at IS NULL OR $2)
             AND ($3::timestamptz IS NULL OR v.created_at >= $3 OR version_kept_visible(v.id, v.database_id))"#,
        database.id,
        query.removed,
        cutoff,
    )
    .fetch_one(&state.db)
    .await?
        - i64::try_from(rows.len()).unwrap_or(i64::MAX);

    // The same versions are the same history: built once, whoever asks.
    let ids: Vec<Uuid> = rows.iter().map(|r| r.summary.id).collect();
    let key = history_key(database.id, &ids);
    let content = if let Some(content) = state.timelines.history(&key) {
        content
    } else {
        let _turn = state.timelines.build_turn(member.org.id).await;
        // Built meanwhile by whoever had the turn before.
        if let Some(content) = state.timelines.history(&key) {
            content
        } else {
            let content = build(&state, database.id, &rows).await?;
            state.timelines.insert_history(key, content.clone());
            content
        }
    };

    let head = Head {
        versions: rows
            .into_iter()
            .map(|r| TimelineVersion {
                summary: r.summary,
                removed_at: r.removed_at,
                removal_reason: r.removal_reason,
            })
            .collect(),
        earlier: earlier.max(0),
    };
    let body = joined(&head, &content)?;
    Ok(([(header::CONTENT_TYPE, "application/json")], body).into_response())
}

/// A timeline's versions: joined to its cached content per request.
#[derive(Serialize)]
struct Head {
    versions: Vec<TimelineVersion>,
    earlier: i64,
}

/// What a timeline holds besides its versions (whose authors can be
/// renamed): what is cached.
#[derive(Serialize)]
struct Content {
    history: History,
    layout: VersionLayout,
}

/// The history over `rows` (oldest first), serialised: its steps from the
/// cache or computed, folded, and the union of the versions' layouts.
async fn build(state: &AppState, database: Uuid, rows: &[VersionRow]) -> ApiResult<Bytes> {
    // Each version's step: from the one before it (the first from nothing).
    let pairs: Vec<Pair> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| (i.checked_sub(1).map(|p| rows[p].summary.id), row.summary.id))
        .collect();
    let mut steps = state.timelines.steps(&pairs);
    let missing: Vec<Pair> = pairs
        .iter()
        .filter(|p| !steps.contains_key(p))
        .copied()
        .collect();
    if !missing.is_empty() {
        steps.extend(compute(state, database, &missing).await?);
    }
    let ordered: Vec<Arc<Step>> = pairs
        .iter()
        .map(|p| {
            steps.get(p).cloned().ok_or_else(|| {
                ApiError::internal(std::io::Error::other("a step of the timeline is missing"))
            })
        })
        .collect::<ApiResult<_>>()?;
    let layouts: Vec<VersionLayout> = rows.iter().rev().map(|r| r.layout.clone()).collect();
    tokio::task::spawn_blocking(move || {
        let history = history::fold(&ordered.iter().map(AsRef::as_ref).collect::<Vec<_>>());
        let alive: HashSet<String> = history
            .tables
            .iter()
            .filter(|t| t.states.last().is_some_and(|s| s.to.is_none()))
            .map(|t| t.id.clone())
            .collect();
        let layout = union_of(layouts, &alive);
        serde_json::to_vec(&Content { history, layout })
            .map(Bytes::from)
            .map_err(ApiError::internal)
    })
    .await
    .map_err(ApiError::internal)?
}

/// `head` and `content`, two JSON objects, as one, without parsing the
/// cached one again.
fn joined(head: &impl Serialize, content: &[u8]) -> ApiResult<Bytes> {
    let head = serde_json::to_vec(head).map_err(ApiError::internal)?;
    let (Some((b'}', head)), Some((b'{', rest))) = (head.split_last(), content.split_first())
    else {
        return Err(ApiError::internal(std::io::Error::other(
            "a timeline's parts are not JSON objects",
        )));
    };
    let mut body = Vec::with_capacity(head.len() + rest.len() + 1);
    body.extend_from_slice(head);
    if rest != b"}" {
        body.push(b',');
    }
    body.extend_from_slice(rest);
    Ok(Bytes::from(body))
}

struct VersionRow {
    summary: VersionSummary,
    layout: VersionLayout,
    removed_at: Option<DateTime<Utc>>,
    removal_reason: Option<String>,
}

/// The newest [`MAX_VERSIONS`] versions, oldest first, removed ones only
/// when asked.
async fn versions(
    db: &sqlx::PgPool,
    database_id: Uuid,
    removed: bool,
    cutoff: Option<DateTime<Utc>>,
) -> ApiResult<Vec<VersionRow>> {
    let rows = sqlx::query!(
        r#"SELECT v.id, v.number, v.origin AS "origin: VersionOrigin",
                  v.source_kind AS "source_kind: SourceKind", v.message, v.created_at,
                  u.id AS "user_id?", u.name AS "user_name?", t.id AS "token_id?", t.name AS "token_name?",
                  v.migration_sql IS NOT NULL AS "written_migration!",
                  v.server_version, v.pg_major, v.database_major,
                  v.layout, v.removed_at, v.removal_reason
           FROM versions v
           LEFT JOIN users u ON u.id = v.created_by
           LEFT JOIN api_tokens t ON t.id = v.created_by_token
           WHERE v.database_id = $1 AND (v.removed_at IS NULL OR $2)
             AND ($4::timestamptz IS NULL OR v.created_at >= $4 OR version_kept_visible(v.id, v.database_id))
           ORDER BY v.number DESC LIMIT $3"#,
        database_id,
        removed,
        MAX_VERSIONS,
        cutoff,
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .rev()
        .map(|r| VersionRow {
            layout: VersionLayout::from_value(&r.layout),
            removed_at: r.removed_at,
            removal_reason: r.removal_reason,
            summary: SummaryRow {
                id: r.id,
                number: r.number,
                origin: r.origin,
                source_kind: r.source_kind,
                message: r.message,
                created_at: r.created_at,
                user_id: r.user_id,
                user_name: r.user_name,
                token_id: r.token_id,
                token_name: r.token_name,
                written_migration: r.written_migration,
                server_version: r.server_version,
                pg_major: r.pg_major,
                database_major: r.database_major,
            }
            .into(),
        })
        .collect())
}

/// The steps of `pairs`, computed: their versions' models streamed in order
/// to a blocking task that diffs each with the one before. Each step goes
/// into the cache as soon as it is computed, so a request that times out
/// leaves its work for the next one.
async fn compute(
    state: &AppState,
    database: Uuid,
    pairs: &[Pair],
) -> ApiResult<HashMap<Pair, Arc<Step>>> {
    let mut wanted: Vec<Uuid> = Vec::new();
    for (from, to) in pairs {
        wanted.extend(from.iter().copied());
        wanted.push(*to);
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Uuid, String)>(2);
    let cache = state.timelines.clone();
    let pairs = pairs.to_vec();
    let worker = tokio::task::spawn_blocking(move || -> ApiResult<HashMap<Pair, Arc<Step>>> {
        let mut out = HashMap::new();
        let mut previous: Option<(Uuid, Schema)> = None;
        while let Some((id, text)) = rx.blocking_recv() {
            let model = Schema::from_json(&text).map_err(ApiError::internal)?;
            drop(text);
            for pair in pairs.iter().filter(|p| p.1 == id) {
                let nothing;
                let old = match pair.0 {
                    None => {
                        nothing = empty(&model);
                        &nothing
                    }
                    Some(from) => match &previous {
                        Some((previous_id, previous)) if *previous_id == from => previous,
                        _ => {
                            return Err(ApiError::internal(std::io::Error::other(
                                "a timeline's models came out of order",
                            )));
                        }
                    },
                };
                let step = Arc::new(history::step(old, &model));
                cache.insert_step(database, *pair, step.clone());
                out.insert(*pair, step);
            }
            previous = Some((id, model));
        }
        Ok(out)
    });
    let mut models = sqlx::query!(
        r#"SELECT id, model::text AS "model!" FROM versions
           WHERE id = ANY($1) ORDER BY number"#,
        &wanted,
    )
    .fetch(&state.db);
    while let Some(row) = models.try_next().await? {
        if tx.send((row.id, row.model)).await.is_err() {
            // The worker stopped: its error says why.
            break;
        }
    }
    drop(tx);
    worker.await.map_err(ApiError::internal)?
}

/// The layout of every table the history has: `layouts` newest first,
/// each table where the newest version placing it put it. Groups are the
/// newest version's, plus, for tables no longer there (not in `alive`),
/// the group an older version had them in: the newest one's group of the
/// same id, or that old group (holding only them) when the newest dropped
/// it.
fn union_of(layouts: Vec<VersionLayout>, alive: &HashSet<String>) -> VersionLayout {
    let mut layouts = layouts.into_iter();
    let mut out = layouts.next().unwrap_or_default();
    let mut grouped: HashSet<String> = out
        .groups
        .values()
        .flat_map(|g| g.members.iter().cloned())
        .collect();
    for older in layouts {
        for (name, position) in older.layout {
            out.layout.entry(name).or_insert(position);
        }
        for (id, mut group) in older.groups {
            group
                .members
                .retain(|name| !alive.contains(name) && !grouped.contains(name));
            if group.members.is_empty() {
                continue;
            }
            grouped.extend(group.members.iter().cloned());
            match out.groups.get_mut(&id) {
                Some(current) => current.members.extend(group.members),
                None => {
                    out.groups.insert(id, group);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn layout(value: &serde_json::Value) -> VersionLayout {
        VersionLayout::from_value(value)
    }

    #[test]
    fn the_layout_is_the_newest_one_with_older_positions_and_groups_for_the_rest() {
        let newest = layout(&json!({
            "layout": { "public.users": { "x": 10, "y": 10 } },
            "groups": { "g1": { "name": "Core", "color": "blue", "members": ["public.users"] } }
        }));
        let middle = layout(&json!({
            "layout": {
                "public.users": { "x": 500, "y": 500 },
                "public.orders": { "x": 300, "y": 0 },
                "public.audit": { "x": 0, "y": 900 }
            },
            "groups": {
                "g1": { "name": "Old", "color": "red", "members": ["public.users", "public.orders"] },
                "g2": { "name": "Logs", "color": "stone", "members": ["public.audit"] }
            }
        }));
        let oldest = layout(&json!({
            "layout": { "public.orders": { "x": 1, "y": 1 }, "public.legacy": { "x": 0, "y": 300 } },
            "groups": { "g3": { "name": "Legacy", "color": "teal",
                                "members": ["public.legacy", "public.orders"] } }
        }));
        // Only `users` is still there; `audit` is too but was ungrouped since.
        let alive: HashSet<String> = ["public.users", "public.audit"]
            .into_iter()
            .map(String::from)
            .collect();
        let union = union_of(vec![newest, middle, oldest], &alive);
        assert_eq!(
            serde_json::to_value(&union).unwrap(),
            json!({
                "layout": {
                    "public.audit": { "x": 0.0, "y": 900.0 },
                    "public.legacy": { "x": 0.0, "y": 300.0 },
                    "public.orders": { "x": 300.0, "y": 0.0 },
                    "public.users": { "x": 10.0, "y": 10.0 }
                },
                "groups": {
                    "g1": { "name": "Core", "color": "blue",
                            "members": ["public.users", "public.orders"] },
                    "g3": { "name": "Legacy", "color": "teal", "members": ["public.legacy"] }
                }
            })
        );
    }
}
