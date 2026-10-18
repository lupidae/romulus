//! A playground's shared history: who ran what, when, how long, rows or
//! error; never results (docs/playground.md, "History"). Console runs are
//! recorded as their `finished` event passes the API ([`super::console`]),
//! the newest [`CONSOLE_RUNS_KEPT`] kept; resets and migration runs (rate
//! limited, and what a playground holds) by their endpoints
//! ([`super::migration`]), all kept.

use axum::extract::State;
use chrono::{DateTime, Utc};
use romulus_playground::protocol::MigrationReport;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgExecutor, PgPool};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::find;
use crate::{
    ApiResult, AppState,
    auth::Member,
    databases,
    domain::{PlaygroundRunKind, PlaygroundRunStatus},
    error::ErrorBody,
    extract::{Json, Path, Query},
};

/// Stored SQL is cut here.
const SQL_BYTES: usize = 64 * 1024;
/// Console runs (queries, explains) kept per playground; older ones go.
pub const CONSOLE_RUNS_KEPT: i64 = 500;

/// One entry of the history.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaygroundRun {
    pub id: Uuid,
    pub kind: PlaygroundRunKind,
    /// Who: their name as it was (`id` is `null` once their account is gone).
    pub by: RunBy,
    /// What ran (its first 64 KiB); `null` for a reset.
    pub sql: Option<String>,
    pub started_at: DateTime<Utc>,
    pub duration_ms: i64,
    pub status: PlaygroundRunStatus,
    /// Rows the last statement returned or changed.
    pub rows: Option<i64>,
    /// What went wrong, in one sentence.
    pub error: Option<String>,
    /// A migration run's report, statement by statement.
    pub report: Option<MigrationReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunBy {
    pub id: Option<Uuid>,
    pub name: String,
}

/// A page of the history, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunsPage {
    pub runs: Vec<PlaygroundRun>,
    /// Pass as `before` for the next page; `null` on the last one.
    pub next_before: Option<Uuid>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListRuns {
    /// Entries older than this one (from `next_before`).
    pub before: Option<Uuid>,
    /// 1 to 100; 50 by default.
    pub limit: Option<i64>,
}

/// What to record.
pub struct NewRun<'a> {
    pub playground: Uuid,
    pub user: (Uuid, &'a str),
    pub kind: PlaygroundRunKind,
    pub sql: Option<&'a str>,
    pub duration_ms: u64,
    pub status: PlaygroundRunStatus,
    pub rows: Option<u64>,
    pub error: Option<&'a str>,
    pub report: Option<&'a MigrationReport>,
    pub migration_input_sha256: Option<&'a str>,
}

fn cut(sql: &str) -> &str {
    if sql.len() <= SQL_BYTES {
        return sql;
    }
    let mut end = SQL_BYTES;
    while !sql.is_char_boundary(end) {
        end -= 1;
    }
    &sql[..end]
}

/// Records a console run, and lets the oldest go past [`CONSOLE_RUNS_KEPT`].
pub async fn record_console(db: &PgPool, run: NewRun<'_>) -> Result<PlaygroundRun, sqlx::Error> {
    let playground = run.playground;
    let recorded = record(db, run).await?;
    sqlx::query!(
        "DELETE FROM playground_runs
         WHERE playground_id = $1 AND kind IN ('query', 'explain')
           AND id < (SELECT id FROM playground_runs
                     WHERE playground_id = $1 AND kind IN ('query', 'explain')
                     ORDER BY id DESC OFFSET $2::int8 - 1 LIMIT 1)",
        playground,
        CONSOLE_RUNS_KEPT,
    )
    .execute(db)
    .await?;
    Ok(recorded)
}

/// Records `run`; answers it as the history shows it.
pub async fn record(
    db: impl PgExecutor<'_>,
    run: NewRun<'_>,
) -> Result<PlaygroundRun, sqlx::Error> {
    let error = run
        .error
        .map(|message| serde_json::json!({ "message": message }));
    let report = run
        .report
        .map(|r| serde_json::to_value(r).unwrap_or_default());
    let row = sqlx::query!(
        r#"INSERT INTO playground_runs
               (playground_id, user_id, user_name, kind, sql, migration_input_sha256, duration_ms,
                status, rows, error, report)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
           RETURNING id, started_at"#,
        run.playground,
        run.user.0,
        run.user.1.chars().take(200).collect::<String>(),
        run.kind as PlaygroundRunKind,
        run.sql.map(cut),
        run.migration_input_sha256,
        i64::try_from(run.duration_ms).unwrap_or(i64::MAX),
        run.status as PlaygroundRunStatus,
        run.rows.map(|r| i64::try_from(r).unwrap_or(i64::MAX)),
        error,
        report,
    )
    .fetch_one(db)
    .await?;
    Ok(PlaygroundRun {
        id: row.id,
        kind: run.kind,
        by: RunBy {
            id: Some(run.user.0),
            name: run.user.1.to_owned(),
        },
        sql: run.sql.map(|s| cut(s).to_owned()),
        started_at: row.started_at,
        duration_ms: i64::try_from(run.duration_ms).unwrap_or(i64::MAX),
        status: run.status,
        rows: run.rows.map(|r| i64::try_from(r).unwrap_or(i64::MAX)),
        error: run.error.map(str::to_owned),
        report: run.report.cloned(),
    })
}

struct Stored {
    id: Uuid,
    kind: PlaygroundRunKind,
    user_id: Option<Uuid>,
    user_name: String,
    sql: Option<String>,
    started_at: DateTime<Utc>,
    duration_ms: i64,
    status: PlaygroundRunStatus,
    rows: Option<i64>,
    error: Option<Value>,
    report: Option<Value>,
}

impl From<Stored> for PlaygroundRun {
    fn from(r: Stored) -> Self {
        Self {
            id: r.id,
            kind: r.kind,
            by: RunBy {
                id: r.user_id,
                name: r.user_name,
            },
            sql: r.sql,
            started_at: r.started_at,
            duration_ms: r.duration_ms,
            status: r.status,
            rows: r.rows,
            error: r
                .error
                .and_then(|e| e.get("message").and_then(Value::as_str).map(str::to_owned)),
            report: r.report.and_then(|r| serde_json::from_value(r).ok()),
        }
    }
}

/// One entry, by id.
pub async fn one(db: &PgPool, id: Uuid) -> ApiResult<Option<PlaygroundRun>> {
    Ok(sqlx::query_as!(
        Stored,
        r#"SELECT id, kind AS "kind: PlaygroundRunKind", user_id, user_name, sql, started_at,
                  duration_ms, status AS "status: PlaygroundRunStatus", rows, error, report
           FROM playground_runs WHERE id = $1"#,
        id,
    )
    .fetch_optional(db)
    .await?
    .map(Into::into))
}

/// The playground's history, newest first. Every member.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/playgrounds/{id}/runs",
    operation_id = "list_playground_runs",
    tag = "playgrounds",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path), ListRuns),
    responses(
        (status = 200, body = RunsPage),
        (status = 404, body = ErrorBody),
    )
)]
pub(super) async fn list_runs(
    member: Member,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    Query(query): Query<ListRuns>,
    State(state): State<AppState>,
) -> ApiResult<Json<RunsPage>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let playground = find(&state.db, &database, id).await?;
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let rows = sqlx::query_as!(
        Stored,
        r#"SELECT id, kind AS "kind: PlaygroundRunKind", user_id, user_name, sql, started_at,
                  duration_ms, status AS "status: PlaygroundRunStatus", rows, error, report
           FROM playground_runs
           WHERE playground_id = $1 AND ($2::uuid IS NULL OR id < $2)
           ORDER BY id DESC LIMIT $3"#,
        playground.id,
        query.before,
        limit + 1,
    )
    .fetch_all(&state.db)
    .await?;
    let page = usize::try_from(limit).unwrap_or(usize::MAX);
    let more = rows.len() > page;
    let runs: Vec<PlaygroundRun> = rows.into_iter().take(page).map(Into::into).collect();
    let next_before = if more {
        runs.last().map(|r| r.id)
    } else {
        None
    };
    Ok(Json(RunsPage { runs, next_before }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_sql_is_cut_on_a_character() {
        let sql = "é".repeat(SQL_BYTES);
        let cut = cut(&sql);
        assert!(cut.len() <= SQL_BYTES);
        assert!(cut.chars().all(|c| c == 'é'));
        assert_eq!(super::cut("select 1"), "select 1");
    }
}
