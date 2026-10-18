//! Reset and migration runs: what changes a playground's rows for everyone
//! in it (each is announced on its consoles, recorded in its history and in
//! the audit log). Developers and admins only.
//!
//! A migration run is of the draft's text as the person reviewed it
//! (`sha256`, as the draft's migration endpoint takes it): in schema mode
//! the migration romulus-migration plans from the playground's version to
//! the draft's model, in migration mode the migration as written. The host
//! resets the rows first, then runs and measures each statement
//! (`romulus_playground::server::migrate`); the playground then holds the
//! migrated rows, until Reset. The run continues if the person leaves.
//!
//! Refused before anything streams or is recorded: a busy playground (409
//! `playground_busy`, from the host), a person already running one (409
//! `migration_in_progress`: one at a time each), more than
//! `limits::MIGRATION_STATEMENTS` statements (422 `migration_too_long`).

use std::{convert::Infallible, time::Duration};

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};
use romulus_migration::{Options, Stage};
use romulus_playground::protocol::{
    Migrate, MigrationEvent, MigrationReport, MigrationStatement, StatementStatus, limits,
};
use serde::Deserialize;
use tokio::sync::mpsc;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    Announcement, ConsoleNotice, MAJOR, MigrationGuard, Playground, Row, details, find,
    host::Caller,
    ready,
    runs::{self, NewRun},
    touch,
};
use crate::{
    ApiError, ApiResult, AppState, audit,
    auth::{Member, min},
    databases,
    domain::{AuditAction, DraftMode, DraftStatus, PlaygroundRunKind, PlaygroundRunStatus},
    drafts::{
        Person,
        publish::{compiled, hub_error, reviewed},
        versions::by_id,
    },
    error::ErrorBody,
    extract::{Json, Path},
};

/// A blank line after this long without an event keeps proxies waiting.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// Puts the generated rows back. Developers and admins.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/playgrounds/{id}/reset",
    operation_id = "reset_playground",
    tag = "playgrounds",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    responses(
        (status = 200, body = Playground),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`playground_not_ready`, `playground_expired`, `playground_busy`", body = ErrorBody),
        (status = 429, body = ErrorBody),
        (status = 503, description = "`host_unavailable`", body = ErrorBody),
    )
)]
pub(super) async fn reset(
    member: Member<min::Developer>,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
) -> ApiResult<Json<Playground>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    ready(&row)?;
    state.limits.playground_actions.check(id)?;
    let backend = state.playgrounds.host.backend()?;
    let by = member.user.name.clone();
    let person = Person {
        id: member.user.id,
        name: by.clone(),
    };
    state.playgrounds.tell(
        id,
        ConsoleNotice::Announcement {
            event: Announcement::Resetting,
            by: person.clone(),
        },
    );
    let started = std::time::Instant::now();
    // A busy playground answers 409 at once (no reset queued behind a run).
    let live = backend.reset(id, row.caller(&member)).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query!(
        "UPDATE playgrounds SET migrated_by_run = NULL WHERE id = $1",
        id
    )
    .execute(&mut *tx)
    .await?;
    let run = runs::record(
        &mut *tx,
        NewRun {
            playground: id,
            user: (member.user.id, &member.user.name),
            kind: PlaygroundRunKind::Reset,
            sql: None,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            status: PlaygroundRunStatus::Ok,
            rows: None,
            error: None,
            report: None,
            migration_input_sha256: None,
        },
    )
    .await?;
    audit::record(
        &mut *tx,
        &audit::Entry {
            org_id: member.org.id,
            database_id: Some(database.id),
            action: AuditAction::PlaygroundReset,
            actor_id: Some(member.user.id),
            actor_name: Some(by.clone()),
            version_number: Some(row.version_number),
            reason: None,
            details: details(&row),
        },
    )
    .await?;
    tx.commit().await?;
    touch(&state.db, id).await?;
    state.playgrounds.tell(id, ConsoleNotice::History { run });
    state.playgrounds.tell(
        id,
        ConsoleNotice::Announcement {
            event: Announcement::Reset,
            by: person,
        },
    );
    let row = find(&state.db, &database, id).await?;
    Ok(Json(row.into_playground(&state, Some(live)).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RunMigration {
    /// What the person reviewed: the draft's live compile SHA-256 (the
    /// migration tab's), so a run is never of a text nobody saw (409
    /// `draft_changed` when the draft moved on).
    pub sha256: String,
}

/// The statements a run applies: planned (schema mode) or as written
/// (migration mode).
async fn statements(
    state: &AppState,
    row: &Row,
    draft: Uuid,
    sha256: &str,
) -> ApiResult<Vec<MigrationStatement>> {
    let content = state.collab.content(draft).await.map_err(hub_error)?;
    if reviewed(&content) != sha256 {
        return Err(ApiError::Conflict {
            code: "draft_changed",
            message: "The draft changed since you looked at its migration. Look at it again."
                .into(),
        });
    }
    let compile_failed = || ApiError::Unprocessable {
        code: "compile_failed",
        message: "The draft's SQL has errors: fix them to run its migration.".into(),
        details: None,
    };
    let statements = match content.mode {
        DraftMode::Schema => {
            let (model, _) = compiled(&state.collab, &content).await?;
            let new = model.ok_or_else(compile_failed)?;
            let base = by_id(&state.db, row.database_id, row.version_id)
                .await?
                .ok_or_else(|| ApiError::not_found("The playground's version is gone."))?;
            let old = base.schema()?;
            let planned = tokio::task::spawn_blocking(move || {
                // As the playground's Postgres runs it.
                romulus_migration::migration(
                    &old,
                    &new,
                    &Options {
                        major: Some(MAJOR.number()),
                        ..Options::default()
                    },
                )
            })
            .await
            .map_err(ApiError::internal)?;
            planned
                .statements
                .into_iter()
                .map(|s| MigrationStatement {
                    hazard: s.hazard.and_then(|h| {
                        serde_json::to_value(h)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_owned))
                    }),
                    outside_transaction: s.stage == Stage::Concurrent,
                    sql: s.sql,
                })
                .collect::<Vec<_>>()
        }
        DraftMode::Migration => {
            let text = content.migration.clone();
            let parsed = tokio::task::spawn_blocking(move || {
                let parsed = romulus_sandbox::parse(&text);
                let ok = parsed.errors.is_empty();
                let statements: Vec<String> = parsed
                    .statements
                    .iter()
                    .map(|s| format!("{};", s.text(&text)))
                    .collect();
                (ok, statements)
            })
            .await
            .map_err(ApiError::internal)?;
            if !parsed.0 {
                return Err(compile_failed());
            }
            parsed
                .1
                .into_iter()
                .map(|sql| MigrationStatement {
                    sql,
                    hazard: None,
                    outside_transaction: false,
                })
                .collect()
        }
    };
    if statements.is_empty() {
        return Err(ApiError::Unprocessable {
            code: "nothing_to_migrate",
            message: "The draft changes nothing yet: there is no migration to run.".into(),
            details: None,
        });
    }
    if statements.len() > limits::MIGRATION_STATEMENTS {
        return Err(ApiError::Unprocessable {
            code: "migration_too_long",
            message: format!(
                "The migration has {} statements: a playground runs at most {}.",
                statements.len(),
                limits::MIGRATION_STATEMENTS
            ),
            details: None,
        });
    }
    Ok(statements)
}

/// Runs the draft's migration on the playground's rows (reset first). The
/// answer streams `PlaygroundMigrationEvent`s, one JSON per line (blank
/// lines keep the connection alive), the last one `done` (the report) or
/// `failed`. Developers and admins.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/playgrounds/{id}/migration",
    operation_id = "run_playground_migration",
    tag = "playgrounds",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    request_body = RunMigration,
    responses(
        (status = 200, description = "The progress, one event per line", content(
            (romulus_playground::protocol::MigrationEvent = "application/x-ndjson"),
        )),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`playground_not_ready`, `playground_expired`, `playground_busy`, `migration_in_progress` (yours, elsewhere), `draft_not_open`, `playground_stale` (the draft moved to another base), `draft_changed`", body = ErrorBody),
        (status = 422, description = "`no_migration` (a version's playground), `compile_failed`, `nothing_to_migrate`, `migration_too_long`", body = ErrorBody),
        (status = 429, body = ErrorBody),
        (status = 503, description = "`host_unavailable`", body = ErrorBody),
    )
)]
pub(super) async fn run_migration(
    member: Member<min::Developer>,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
    Json(request): Json<RunMigration>,
) -> ApiResult<Response> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    ready(&row)?;
    let Some(draft) = row.draft_id else {
        return Err(ApiError::Unprocessable {
            code: "no_migration",
            message: "This playground holds a version, not a draft: there is no migration to run. Open a playground from a draft.".into(),
            details: None,
        });
    };
    if row.draft_status != Some(DraftStatus::Open) {
        return Err(ApiError::Conflict {
            code: "draft_not_open",
            message: "The draft was published or deleted.".into(),
        });
    }
    if row.draft_base != Some(row.version_id) {
        return Err(ApiError::Conflict {
            code: "playground_stale",
            message: format!(
                "The draft moved to another version since this playground was made from v{}: create a new playground for it.",
                row.version_number
            ),
        });
    }
    // One run at a time per person, wherever.
    let Some(one) = state.playgrounds.start_migration(member.user.id) else {
        return Err(ApiError::Conflict {
            code: "migration_in_progress",
            message: "You are running a migration already. Wait for it to finish.".into(),
        });
    };
    state.limits.playground_actions.check(id)?;
    // Checked before the statements are planned: a host that is off answers now.
    let backend = state.playgrounds.host.backend()?;
    let statements = statements(&state, &row, draft, &request.sha256).await?;
    let script: String = statements
        .iter()
        .map(|s| s.sql.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let caller = Caller {
        org: member.org.id,
        user: Some((member.user.id, member.user.name.clone())),
        access: romulus_playground::protocol::Access::Develop,
    };
    // The host takes the run or refuses it (busy) before anything streams:
    // a refused run is a 409, never a failed run in the history.
    let received = backend
        .migrate(id, caller.clone(), Migrate { statements })
        .await?;

    let (events, streamed) = mpsc::unbounded_channel();
    let job = Job {
        state: state.clone(),
        row,
        caller,
        user: (member.user.id, member.user.name.clone()),
        sha256: request.sha256,
        script,
        database_id: database.id,
        _one: one,
    };
    tokio::spawn(job.run(received, events));
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        Body::from_stream(ndjson(streamed)),
    )
        .into_response())
}

fn ndjson(
    received: mpsc::UnboundedReceiver<MigrationEvent>,
) -> impl futures_util::Stream<Item = Result<Bytes, Infallible>> {
    futures_util::stream::unfold(received, move |mut received| async move {
        let line = match tokio::time::timeout(KEEP_ALIVE, received.recv()).await {
            Ok(Some(event)) => {
                let mut line = serde_json::to_vec(&event).unwrap_or_default();
                line.push(b'\n');
                line
            }
            Ok(None) => return None,
            Err(_) => b"\n".to_vec(),
        };
        Some((Ok(Bytes::from(line)), received))
    })
}

struct Job {
    state: AppState,
    row: Row,
    caller: Caller,
    user: (Uuid, String),
    sha256: String,
    script: String,
    database_id: Uuid,
    /// The person's one run, until the job ends.
    _one: MigrationGuard,
}

/// How a report reads in the audit log.
fn outcome(report: &MigrationReport) -> String {
    match report.failed_at {
        None => format!("passed ({} statements)", report.statements.len()),
        Some(at) => format!(
            "failed at statement {} of {}",
            at + 1,
            report.statements.len()
        ),
    }
}

impl Job {
    fn person(&self) -> Person {
        Person {
            id: self.user.0,
            name: self.user.1.clone(),
        }
    }

    async fn run(
        self,
        mut received: mpsc::Receiver<MigrationEvent>,
        events: mpsc::UnboundedSender<MigrationEvent>,
    ) {
        let playgrounds = &self.state.playgrounds;
        let id = self.row.id;
        playgrounds.tell(
            id,
            ConsoleNotice::Announcement {
                event: Announcement::Migrating,
                by: self.person(),
            },
        );
        let started = std::time::Instant::now();
        let mut report = None;
        let mut failure = None;
        while let Some(event) = received.recv().await {
            match &event {
                MigrationEvent::Done { report: done } => report = Some(done.clone()),
                MigrationEvent::Failed { message } => failure = Some(message.clone()),
                _ => {}
            }
            let _ = events.send(event);
        }
        if report.is_none() && failure.is_none() {
            failure = Some("The playground host stopped during the run.".to_owned());
        }
        if let Some(message) = &failure
            && report.is_none()
        {
            let _ = events.send(MigrationEvent::Failed {
                message: message.clone(),
            });
        }
        let duration = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match self
            .record(report.as_ref(), failure.as_deref(), duration)
            .await
        {
            Ok(run) => {
                playgrounds.tell(id, ConsoleNotice::History { run });
            }
            Err(err) => {
                tracing::error!(playground = %id, error = %err, "cannot record a migration run");
            }
        }
        playgrounds.tell(
            id,
            ConsoleNotice::Announcement {
                event: Announcement::Migrated,
                by: self.person(),
            },
        );
    }

    async fn record(
        &self,
        report: Option<&MigrationReport>,
        failure: Option<&str>,
        duration_ms: u64,
    ) -> Result<runs::PlaygroundRun, sqlx::Error> {
        let status = match report {
            Some(r) if r.passed => PlaygroundRunStatus::Ok,
            _ => PlaygroundRunStatus::Error,
        };
        let failed = report.and_then(|r| {
            let at = r.failed_at?;
            let statement = r.statements.get(at)?;
            statement.error.as_ref().map(|e| e.message.clone())
        });
        let rows = report.map(|r| {
            r.statements
                .iter()
                .filter(|s| s.status == StatementStatus::Passed)
                .filter_map(|s| s.rows_changed)
                .map(|n| u64::try_from(n).unwrap_or(0))
                .sum::<u64>()
        });
        let mut tx = self.state.db.begin().await?;
        let run = runs::record(
            &mut *tx,
            NewRun {
                playground: self.row.id,
                user: (self.user.0, &self.user.1),
                kind: PlaygroundRunKind::Migration,
                sql: Some(&self.script),
                duration_ms,
                status,
                rows,
                error: failed.as_deref().or(failure),
                report,
                migration_input_sha256: Some(&self.sha256),
            },
        )
        .await?;
        // The rows it holds now are the migrated ones (the run reset them
        // first, so even a failed run left its first statements' work).
        if report.is_some() {
            sqlx::query!(
                "UPDATE playgrounds SET migrated_by_run = $2, last_used_at = now(),
                        expires_at = now() + interval '7 days'
                 WHERE id = $1",
                self.row.id,
                run.id,
            )
            .execute(&mut *tx)
            .await?;
        }
        let mut details = details(&self.row);
        details.outcome = Some(report.map_or_else(
            || format!("did not run: {}", failure.unwrap_or("the host failed")),
            outcome,
        ));
        audit::record(
            &mut *tx,
            &audit::Entry {
                org_id: self.caller.org,
                database_id: Some(self.database_id),
                action: AuditAction::PlaygroundMigrationRun,
                actor_id: Some(self.user.0),
                actor_name: Some(self.user.1.clone()),
                version_number: Some(self.row.version_number),
                reason: None,
                details,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(run)
    }
}

#[cfg(test)]
mod tests {
    use romulus_playground::protocol::StatementResult;

    use super::*;

    fn statement(index: usize, status: StatementStatus) -> StatementResult {
        StatementResult {
            index,
            sql: String::new(),
            status,
            hazard: None,
            duration_ms: 0,
            locks: Vec::new(),
            locks_documented: false,
            rewrote: Vec::new(),
            rows_changed: None,
            tag: None,
            error: None,
            offending: None,
        }
    }

    #[test]
    fn the_audit_log_says_where_a_run_failed() {
        let report = MigrationReport {
            passed: false,
            statements: vec![
                statement(0, StatementStatus::Passed),
                statement(1, StatementStatus::Failed),
                statement(2, StatementStatus::NotRun),
            ],
            failed_at: Some(1),
            duration_ms: 3,
        };
        assert_eq!(outcome(&report), "failed at statement 2 of 3");
    }
}
