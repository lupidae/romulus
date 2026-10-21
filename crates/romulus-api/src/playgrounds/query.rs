//! A console run over REST: `POST …/playgrounds/{id}/query` [member]. The
//! same session the console's websocket opens (the playground's read-only
//! role, the person's access signed by the API, reviewers' shorter
//! deadline), for one run: its statements' columns and first rows, then the
//! session ends (its transaction rolled back). Limited and recorded like a
//! console run (the person's rate and runs in flight; the shared history),
//! so it is a read, as the console is. The MCP endpoint's
//! `playground_query` goes through here.

use std::time::Duration;

use axum::extract::State;
use romulus_migration_check::SqlError;
use romulus_playground::protocol::{Column, ConsoleEvent, ConsoleRequest, RunStatus, limits};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{RUNS_PER_PERSON, console, find, ready};
use crate::{
    ApiError, ApiResult, AppState,
    auth::Member,
    databases,
    drafts::Person,
    error::ErrorBody,
    extract::{Json, Path},
};

/// How long a run may take here, the session's start included (the host's
/// own deadline is shorter for most statements).
const RUN_WAIT: Duration = Duration::from_secs(25);

/// Rows kept per statement unless asked otherwise, and at most.
const DEFAULT_ROWS: usize = 100;
const MAX_ROWS: usize = limits::ROWS;

#[derive(Debug, Deserialize, ToSchema)]
pub struct PlaygroundQuery {
    /// One or more statements, run in order in one transaction (rolled
    /// back), stopping at the first error. At most 256 KiB.
    pub sql: String,
    /// The plan of `sql` (one statement), analyzed: `EXPLAIN (ANALYZE…)`.
    #[serde(default)]
    pub explain: bool,
    /// Rows kept per statement: 100 unless given, at most 1000.
    #[serde(default)]
    pub max_rows: Option<usize>,
}

/// How a run went.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QueryResult {
    /// `ok`, `error` (a statement failed: its `error`) or `cancelled` (it
    /// took longer than this allows).
    pub status: RunStatus,
    pub ms: u64,
    /// The statements that ran, in order.
    pub statements: Vec<QueryStatement>,
    /// Why the run stopped, when it did not finish.
    pub error: Option<String>,
}

/// One statement of a run.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct QueryStatement {
    pub index: usize,
    /// Postgres's command tag (`SELECT 212`).
    pub tag: Option<String>,
    pub columns: Vec<Column>,
    /// Values as Postgres prints them, NULL as null; the first `max_rows`.
    pub rows: Vec<Vec<Option<String>>>,
    /// How many rows it returned or touched.
    pub row_count: u64,
    /// More rows than `rows` holds.
    pub truncated: bool,
    /// The plan, with `explain`.
    pub plan: Option<String>,
    pub notices: Vec<String>,
    pub error: Option<SqlError>,
    pub ms: Option<u64>,
}

/// Runs SQL on the playground, read-only, and answers its results.
#[allow(clippy::too_many_lines, reason = "one console run, start to end")]
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/playgrounds/{id}/query",
    operation_id = "query_playground",
    tag = "playgrounds",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    request_body = PlaygroundQuery,
    responses(
        (status = 200, body = QueryResult),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`playground_not_ready`, `playground_expired`", body = ErrorBody),
        (status = 413, body = ErrorBody),
        (status = 429, description = "`rate_limited`: too many runs, or too many in flight", body = ErrorBody),
        (status = 503, description = "`host_unavailable`, `playground_unavailable`", body = ErrorBody),
    )
)]
pub(super) async fn query(
    member: Member,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
    Json(body): Json<PlaygroundQuery>,
) -> ApiResult<Json<QueryResult>> {
    if body.sql.len() > limits::SQL_BYTES {
        return Err(ApiError::PayloadTooLarge);
    }
    if body.sql.trim().is_empty() {
        return Err(crate::orgs::invalid("sql", "Write a query to run."));
    }
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    ready(&row)?;
    let backend = state.playgrounds.host.backend()?;
    state.limits.playground_console.check(member.user.id)?;
    let Some(_counted) = state.playgrounds.start_run(member.user.id) else {
        return Err(ApiError::Conflict {
            code: "runs_in_flight",
            message: format!(
                "You have {RUNS_PER_PERSON} runs in progress (here or in the console): wait \
                 for one to finish."
            ),
        });
    };
    let max_rows = body.max_rows.unwrap_or(DEFAULT_ROWS).clamp(1, MAX_ROWS);
    let mut link = backend.console(id, row.caller(&member)).await?;
    let run = "q".to_owned();
    let request = ConsoleRequest::Run {
        id: run.clone(),
        sql: body.sql.clone(),
        explain: body.explain,
    };
    let encoded = serde_json::to_string(&request).map_err(ApiError::internal)?;
    if link.to_host.send(encoded).await.is_err() {
        return Err(gone());
    }

    let mut result = QueryResult {
        status: RunStatus::Ok,
        ms: 0,
        statements: Vec::new(),
        error: None,
    };
    let deadline = tokio::time::Instant::now() + RUN_WAIT;
    let mut cancelled = false;
    let finished = loop {
        let frame = match tokio::time::timeout_at(deadline, link.from_host.recv()).await {
            Ok(Some(frame)) => frame,
            Ok(None) => return Err(gone()),
            Err(_) if !cancelled => {
                // Too long: stop it, and answer what came.
                cancelled = true;
                let cancel = serde_json::to_string(&ConsoleRequest::Cancel { id: run.clone() })
                    .map_err(ApiError::internal)?;
                let _ = link.to_host.send(cancel).await;
                match tokio::time::timeout(Duration::from_secs(3), link.from_host.recv()).await {
                    Ok(Some(frame)) => frame,
                    _ => break None,
                }
            }
            Err(_) => break None,
        };
        let Ok(event) = serde_json::from_str::<ConsoleEvent>(&frame) else {
            continue;
        };
        match event {
            ConsoleEvent::Statement { index, .. } => {
                statement(&mut result, index);
            }
            ConsoleEvent::Columns { index, columns, .. } => {
                statement(&mut result, index).columns = columns;
            }
            ConsoleEvent::Rows { index, rows, .. } => {
                let kept = statement(&mut result, index);
                let room = max_rows.saturating_sub(kept.rows.len());
                if rows.len() > room {
                    kept.truncated = true;
                }
                kept.rows.extend(rows.into_iter().take(room));
            }
            ConsoleEvent::Plan { index, text, .. } => {
                statement(&mut result, index).plan = Some(text);
            }
            ConsoleEvent::Notice {
                index,
                severity,
                message,
                ..
            } => {
                statement(&mut result, index)
                    .notices
                    .push(format!("{severity}: {message}"));
            }
            ConsoleEvent::Done {
                index,
                tag,
                rows,
                truncated,
                ms,
                ..
            } => {
                let done = statement(&mut result, index);
                done.tag = Some(tag);
                done.row_count = rows;
                done.truncated |= truncated || rows > done.rows.len() as u64;
                done.ms = Some(ms);
            }
            ConsoleEvent::Error { index, error, .. } => {
                statement(&mut result, index).error = Some(error);
            }
            ConsoleEvent::Finished {
                id: finished,
                status,
                statements,
                rows,
                ms,
                error,
            } if finished == run => {
                result.status = status;
                result.ms = ms;
                result.error.clone_from(&error);
                break Some((status, statements, rows, ms, error));
            }
            ConsoleEvent::Unavailable { reason, .. } => {
                return Err(ApiError::Down {
                    code: "playground_unavailable",
                    message: reason,
                });
            }
            ConsoleEvent::Deleting => {
                return Err(ApiError::not_found("The playground is being deleted."));
            }
            _ => {}
        }
    };
    // Dropping the link ends the session (its transaction rolls back).
    drop(link);
    let Some((status, statements, rows, ms, error)) = finished else {
        result.status = RunStatus::Cancelled;
        result.error = Some(format!(
            "The run took longer than {} s and was stopped.",
            RUN_WAIT.as_secs()
        ));
        return Ok(Json(result));
    };
    if cancelled {
        result.error = Some(format!(
            "The run took longer than {} s and was stopped.",
            RUN_WAIT.as_secs()
        ));
    }
    // A run that ran nothing (a syntax error) is the person's own business.
    if statements > 0 {
        console::record_run(
            &state,
            id,
            &Person {
                id: member.user.id,
                name: member.user.name.clone(),
            },
            &body.sql,
            body.explain,
            (status, rows, ms, error.as_deref()),
        )
        .await;
    }
    Ok(Json(result))
}

/// The statement `index` of the result, added if new.
fn statement(result: &mut QueryResult, index: usize) -> &mut QueryStatement {
    let at = if let Some(at) = result.statements.iter().position(|s| s.index == index) {
        at
    } else {
        result.statements.push(QueryStatement {
            index,
            ..QueryStatement::default()
        });
        result.statements.len() - 1
    };
    &mut result.statements[at]
}

fn gone() -> ApiError {
    ApiError::Down {
        code: "host_unavailable",
        message: "The playground host went away. Try again in a moment.".into(),
    }
}
