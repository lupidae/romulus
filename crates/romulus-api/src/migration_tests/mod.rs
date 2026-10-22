//! Test migration (docs/playground.md, "Test migration"): one click runs a
//! migration up and down on a playground of its own, filled with generated
//! rows, and says whether everything is alright: the statements (each
//! measured: duration, locks, rewrites), the schema after each direction,
//! the rows (counts, a sample by primary key followed through renames and
//! conversions), and the data that does not come back, matched against
//! what the migration flags. The checks are romulus-migration-check's, the
//! migration oracle's own: the playground service runs them
//! (`romulus_playground::server::testing`).
//!
//! A test's playground is short-lived: built for it, deleted after its
//! report (counted in the host's caps meanwhile). Tests wait for their turn
//! ([`queue`]: a few at a time, one per organization), their place in line
//! live. The report is kept: everyone in the organization sees it, and a
//! draft's, a pair of versions' or a version's tests are its history.
//!
//! Endpoints (who may call them in brackets; see `auth::member`):
//! - `POST /orgs/{org}/databases/{db}/migration-tests` [developer]: test a
//!   draft's migration as reviewed, the migration between two versions, or
//!   a version's own written or imported migration ([`plan`]); 201 with
//!   the test, queued; it runs in the background ([`job`]).
//! - `GET …/migration-tests?draft=|from=&to=|version=` [member]: the
//!   newest tests, of what is named (all of the database's otherwise),
//!   without their reports.
//! - `GET …/migration-tests/{id}` [member]: one, with its live progress or
//!   its report.
//! - `POST …/migration-tests/{id}/cancel` [developer]: stops it, deletes
//!   its playground.
//!
//! Developers and admins run tests (as Run migration); reviewers read them.
//! Tables: `migration_tests`.

pub mod job;
pub mod plan;
pub mod queue;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use romulus_migration_check::{Direction, Phase, Report, Status as CheckStatus, Verdict};
use serde::{Deserialize, Serialize};
use tokio::task::AbortHandle;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub use self::plan::{CreateMigrationTest, DownChoice};
use self::{job::Job, queue::Queue};
use crate::{
    ApiError, ApiResult, AppState,
    auth::{Member, min},
    collab::compile::input_hash,
    databases::{self, DatabaseRef},
    domain::{
        AuditAction, MigrationTestDown, MigrationTestSource, MigrationTestStatus,
        MigrationTestVerdict, PlaygroundProfile, PlaygroundSize, Role,
    },
    drafts::Person,
    error::ErrorBody,
    extract::{Json, Path, Query},
    playgrounds::{
        BuildProgress,
        build::{check_major, seed},
        host::Caller,
    },
};

/// Tests a list answers, at most.
pub const LISTED: i64 = 30;

/// A new id, as the database mints them (`uuidv7()`): a test's, before
/// its row (its place in line comes first), and its playground's.
pub(crate) async fn new_id(db: &sqlx::PgPool) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar!(r#"SELECT uuidv7() AS "id!""#)
        .fetch_one(db)
        .await
}

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get))
        .routes(routes!(cancel))
        .routes(routes!(acknowledge))
}

/// Where a test is, while it is not over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TestStage {
    /// In line, or waiting for room on the host (`waiting` says why).
    Queued,
    /// Its playground is being built.
    Building,
    /// The migration runs on it.
    Testing,
}

/// A phase that ended, as the page lists them while the test runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PhaseDone {
    pub phase: Phase,
    pub status: CheckStatus,
}

/// What a running test is doing.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TestProgress {
    pub stage: TestStage,
    /// Why it waits for room, when it does.
    pub waiting: Option<String>,
    /// Its playground's build, while building.
    pub build: Option<BuildProgress>,
    /// The phase running, while testing.
    pub phase: Option<Phase>,
    /// The statement running (from 0) of `statements`, and which way.
    pub direction: Option<Direction>,
    pub statement: Option<usize>,
    pub statements: Option<usize>,
    /// The phases done so far.
    pub done: Vec<PhaseDone>,
}

impl TestProgress {
    pub fn stage(stage: TestStage) -> Self {
        Self {
            stage,
            waiting: None,
            build: None,
            phase: None,
            direction: None,
            statement: None,
            statements: None,
            done: Vec::new(),
        }
    }
}

/// Tests in flight in this API process: their progress, their tasks (for
/// Cancel), the queue. Cheap to clone; in `AppState`.
#[derive(Clone, Default)]
pub struct MigrationTests {
    pub queue: Queue,
    live: Arc<Mutex<Live>>,
}

#[derive(Default)]
struct Live {
    progress: HashMap<Uuid, TestProgress>,
    jobs: HashMap<Uuid, AbortHandle>,
    /// When each playground was last copied for a test: a copy pauses its
    /// owner's playground, so one a minute at most ([`FORK_EVERY`]).
    forked: HashMap<Uuid, tokio::time::Instant>,
}

/// One copy of a playground a minute, at most: each pauses it a moment.
pub const FORK_EVERY: std::time::Duration = std::time::Duration::from_mins(1);

impl MigrationTests {
    fn live(&self) -> std::sync::MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set_progress(&self, test: Uuid, progress: Option<TestProgress>) {
        let mut live = self.live();
        match progress {
            Some(progress) => live.progress.insert(test, progress),
            None => live.progress.remove(&test),
        };
    }

    pub fn progress(&self, test: Uuid) -> Option<TestProgress> {
        self.live().progress.get(&test).cloned()
    }

    /// How long to wait before copying `playground` again (zero: now),
    /// the copy then counted as made.
    pub fn fork_turn(&self, playground: Uuid) -> std::time::Duration {
        let mut live = self.live();
        let now = tokio::time::Instant::now();
        let next = live
            .forked
            .get(&playground)
            .map_or(now, |last| (*last + FORK_EVERY).max(now));
        live.forked.insert(playground, next);
        live.forked
            .retain(|_, at| now.saturating_duration_since(*at) < FORK_EVERY);
        next - now
    }

    fn started(&self, test: Uuid, job: AbortHandle) {
        self.live().jobs.insert(test, job);
    }

    /// The job is over: nothing to cancel any more.
    pub fn forget(&self, test: Uuid) {
        self.live().jobs.remove(&test);
    }

    /// Stops the job (its place or turn goes with it). False when this
    /// process had none running.
    fn abort(&self, test: Uuid) -> bool {
        let job = self.live().jobs.remove(&test);
        self.set_progress(test, None);
        job.is_some_and(|job| {
            job.abort();
            true
        })
    }
}

/// The draft a test is of.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TestDraft {
    pub id: Uuid,
    pub name: String,
}

/// The rows a test runs on.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TestData {
    pub size: PlaygroundSize,
    pub profile: PlaygroundProfile,
    /// Hex: the same model, size, profile, seed and generator give the same
    /// rows.
    pub seed: String,
    /// The playground whose rows were copied for the test; `null` for rows
    /// generated for it.
    pub playground: Option<TestPlayground>,
}

/// A playground a test ran on a copy of.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TestPlayground {
    /// `null` once that playground is deleted.
    pub id: Option<Uuid>,
    /// `Alice's playground (small, seed 7f3a91)`.
    pub label: String,
}

/// The statement that failed, for lists.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FailedStatement {
    pub direction: Direction,
    pub index: usize,
    pub sqlstate: Option<String>,
    pub message: Option<String>,
}

/// A test's report in a line, for lists.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TestSummary {
    pub verdict: Verdict,
    pub headline: String,
    pub up_statements: usize,
    /// `null` when no down migration ran.
    pub down_statements: Option<usize>,
    /// Findings of data that does not come back, and of those, the ones no
    /// flagged statement explains.
    pub data_loss: usize,
    pub unexpected_loss: usize,
    pub failed: Option<FailedStatement>,
    pub rows: i64,
    pub duration_ms: u64,
}

impl TestSummary {
    fn of(report: &Report) -> Self {
        Self {
            verdict: report.verdict,
            headline: report.headline.clone(),
            up_statements: report.up.statements.len(),
            down_statements: report.down.as_ref().map(|d| d.statements.len()),
            data_loss: report.data_loss.len(),
            unexpected_loss: report.data_loss.iter().filter(|l| !l.expected).count(),
            failed: report
                .failed_statement()
                .map(|(direction, s)| FailedStatement {
                    direction,
                    index: s.index,
                    sqlstate: s.error.as_ref().map(|e| e.sqlstate.clone()),
                    message: s.error.as_ref().map(|e| e.message.clone()),
                }),
            rows: report.rows,
            duration_ms: report.duration_ms,
        }
    }
}

/// A migration test.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MigrationTest {
    pub id: Uuid,
    pub status: MigrationTestStatus,
    pub source: MigrationTestSource,
    /// The draft tested (`source` `draft`).
    pub draft: Option<TestDraft>,
    /// The version its playground holds: the base.
    pub from: i32,
    /// The version migrated to (`versions`, `version`).
    pub to: Option<i32>,
    pub down: MigrationTestDown,
    /// The report's verdict, once it ran (`passed_up_only`: no down
    /// migration ran; `needs_acknowledgement`: see `acknowledged`).
    pub verdict: Option<MigrationTestVerdict>,
    /// False when every table the migration changes (something rows matter
    /// to touches it) was empty: nothing was checked on rows (a publish gate
    /// does not count it). Renames, nullable columns and plain indexes need
    /// no rows.
    pub rows_checked: bool,
    /// Who acknowledged the data loss it found that the migration does not
    /// declare, and when (it then counts as passed).
    pub acknowledged: Option<Acknowledged>,
    pub data: TestData,
    /// For a draft: the SHA-256 of the text tested (`compile::input_hash`,
    /// a live compile's).
    pub sha256: Option<String>,
    /// For a draft's tests listed by `?draft=`: whether it is of the
    /// draft's text as it is now (`null` elsewhere, or when the draft
    /// cannot be read).
    pub current: Option<bool>,
    /// Its place in line, from 1, while queued.
    pub position: Option<usize>,
    /// While it runs (in this API process).
    pub progress: Option<TestProgress>,
    pub summary: Option<TestSummary>,
    /// The whole report (one test's answer, not lists').
    pub report: Option<Box<Report>>,
    /// Why it could not run (`error`).
    pub failure: Option<String>,
    /// The up migration as tested, and the down one (`null`: none).
    pub up_sql: Option<String>,
    pub down_sql: Option<String>,
    /// `null` once their account is gone.
    pub created_by: Option<Person>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// A developer's acknowledgement of a test's undeclared data loss.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Acknowledged {
    /// `null` once their account is gone.
    pub by: Option<Person>,
    pub at: DateTime<Utc>,
}

#[allow(clippy::struct_field_names, reason = "the columns' names")]
struct Row {
    id: Uuid,
    status: MigrationTestStatus,
    source: MigrationTestSource,
    draft_id: Option<Uuid>,
    draft_name: Option<String>,
    from_number: i32,
    to_number: Option<i32>,
    down: MigrationTestDown,
    verdict: Option<MigrationTestVerdict>,
    rows_checked: bool,
    acknowledged_by: Option<Uuid>,
    acknowledger_name: Option<String>,
    acknowledged_at: Option<DateTime<Utc>>,
    size: PlaygroundSize,
    profile: PlaygroundProfile,
    seed: i64,
    data_playground_id: Option<Uuid>,
    data_label: Option<String>,
    input_sha256: Option<Vec<u8>>,
    report: Option<serde_json::Value>,
    failure: Option<String>,
    up_sql: String,
    down_sql: Option<String>,
    created_by: Option<Uuid>,
    creator_name: Option<String>,
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    org_id: Uuid,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

impl Row {
    fn into_test(self, state: &AppState, whole: bool) -> MigrationTest {
        let report: Option<Report> = self.report.and_then(|value| {
            serde_json::from_value(value)
                .map_err(|e| tracing::warn!(test = %self.id, error = %e, "unreadable report"))
                .ok()
        });
        let live = !self.status.is_final();
        MigrationTest {
            id: self.id,
            status: self.status,
            source: self.source,
            draft: self
                .draft_id
                .zip(self.draft_name)
                .map(|(id, name)| TestDraft { id, name }),
            from: self.from_number,
            to: self.to_number,
            down: self.down,
            verdict: self.verdict,
            rows_checked: self.rows_checked,
            acknowledged: self.acknowledged_at.map(|at| Acknowledged {
                by: self
                    .acknowledged_by
                    .zip(self.acknowledger_name)
                    .map(|(id, name)| Person { id, name }),
                at,
            }),
            data: TestData {
                size: self.size,
                profile: self.profile,
                seed: format!("{:06x}", self.seed),
                playground: self.data_label.map(|label| TestPlayground {
                    id: self.data_playground_id,
                    label,
                }),
            },
            sha256: self.input_sha256.as_deref().map(hex),
            current: None,
            position: (self.status == MigrationTestStatus::Queued)
                .then(|| state.migration_tests.queue.position(self.id))
                .flatten(),
            progress: live
                .then(|| state.migration_tests.progress(self.id))
                .flatten(),
            summary: report.as_ref().map(TestSummary::of),
            report: whole.then(|| report.map(Box::new)).flatten(),
            failure: self.failure,
            up_sql: whole.then_some(self.up_sql),
            down_sql: if whole { self.down_sql } else { None },
            created_by: self
                .created_by
                .zip(self.creator_name)
                .map(|(id, name)| Person { id, name }),
            created_at: self.created_at,
            started_at: self.started_at,
            finished_at: self.finished_at,
        }
    }
}

/// The test `id` of `database`, or 404.
async fn find(state: &AppState, database: &DatabaseRef, id: Uuid) -> ApiResult<Row> {
    sqlx::query_as!(
        Row,
        r#"SELECT t.id, t.status AS "status: MigrationTestStatus",
                  t.source AS "source: MigrationTestSource", t.draft_id, d.name AS "draft_name?",
                  f.number AS from_number, v.number AS "to_number?",
                  t.down AS "down: MigrationTestDown",
                  t.verdict AS "verdict: MigrationTestVerdict", t.rows_checked,
                  t.acknowledged_by, a.name AS "acknowledger_name?", t.acknowledged_at,
                  t.size AS "size: PlaygroundSize",
                  t.profile AS "profile: PlaygroundProfile", t.seed, t.data_playground_id, t.data_label,
                  t.input_sha256,
                  t.report, t.failure, t.up_sql, t.down_sql, t.created_by, u.name AS "creator_name?",
                  t.created_at, t.started_at, t.finished_at, t.org_id
           FROM migration_tests t
           JOIN versions f ON f.id = t.from_version_id
           LEFT JOIN versions v ON v.id = t.to_version_id
           LEFT JOIN drafts d ON d.id = t.draft_id
           LEFT JOIN users u ON u.id = t.created_by
           LEFT JOIN users a ON a.id = t.acknowledged_by
           WHERE t.id = $1 AND t.database_id = $2"#,
        id,
        database.id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("No such migration test."))
}

/// Which tests to list.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListMigrationTests {
    /// A draft's tests.
    pub draft: Option<Uuid>,
    /// The tests of the migration from `from` to `to`.
    pub from: Option<i32>,
    pub to: Option<i32>,
    /// The tests of the migration version `version` was written or
    /// imported as.
    pub version: Option<i32>,
}

/// The newest tests of a draft, of a pair of versions, of a version's own
/// migration, or of the database: without their reports (`summary` says
/// how each went).
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/migration-tests",
    operation_id = "list_migration_tests",
    tag = "migration_tests",
    params(("org" = String, Path), ("db" = String, Path), ListMigrationTests),
    responses(
        (status = 200, body = Vec<MigrationTest>),
        (status = 404, body = ErrorBody),
    )
)]
async fn list(
    member: Member,
    Path((_, db_slug)): Path<(String, String)>,
    Query(which): Query<ListMigrationTests>,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<MigrationTest>>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let (source, from, to) = match (which.draft, which.from, which.to, which.version) {
        (Some(_), None, None, None) => (Some(MigrationTestSource::Draft), None, None),
        (None, Some(from), Some(to), None) => {
            (Some(MigrationTestSource::Versions), Some(from), Some(to))
        }
        (None, None, None, Some(version)) => {
            (Some(MigrationTestSource::Version), None, Some(version))
        }
        (None, None, None, None) => (None, None, None),
        _ => {
            return Err(ApiError::Validation {
                message: "Name a draft, two versions (from and to), or one version.".into(),
                details: None,
            });
        }
    };
    let rows = sqlx::query_as!(
        Row,
        r#"SELECT t.id, t.status AS "status: MigrationTestStatus",
                  t.source AS "source: MigrationTestSource", t.draft_id, d.name AS "draft_name?",
                  f.number AS from_number, v.number AS "to_number?",
                  t.down AS "down: MigrationTestDown",
                  t.verdict AS "verdict: MigrationTestVerdict", t.rows_checked,
                  t.acknowledged_by, a.name AS "acknowledger_name?", t.acknowledged_at,
                  t.size AS "size: PlaygroundSize",
                  t.profile AS "profile: PlaygroundProfile", t.seed, t.data_playground_id, t.data_label,
                  t.input_sha256,
                  t.report, t.failure, t.up_sql, t.down_sql, t.created_by, u.name AS "creator_name?",
                  t.created_at, t.started_at, t.finished_at, t.org_id
           FROM migration_tests t
           JOIN versions f ON f.id = t.from_version_id
           LEFT JOIN versions v ON v.id = t.to_version_id
           LEFT JOIN drafts d ON d.id = t.draft_id
           LEFT JOIN users u ON u.id = t.created_by
           LEFT JOIN users a ON a.id = t.acknowledged_by
           WHERE t.database_id = $1
             AND ($2::migration_test_source IS NULL OR t.source = $2)
             AND ($3::uuid IS NULL OR t.draft_id = $3)
             AND ($4::int IS NULL OR f.number = $4)
             AND ($5::int IS NULL OR v.number = $5)
           ORDER BY t.created_at DESC
           LIMIT $6"#,
        database.id,
        source as Option<MigrationTestSource>,
        which.draft,
        from,
        to,
        LISTED,
    )
    .fetch_all(&state.db)
    .await?;
    // Which of a draft's tests are of its text now.
    let now = match which.draft {
        Some(draft) if !rows.is_empty() => state
            .collab
            .content(draft)
            .await
            .ok()
            .map(|content| input_hash(content.mode, &content.base_sql, content.source())),
        _ => None,
    };
    Ok(Json(
        rows.into_iter()
            .map(|row| {
                let mut test = row.into_test(&state, false);
                if which.draft.is_some() {
                    test.current = now.as_ref().map(|now| test.sha256.as_ref() == Some(now));
                }
                test
            })
            .collect(),
    ))
}

/// Tests a migration on a playground of its own, filled with generated
/// rows: up, checks, down, checks. Answers at once with the test, queued;
/// `GET` it for its place in line, its progress, then its report.
/// Developers and admins.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/migration-tests",
    operation_id = "create_migration_test",
    tag = "migration_tests",
    params(("org" = String, Path), ("db" = String, Path)),
    request_body = CreateMigrationTest,
    responses(
        (status = 201, body = MigrationTest),
        (status = 402, description = "`plan_limit`: a size or playground hours beyond the plan (hosted billing)", body = ErrorBody),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`draft_changed` (look at the migration again), `draft_not_open`, `test_queue_full`", body = ErrorBody),
        (status = 422, description = "`validation`; `compile_failed`, `nothing_to_migrate`, `migration_too_long`, `migration_unreadable`, `no_written_migration`, `no_previous_version`, `no_version`, `major_unsupported`", body = ErrorBody),
        (status = 429, body = ErrorBody),
        (status = 503, description = "`host_unavailable`", body = ErrorBody),
    )
)]
async fn create(
    member: Member<min::Developer>,
    Path((_, db_slug)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<CreateMigrationTest>,
) -> ApiResult<(StatusCode, Json<MigrationTest>)> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    // A test builds a playground: the same limit as creating one.
    state.limits.playgrounds.check(member.user.id)?;
    check_major(&database)?;
    // Off: say so before anything is planned or stored.
    state.playgrounds.host.backend()?;
    let planned = plan::plan(&state, &database, &request).await?;
    let (data, size, profile, seed) = match request.playground {
        Some(playground) => {
            let row: Source = on_playground(&state, &database, playground, &planned).await?;
            (
                job::Data::Playground {
                    source: row.id,
                    label: row.label.clone(),
                },
                row.size,
                row.profile,
                row.seed,
            )
        }
        None => (
            job::Data::Generated,
            request.size.unwrap_or(PlaygroundSize::Small),
            request.profile,
            seed(request.seed.as_deref())?,
        ),
    };
    if let Some(plan) = state.billing.plan(&state.db, member.org.id).await? {
        crate::billing::caps::playground_size(plan, size)?;
        crate::billing::hours::check(&state, member.org.id).await?;
    }
    let (data_playground, data_label) = match &data {
        job::Data::Playground { source, label } => (Some(*source), Some(label.clone())),
        job::Data::Generated => (None, None),
    };
    let id = new_id(&state.db).await?;
    let place = state.migration_tests.queue.enqueue(id, member.org.id)?;
    sqlx::query!(
        "INSERT INTO migration_tests (id, org_id, database_id, source, draft_id, from_version_id,
                                      to_version_id, input_sha256, up_sql, down_sql, down, size,
                                      profile, seed, created_by, data_playground_id, data_label,
                                      gate_sha256)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)",
        id,
        member.org.id,
        database.id,
        planned.source as MigrationTestSource,
        planned.draft.as_ref().map(|(id, _)| *id),
        planned.base.id,
        planned.to.map(|(id, _)| id),
        planned.input_sha256.as_deref(),
        planned.up_sql,
        planned.down_sql.as_deref(),
        planned.down_origin as MigrationTestDown,
        size as PlaygroundSize,
        profile as PlaygroundProfile,
        seed,
        member.user.id,
        data_playground,
        data_label,
        planned.gate_sha256.as_deref(),
    )
    .execute(&state.db)
    .await?;
    let job = Job {
        state: state.clone(),
        id,
        org: member.org.id,
        user: (member.user.id, member.user.name.clone()),
        planned,
        data,
        size,
        profile,
        seed,
        place,
    };
    let task = tokio::spawn(job.run());
    state.migration_tests.started(id, task.abort_handle());
    let row = find(&state, &database, id).await?;
    Ok((StatusCode::CREATED, Json(row.into_test(&state, true))))
}

/// A playground a test may run on (a copy of its rows).
struct Source {
    id: Uuid,
    label: String,
    size: PlaygroundSize,
    profile: PlaygroundProfile,
    seed: i64,
}

/// The playground `id` of `database`, if a test of `planned` may run on its
/// rows: ready, holding the base version as built (no migration run's rows
/// in it).
async fn on_playground(
    state: &AppState,
    database: &DatabaseRef,
    id: Uuid,
    planned: &plan::Planned,
) -> ApiResult<Source> {
    let row = crate::playgrounds::find(&state.db, database, id).await?;
    crate::playgrounds::ready(&row)?;
    if row.version_id != planned.base.id {
        return Err(ApiError::Conflict {
            code: "playground_other_version",
            message: format!(
                "This playground is at v{}, and the migration starts from {}. Test on fresh generated data instead.",
                row.version_number, planned.base_label
            ),
        });
    }
    if row.migrated_by_run.is_some() {
        return Err(ApiError::Conflict {
            code: "playground_migrated",
            message: format!(
                "This playground holds the rows a migration run left: it is not at {}'s schema any more. Reset its rows, or test on fresh generated data.",
                planned.base_label
            ),
        });
    }
    let owner = row
        .creator_name
        .clone()
        .map_or_else(|| "A".to_owned(), |name| format!("{name}'s"));
    let size = match row.size {
        PlaygroundSize::Small => "small",
        PlaygroundSize::Medium => "medium",
    };
    Ok(Source {
        id: row.id,
        label: format!("{owner} playground ({size}, seed {:06x})", row.seed),
        size: row.size,
        profile: row.profile,
        seed: row.seed,
    })
}

/// One test: its place in line or progress while it runs, its report once
/// it ran.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/migration-tests/{id}",
    operation_id = "get_migration_test",
    tag = "migration_tests",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    responses(
        (status = 200, body = MigrationTest),
        (status = 404, body = ErrorBody),
    )
)]
async fn get(
    member: Member,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
) -> ApiResult<Json<MigrationTest>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state, &database, id).await?;
    Ok(Json(row.into_test(&state, true)))
}

/// Stops a test that waits or runs, and deletes its playground. Developers
/// and admins.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/migration-tests/{id}/cancel",
    operation_id = "cancel_migration_test",
    tag = "migration_tests",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    responses(
        (status = 200, body = MigrationTest),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`test_finished`: it ran already", body = ErrorBody),
    )
)]
async fn cancel(
    member: Member<min::Developer>,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
) -> ApiResult<Json<MigrationTest>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state, &database, id).await?;
    if row.status.is_final() {
        return Err(ApiError::Conflict {
            code: "test_finished",
            message: "This test is over already.".into(),
        });
    }
    state.migration_tests.abort(id);
    let stopped = sqlx::query_scalar!(
        "UPDATE migration_tests SET status = 'cancelled', finished_at = now(),
                failure = $2
         WHERE id = $1 AND status IN ('queued', 'running')
         RETURNING playground_id",
        id,
        format!("Stopped by {}.", member.user.name),
    )
    .fetch_optional(&state.db)
    .await?;
    if let Some(Some(playground)) = stopped
        && let Ok(backend) = state.playgrounds.host.backend()
    {
        let caller = Caller {
            org: row.org_id,
            user: Some((member.user.id, member.user.name.clone())),
            access: romulus_playground::protocol::Access::Develop,
        };
        match backend.destroy(playground, caller).await {
            Ok(()) => {
                sqlx::query!(
                    "UPDATE migration_tests SET playground_id = NULL WHERE id = $1",
                    id
                )
                .execute(&state.db)
                .await?;
            }
            // The sweeper takes it later (no live test holds it).
            Err(err) => {
                tracing::warn!(test = %id, error = %err, "cannot delete a cancelled test's playground yet");
            }
        }
    }
    let row = find(&state, &database, id).await?;
    Ok(Json(row.into_test(&state, true)))
}

/// What the publish gate of an organization requiring a passing test
/// (`organizations.require_passing_test`) says of a draft as it is: the
/// publish dialog shows it, publishing enforces it ([`gate`]).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TestGate {
    /// The organization publishes only migrations that passed a test.
    pub required: bool,
    /// Tests can run here (playgrounds on this server, the database on
    /// their Postgres major).
    pub available: bool,
    /// Nothing to test: no base version (no rows yet), or nothing to run.
    pub nothing_to_test: bool,
    /// The newest test of exactly the migration publishing would run that
    /// counts: passed up and down on rows (or its undeclared loss
    /// acknowledged).
    pub passing: Option<Uuid>,
    /// Why it holds publishing, when it does.
    pub reason: Option<String>,
}

impl TestGate {
    /// Publishing may go ahead (an override aside).
    pub fn open(&self) -> bool {
        !self.required || self.nothing_to_test || self.passing.is_some()
    }
}

/// The gate for `draft` (`content` as it is, compiled to `model`, on
/// version `base`) with the renames `decisions`. When it is not required,
/// nothing is planned.
pub async fn gate_state(
    state: &AppState,
    database: &DatabaseRef,
    draft: Uuid,
    content: &crate::collab::room::Content,
    model: &remus_core::Schema,
    required: bool,
) -> ApiResult<TestGate> {
    let available = state.playgrounds.host.is_on() && check_major(database).is_ok();
    let mut gate = TestGate {
        required,
        available,
        nothing_to_test: false,
        passing: None,
        reason: None,
    };
    if !required {
        return Ok(gate);
    }
    let base = match content.base_version {
        Some(base) => crate::drafts::versions::by_id(&state.db, database.id, base).await?,
        None => None,
    };
    let Some(base) = base else {
        gate.nothing_to_test = true;
        return Ok(gate);
    };
    // Its two forms: indexes built in a transaction, or `CONCURRENTLY`.
    let mut keys: Vec<Vec<u8>> = Vec::new();
    for concurrently in [false, true] {
        if let Some(key) =
            plan::gate_key(&base, content, &content.renames, model, concurrently).await?
            && !keys.contains(&key)
        {
            keys.push(key);
        }
    }
    if keys.is_empty() {
        gate.nothing_to_test = true;
        return Ok(gate);
    }
    // The newest test of this exact migration, and the newest that counts:
    // passed (or its loss acknowledged) with its down migration run, on
    // rows. An up-only test never counts, acknowledged or not.
    let tests = sqlx::query!(
        r#"SELECT id, status AS "status: MigrationTestStatus",
                  verdict AS "verdict: MigrationTestVerdict", rows_checked,
                  down AS "down: MigrationTestDown"
           FROM migration_tests
           WHERE draft_id = $1 AND gate_sha256 = ANY($2)
           ORDER BY created_at DESC LIMIT 20"#,
        draft,
        &keys,
    )
    .fetch_all(&state.db)
    .await?;
    let counts = |status: MigrationTestStatus,
                  verdict: Option<MigrationTestVerdict>,
                  rows: bool,
                  down: MigrationTestDown| {
        status == MigrationTestStatus::Passed
            && rows
            && down != MigrationTestDown::None
            && matches!(
                verdict,
                Some(MigrationTestVerdict::Passed | MigrationTestVerdict::NeedsAcknowledgement)
            )
    };
    gate.passing = tests
        .iter()
        .find(|t| counts(t.status, t.verdict, t.rows_checked, t.down))
        .map(|t| t.id);
    if gate.passing.is_some() {
        return Ok(gate);
    }
    gate.reason = Some(if available {
        match tests.first() {
            None => "Publishing needs a passing test of this migration: Test on data is in the Migration tab.".to_owned(),
            Some(t) if !t.status.is_final() => {
                "A test of this migration is running: publish once it passes.".to_owned()
            }
            Some(t)
                if t.verdict == Some(MigrationTestVerdict::PassedUpOnly)
                    || (t.status == MigrationTestStatus::Passed
                        && t.down == MigrationTestDown::None) =>
            {
                "Its test ran up only: publishing needs one with a down migration (Test with a generated down)."
                    .to_owned()
            }
            Some(t) if t.status == MigrationTestStatus::NeedsAcknowledgement => {
                "Its test found data loss the migration does not declare: acknowledge it in the report, or change the migration."
                    .to_owned()
            }
            Some(t) if t.status == MigrationTestStatus::Passed && !t.rows_checked => {
                "Its test had nothing to check on: every table the migration changes was empty."
                    .to_owned()
            }
            Some(_) => {
                "Its last test did not pass: fix the migration, then test it again.".to_owned()
            }
        }
    } else {
        "Tests cannot run here (no playgrounds, or not on their Postgres major): an admin may publish without one, saying why.".to_owned()
    });
    Ok(gate)
}

/// An admin publishing without the passing test the organization requires
/// while tests cannot run, saying why: kept in the audit log with the
/// version, in its transaction ([`record_override`]).
#[derive(Debug, Clone)]
pub struct TestOverride {
    pub org_id: Uuid,
    pub draft: Uuid,
    pub draft_name: Option<String>,
    pub reason: String,
    pub actor_id: Uuid,
    pub actor_name: String,
}

/// The publish gate: open ([`TestGate::open`]), or an admin's override while
/// tests cannot run (`override_reason`, kept with the version: the
/// override returned), or 409 `passing_test_required` / `tests_unavailable`
/// saying why. A draft's text, its base version and its renames are all
/// in what the test must have run: renames switched or an update onto
/// another base since the test, and it no longer counts.
pub async fn gate(
    state: &AppState,
    member: &Member<min::Developer>,
    database: &DatabaseRef,
    draft: (Uuid, Option<String>),
    content: &crate::collab::room::Content,
    model: &remus_core::Schema,
    override_reason: Option<&str>,
) -> ApiResult<Option<TestOverride>> {
    let required = member.org.require_passing_test;
    let found = gate_state(state, database, draft.0, content, model, required).await?;
    if found.open() {
        return Ok(None);
    }
    let reason = found.reason.unwrap_or_default();
    if !found.available {
        let given = override_reason.map(str::trim).filter(|r| !r.is_empty());
        if let (Role::Admin, Some(given)) = (member.role, given) {
            return Ok(Some(TestOverride {
                org_id: member.org.id,
                draft: draft.0,
                draft_name: draft.1,
                reason: given.chars().take(1000).collect(),
                actor_id: member.user.id,
                actor_name: member.user.name.clone(),
            }));
        }
        return Err(ApiError::Conflict {
            code: "tests_unavailable",
            message: reason,
        });
    }
    Err(ApiError::Conflict {
        code: "passing_test_required",
        message: reason,
    })
}

/// In the transaction storing version `number` of `database_id`: the
/// override, in the audit log.
pub async fn record_override(
    tx: &mut sqlx::PgConnection,
    found: &TestOverride,
    database_id: Uuid,
    number: i32,
) -> ApiResult<()> {
    crate::audit::record(
        tx,
        &crate::audit::Entry {
            org_id: found.org_id,
            database_id: Some(database_id),
            action: AuditAction::PassingTestOverridden,
            actor_id: Some(found.actor_id),
            actor_name: Some(found.actor_name.clone()),
            version_number: Some(number),
            reason: Some(found.reason.clone()),
            details: crate::audit::AuditDetails {
                draft_id: Some(found.draft),
                draft_name: found.draft_name.clone(),
                ..Default::default()
            },
        },
    )
    .await?;
    Ok(())
}

/// Acknowledges the data loss a test found that the migration does not
/// declare (`needs_acknowledgement`): the test then counts as passed (an
/// up-only one as passed up only: never enough for the publish gate), and
/// the audit log keeps who did, with the test. Developers and admins; where
/// the organization requires a passing test, not the draft's own author or
/// editors (someone else looks at the loss), unless an admin.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/migration-tests/{id}/acknowledge",
    operation_id = "acknowledge_migration_test",
    tag = "migration_tests",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    responses(
        (status = 200, body = MigrationTest),
        (status = 403, description = "`own_draft`: the draft's author or an editor, where the organization requires a passing test (someone else, or an admin)", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`nothing_to_acknowledge`: it does not need it", body = ErrorBody),
    )
)]
async fn acknowledge(
    member: Member<min::Developer>,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
) -> ApiResult<Json<MigrationTest>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state, &database, id).await?;
    if let Some(draft) = row.draft_id
        && member.org.require_passing_test
        && member.role != Role::Admin
    {
        // The room first: it writes a first edit's `draft_editors` row
        // behind it, so whoever just typed may not be there yet. A room not
        // loaded has written everything, which the database then says.
        let editing = state.collab.editors(draft).await.contains(&member.user.id);
        let own = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM drafts WHERE id = $1 AND created_by = $2
                   UNION ALL
                   SELECT 1 FROM draft_editors WHERE draft_id = $1 AND user_id = $2
               ) AS "own!""#,
            draft,
            member.user.id,
        )
        .fetch_one(&state.db)
        .await?;
        if editing || own {
            return Err(ApiError::Denied {
                code: "own_draft",
                message: "You wrote this draft: someone else acknowledges the data loss its test found, or an admin.".into(),
            });
        }
    }
    let mut tx = state.db.begin().await?;
    // Up only stays up only: acknowledged loss is not a down migration run.
    let done = sqlx::query!(
        "UPDATE migration_tests SET status = 'passed', acknowledged_by = $2, acknowledged_at = now(),
                verdict = CASE WHEN down = 'none' THEN 'passed_up_only'::migration_test_verdict ELSE verdict END
         WHERE id = $1 AND database_id = $3 AND status = 'needs_acknowledgement'",
        id,
        member.user.id,
        database.id,
    )
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() != 1 {
        return Err(ApiError::Conflict {
            code: "nothing_to_acknowledge",
            message: "This test has no data loss to acknowledge.".into(),
        });
    }
    crate::audit::record(
        &mut *tx,
        &crate::audit::Entry {
            org_id: member.org.id,
            database_id: Some(database.id),
            action: AuditAction::MigrationTestAcknowledged,
            actor_id: Some(member.user.id),
            actor_name: Some(member.user.name.clone()),
            version_number: None,
            reason: None,
            details: crate::audit::AuditDetails {
                draft_id: row.draft_id,
                draft_name: row.draft_name.clone(),
                test_id: Some(id),
                message: row
                    .report
                    .as_ref()
                    .and_then(|r| r.get("headline"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                ..Default::default()
            },
        },
    )
    .await?;
    tx.commit().await?;
    let row = find(&state, &database, id).await?;
    Ok(Json(row.into_test(&state, true)))
}
