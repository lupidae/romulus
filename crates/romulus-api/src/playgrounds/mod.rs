//! Playgrounds (docs/playground.md): a real Postgres holding a version's
//! schema (a draft's base, or a version of its own) filled with generated
//! rows, on the playground host. People query it in a read-only console;
//! developers run the draft's migration on its rows and reset them.
//!
//! The API owns what is durable and who may do what: the tables, the role
//! matrix, the caps on playgrounds existing, the shared history, the audit
//! log. The playground service (`romulus-playground`, on the host) builds
//! them and runs every statement; it trusts only calls this module signed
//! ([`host`]).
//!
//! Endpoints (who may call them in brackets; see `auth::member`):
//! - `GET /orgs/{org}/databases/{db}/playgrounds` [member]: the live ones,
//!   newest first.
//! - `POST /orgs/{org}/databases/{db}/playgrounds` [developer]: one for a
//!   draft (its base version) or a version of a database on Postgres
//!   [`MAJOR`] (422 `major_unsupported` otherwise), built in the background
//!   (`GET` it for the progress). One per draft, one per version; at most
//!   [`MAX_PER_ORG`] per organization, [`MAX_PER_DATABASE`] per database,
//!   [`MAX_PER_PERSON`] made by one person across organizations,
//!   [`MAX_TOTAL`] on the host (409 `playground_limit`,
//!   `playground_exists`), and none while the host's volume has less than
//!   the new one's quota plus [`DISK_RESERVE_BYTES`] free (503
//!   `playground_host_full`). Counted again inside the insert's
//!   transaction, under an advisory lock: concurrent creates cannot pass a
//!   cap together ([`build`]).
//! - `GET …/playgrounds/{id}` [member]; `DELETE` [developer].
//! - `POST …/playgrounds/{id}/reset` [developer]: the generated rows back.
//! - `POST …/playgrounds/{id}/migration` [developer]: the draft's migration
//!   on its rows, NDJSON progress ([`migration`]).
//! - `GET …/playgrounds/{id}/runs` [member]: the shared history ([`runs`]):
//!   the newest [`runs::CONSOLE_RUNS_KEPT`] console runs, every reset and
//!   migration run.
//! - `GET …/playgrounds/{id}/console` [member]: the console's websocket
//!   ([`console`]): runs rate limited per person, at most
//!   [`RUNS_PER_PERSON`] in flight per person.
//!
//! Lifetimes ([`sweeper`]): deleted after [`EXPIRES_AFTER_DAYS`] days unused,
//! or once its draft is published or deleted; the host stops it after 15
//! minutes without anyone (it starts again in seconds).
//!
//! Tables: `playgrounds`, `playground_runs`.

pub mod build;
pub mod console;
pub mod host;
pub mod migration;
pub mod runs;
pub mod sweeper;

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use romulus_playground::protocol::{Access, Stage, Status as Live, TableRows};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::{broadcast, watch};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use self::{
    host::{Caller, Host},
    runs::PlaygroundRun,
};
use crate::{
    ApiError, ApiResult, AppState,
    audit::{self, AuditDetails},
    auth::{Member, min},
    databases::{self, DatabaseRef},
    domain::{
        AuditAction, DraftMode, DraftStatus, PgMajor, PlaygroundProfile, PlaygroundSize,
        PlaygroundStatus, Role,
    },
    drafts::Person,
    error::ErrorBody,
    extract::{Json, Path},
};

/// Live playgrounds an organization may have.
pub const MAX_PER_ORG: i64 = 5;
/// Live playgrounds per database.
pub const MAX_PER_DATABASE: i64 = 3;
/// Live playgrounds one person may have made, across organizations (anyone
/// can sign up and make organizations).
pub const MAX_PER_PERSON: i64 = 3;
/// Live playgrounds on the host, all organizations together (it shares its
/// host with the app).
pub const MAX_TOTAL: i64 = 30;
/// Free space kept on the host's playground volume beyond a new
/// playground's own quota.
pub const DISK_RESERVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Console runs one person may have in flight, across tabs and playgrounds.
pub const RUNS_PER_PERSON: usize = 2;
/// The Postgres major playgrounds run (the playground image's,
/// deploy/playground; a test keeps them equal). A database on another one
/// gets none for now: its migration's locks and rewrites would be 17's.
pub const MAJOR: PgMajor = PgMajor::DEFAULT;
/// Deleted after this many days unused.
pub const EXPIRES_AFTER_DAYS: i64 = 7;
/// Console sessions one person may have open on one playground.
pub const SESSIONS_PER_PERSON: usize = 3;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get, delete))
        .routes(routes!(migration::reset))
        .routes(routes!(migration::run_migration))
        .routes(routes!(runs::list_runs))
        .routes(routes!(console::connect))
}

/// The access a role gets on a playground: reviewers read, developers and
/// admins develop (migration runs, longer statements).
pub fn access(role: Role) -> Access {
    if role.can_write() {
        Access::Develop
    } else {
        Access::Read
    }
}

/// What the people in a playground's console hear, besides their own
/// results.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConsoleNotice {
    /// Someone's run ended: the shared history's newest entry.
    History { run: PlaygroundRun },
    /// Who has the console open now.
    Presence { people: Vec<Person> },
    /// Someone reset the rows, ran the migration, or deleted the playground.
    Announcement { event: Announcement, by: Person },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Announcement {
    Resetting,
    Reset,
    Migrating,
    Migrated,
    Deleted,
}

/// Where a build is (while `building`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BuildProgress {
    pub stage: Stage,
    pub tables_done: usize,
    pub tables_total: usize,
    /// The table whose rows came in last.
    pub table: Option<String>,
}

/// One person's console tab.
struct Seat {
    user: Person,
}

#[derive(Default)]
struct Rooms {
    seats: HashMap<Uuid, Vec<(u64, Seat)>>,
    channels: HashMap<Uuid, broadcast::Sender<ConsoleNotice>>,
    progress: HashMap<Uuid, BuildProgress>,
    next: u64,
    /// Console runs in flight, per person.
    runs: HashMap<Uuid, usize>,
    /// People with a migration run in flight.
    migrating: HashSet<Uuid>,
}

/// Playgrounds as this API process sees them: the host, builds in
/// progress, who has each console open. Cheap to clone; in `AppState`.
#[derive(Clone)]
pub struct Playgrounds {
    pub host: Host,
    rooms: Arc<Mutex<Rooms>>,
    /// True once the API shuts down: consoles close (1012).
    closing: watch::Sender<bool>,
}

/// A console run in flight: counted against its person until dropped.
pub struct RunGuard {
    playgrounds: Playgrounds,
    user: Uuid,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let mut rooms = self.playgrounds.rooms();
        if let Some(n) = rooms.runs.get_mut(&self.user) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                rooms.runs.remove(&self.user);
            }
        }
    }
}

/// A migration run in flight: its person's one, until dropped.
pub struct MigrationGuard {
    playgrounds: Playgrounds,
    user: Uuid,
}

impl Drop for MigrationGuard {
    fn drop(&mut self) {
        self.playgrounds.rooms().migrating.remove(&self.user);
    }
}

/// A console seat: gone (and the others told) when dropped.
pub struct SeatGuard {
    playgrounds: Playgrounds,
    playground: Uuid,
    seat: u64,
}

impl Drop for SeatGuard {
    fn drop(&mut self) {
        let people = {
            let mut rooms = self.playgrounds.rooms();
            if let Some(seats) = rooms.seats.get_mut(&self.playground) {
                seats.retain(|(n, _)| *n != self.seat);
                if seats.is_empty() {
                    rooms.seats.remove(&self.playground);
                }
            }
            people(&rooms, self.playground)
        };
        self.playgrounds
            .tell(self.playground, ConsoleNotice::Presence { people });
    }
}

fn people(rooms: &Rooms, playground: Uuid) -> Vec<Person> {
    let mut people: Vec<Person> = Vec::new();
    for (_, seat) in rooms.seats.get(&playground).into_iter().flatten() {
        if !people.iter().any(|p| p.id == seat.user.id) {
            people.push(seat.user.clone());
        }
    }
    people
}

impl Playgrounds {
    pub fn new(host: Host) -> Self {
        Self {
            host,
            rooms: Arc::new(Mutex::new(Rooms::default())),
            closing: watch::channel(false).0,
        }
    }

    fn rooms(&self) -> std::sync::MutexGuard<'_, Rooms> {
        self.rooms.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Tells every console of `playground`.
    pub fn tell(&self, playground: Uuid, notice: ConsoleNotice) {
        if let Some(channel) = self.rooms().channels.get(&playground) {
            let _ = channel.send(notice);
        }
    }

    pub fn subscribe(&self, playground: Uuid) -> broadcast::Receiver<ConsoleNotice> {
        self.rooms()
            .channels
            .entry(playground)
            .or_insert_with(|| broadcast::channel(64).0)
            .subscribe()
    }

    /// Takes a console seat, unless `user` has [`SESSIONS_PER_PERSON`].
    pub fn sit(&self, playground: Uuid, user: Person) -> Option<SeatGuard> {
        let (seat, people) = {
            let mut rooms = self.rooms();
            let seats = rooms.seats.entry(playground).or_default();
            if seats.iter().filter(|(_, s)| s.user.id == user.id).count() >= SESSIONS_PER_PERSON {
                return None;
            }
            rooms.next += 1;
            let seat = rooms.next;
            rooms
                .seats
                .entry(playground)
                .or_default()
                .push((seat, Seat { user }));
            (seat, people(&rooms, playground))
        };
        self.tell(playground, ConsoleNotice::Presence { people });
        Some(SeatGuard {
            playgrounds: self.clone(),
            playground,
            seat,
        })
    }

    /// Who has `playground`'s console open.
    pub fn present(&self, playground: Uuid) -> Vec<Person> {
        people(&self.rooms(), playground)
    }

    /// Counts a console run of `user`'s, unless they have
    /// [`RUNS_PER_PERSON`] in flight.
    pub fn start_run(&self, user: Uuid) -> Option<RunGuard> {
        let mut rooms = self.rooms();
        let n = rooms.runs.entry(user).or_default();
        if *n >= RUNS_PER_PERSON {
            return None;
        }
        *n += 1;
        Some(RunGuard {
            playgrounds: self.clone(),
            user,
        })
    }

    /// `user`'s migration run, unless they have one in flight already.
    pub fn start_migration(&self, user: Uuid) -> Option<MigrationGuard> {
        self.rooms().migrating.insert(user).then(|| MigrationGuard {
            playgrounds: self.clone(),
            user,
        })
    }

    fn set_progress(&self, playground: Uuid, progress: Option<BuildProgress>) {
        let mut rooms = self.rooms();
        match progress {
            Some(progress) => rooms.progress.insert(playground, progress),
            None => rooms.progress.remove(&playground),
        };
    }

    fn progress(&self, playground: Uuid) -> Option<BuildProgress> {
        self.rooms().progress.get(&playground).cloned()
    }

    pub fn closing(&self) -> watch::Receiver<bool> {
        self.closing.subscribe()
    }

    /// Closes every console (graceful shutdown).
    pub fn shutdown(&self) {
        let _ = self.closing.send(true);
    }
}

/// A playground, as its page shows it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Playground {
    pub id: Uuid,
    pub status: PlaygroundStatus,
    /// The draft it was made for (it holds the draft's base version);
    /// `null` for a version's own.
    pub draft: Option<PlaygroundDraft>,
    /// The version whose schema it holds.
    pub version: i32,
    pub size: PlaygroundSize,
    pub profile: PlaygroundProfile,
    /// The seed, in hex: the same model, size, profile, seed and generator
    /// give the same rows.
    pub seed: String,
    /// The generator's version (rows differ between versions).
    pub generator: Option<String>,
    /// Rows per table once generated.
    pub tables: Vec<TableRows>,
    /// What the generator could not satisfy, as it wrote it.
    pub report: Option<Value>,
    /// Why the build failed.
    pub failure: Option<String>,
    /// While building.
    pub progress: Option<BuildProgress>,
    /// What its host says now; `null` when it is not ready or the host did
    /// not answer.
    pub live: Option<Live>,
    /// Who has its console open now.
    pub present: Vec<Person>,
    /// The migration run whose rows it holds; `null` while it holds the
    /// generated rows.
    pub migrated_by: Option<PlaygroundRun>,
    /// The last time a migration test copied its rows (it paused a moment
    /// for the copy), and who ran that test.
    pub copied: Option<PlaygroundCopied>,
    /// `null` once their account is gone.
    pub created_by: Option<Person>,
    pub created_at: DateTime<Utc>,
    pub ready_at: Option<DateTime<Utc>>,
    pub last_used_at: DateTime<Utc>,
    /// Deleted then unless used again.
    pub expires_at: DateTime<Utc>,
}

/// A migration test copying a playground's rows.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaygroundCopied {
    pub at: DateTime<Utc>,
    /// `null` once their account is gone.
    pub by: Option<Person>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaygroundDraft {
    pub id: Uuid,
    pub name: String,
    pub mode: DraftMode,
    pub status: DraftStatus,
    /// The draft is on another base now: its migration no longer starts from
    /// this playground's rows (create a new playground).
    pub moved: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreatePlayground {
    /// For this draft: it holds the draft's base version. Or:
    pub draft_id: Option<Uuid>,
    /// for this version (its number). The latest version when neither.
    pub version: Option<i32>,
    /// `small` unless given.
    #[serde(default)]
    pub size: Option<PlaygroundSize>,
    #[serde(default)]
    pub profile: PlaygroundProfile,
    /// Hex, at most 15 digits; random when absent.
    #[serde(default)]
    pub seed: Option<String>,
}

#[allow(clippy::struct_field_names, reason = "the columns' names")]
pub(crate) struct Row {
    pub id: Uuid,
    pub org_id: Uuid,
    pub database_id: Uuid,
    pub status: PlaygroundStatus,
    pub draft_id: Option<Uuid>,
    pub version_id: Uuid,
    pub version_number: i32,
    pub size: PlaygroundSize,
    pub profile: PlaygroundProfile,
    pub seed: i64,
    pub generator_version: Option<String>,
    pub row_counts: Value,
    pub report: Option<Value>,
    pub failure: Option<String>,
    pub migrated_by_run: Option<Uuid>,
    pub created_by: Option<Uuid>,
    pub creator_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub ready_at: Option<DateTime<Utc>>,
    pub last_used_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub draft_name: Option<String>,
    pub draft_mode: Option<DraftMode>,
    pub draft_status: Option<DraftStatus>,
    pub draft_base: Option<Uuid>,
}

/// The playground `id` of `database`, or 404.
pub(crate) async fn find(
    db: impl sqlx::PgExecutor<'_>,
    database: &DatabaseRef,
    id: Uuid,
) -> ApiResult<Row> {
    sqlx::query_as!(
        Row,
        r#"SELECT p.id, p.org_id, p.database_id, p.status AS "status: PlaygroundStatus", p.draft_id,
                  p.version_id, v.number AS version_number, p.size AS "size: PlaygroundSize",
                  p.profile AS "profile: PlaygroundProfile", p.seed, p.generator_version, p.row_counts,
                  p.report, p.failure, p.migrated_by_run, p.created_by, u.name AS "creator_name?",
                  p.created_at, p.ready_at, p.last_used_at, p.expires_at,
                  d.name AS "draft_name?", d.mode AS "draft_mode?: DraftMode",
                  d.status AS "draft_status?: DraftStatus", d.base_version_id AS draft_base
           FROM playgrounds p
           JOIN versions v ON v.id = p.version_id
           LEFT JOIN users u ON u.id = p.created_by
           LEFT JOIN drafts d ON d.id = p.draft_id
           WHERE p.id = $1 AND p.database_id = $2"#,
        id,
        database.id,
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| ApiError::not_found("No such playground. It may have been deleted."))
}

impl Row {
    fn seed_hex(&self) -> String {
        format!("{:06x}", self.seed)
    }

    pub(crate) fn caller(&self, member: &Member<impl min::MinRole>) -> Caller {
        Caller {
            org: self.org_id,
            user: Some((member.user.id, member.user.name.clone())),
            access: access(member.role),
        }
    }

    async fn into_playground(self, state: &AppState, live: Option<Live>) -> ApiResult<Playground> {
        let migrated_by = match self.migrated_by_run {
            Some(run) => runs::one(&state.db, run).await?,
            None => None,
        };
        let tables: Vec<TableRows> =
            serde_json::from_value(self.row_counts.clone()).unwrap_or_default();
        let copied = sqlx::query!(
            r#"SELECT t.started_at AS "at!", t.created_by, u.name AS "name?"
               FROM migration_tests t LEFT JOIN users u ON u.id = t.created_by
               WHERE t.data_playground_id = $1 AND t.started_at IS NOT NULL
               ORDER BY t.started_at DESC LIMIT 1"#,
            self.id,
        )
        .fetch_optional(&state.db)
        .await?
        .map(|c| PlaygroundCopied {
            at: c.at,
            by: c
                .created_by
                .zip(c.name)
                .map(|(id, name)| Person { id, name }),
        });
        Ok(Playground {
            id: self.id,
            status: self.status,
            draft: self
                .draft_id
                .zip(self.draft_name.clone())
                .map(|(id, name)| PlaygroundDraft {
                    id,
                    name,
                    mode: self.draft_mode.unwrap_or_default(),
                    status: self.draft_status.unwrap_or(DraftStatus::Open),
                    moved: self.draft_base != Some(self.version_id),
                }),
            version: self.version_number,
            size: self.size,
            profile: self.profile,
            seed: self.seed_hex(),
            generator: self.generator_version.clone(),
            tables,
            report: self.report.clone(),
            failure: self.failure.clone(),
            progress: if self.status == PlaygroundStatus::Building {
                state.playgrounds.progress(self.id)
            } else {
                None
            },
            live,
            present: state.playgrounds.present(self.id),
            migrated_by,
            copied,
            created_by: self
                .created_by
                .zip(self.creator_name.clone())
                .map(|(id, name)| Person { id, name }),
            created_at: self.created_at,
            ready_at: self.ready_at,
            last_used_at: self.last_used_at,
            expires_at: self.expires_at,
        })
    }
}

/// Marks it used now: it expires [`EXPIRES_AFTER_DAYS`] from now.
pub(crate) async fn touch(db: &PgPool, id: Uuid) -> ApiResult<()> {
    sqlx::query!(
        "UPDATE playgrounds SET last_used_at = now(), expires_at = now() + make_interval(days => $2)
         WHERE id = $1",
        id,
        i32::try_from(EXPIRES_AFTER_DAYS).unwrap_or(7),
    )
    .execute(db)
    .await?;
    Ok(())
}

/// The database's live playgrounds (building, ready, failed), newest first.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/playgrounds",
    operation_id = "list_playgrounds",
    tag = "playgrounds",
    params(("org" = String, Path), ("db" = String, Path)),
    responses(
        (status = 200, body = Vec<Playground>),
        (status = 404, body = ErrorBody),
    )
)]
async fn list(
    member: Member,
    Path((_, db_slug)): Path<(String, String)>,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<Playground>>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let ids = sqlx::query_scalar!(
        "SELECT id FROM playgrounds WHERE database_id = $1 AND status <> 'expired'
         ORDER BY created_at DESC",
        database.id,
    )
    .fetch_all(&state.db)
    .await?;
    let mut playgrounds = Vec::with_capacity(ids.len());
    for id in ids {
        let row = find(&state.db, &database, id).await?;
        playgrounds.push(row.into_playground(&state, None).await?);
    }
    Ok(Json(playgrounds))
}

/// Creates a playground for a draft (its base version) or a version, and
/// builds it in the background: `GET` it for the progress. Developers and
/// admins.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/playgrounds",
    operation_id = "create_playground",
    tag = "playgrounds",
    params(("org" = String, Path), ("db" = String, Path)),
    request_body = CreatePlayground,
    responses(
        (status = 201, body = Playground),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`playground_exists` (`details.id`: open it), `playground_limit`", body = ErrorBody),
        (status = 422, description = "`validation`; `no_version`: nothing to hold yet", body = ErrorBody),
        (status = 429, body = ErrorBody),
        (status = 503, description = "`host_unavailable`", body = ErrorBody),
    )
)]
async fn create(
    member: Member<min::Developer>,
    Path((_, db_slug)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<CreatePlayground>,
) -> ApiResult<(StatusCode, Json<Playground>)> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let id = build::create(&state, &member, &database, request).await?;
    let row = find(&state.db, &database, id).await?;
    Ok((
        StatusCode::CREATED,
        Json(row.into_playground(&state, None).await?),
    ))
}

/// One playground, with what its host says now.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/playgrounds/{id}",
    operation_id = "get_playground",
    tag = "playgrounds",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    responses(
        (status = 200, body = Playground),
        (status = 404, body = ErrorBody),
    )
)]
async fn get(
    member: Member,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
) -> ApiResult<Json<Playground>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    let live = if row.status == PlaygroundStatus::Ready {
        match state.playgrounds.host.backend() {
            Ok(backend) => tokio::time::timeout(
                Duration::from_secs(3),
                backend.status(id, row.caller(&member)),
            )
            .await
            .ok()
            .and_then(Result::ok),
            Err(_) => None,
        }
    } else {
        None
    };
    Ok(Json(row.into_playground(&state, live).await?))
}

/// Deletes a playground: its cluster on the host, its history. Developers
/// and admins.
#[utoipa::path(
    delete,
    path = "/orgs/{org}/databases/{db}/playgrounds/{id}",
    operation_id = "delete_playground",
    tag = "playgrounds",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    responses(
        (status = 204),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 503, description = "`host_unavailable`", body = ErrorBody),
    )
)]
async fn delete(
    member: Member<min::Developer>,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
) -> ApiResult<StatusCode> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    // A failed build may have left files (a cancelled one, a crash): the
    // host destroys whatever it holds. Expired ones were destroyed already.
    if row.status != PlaygroundStatus::Expired {
        match state.playgrounds.host.backend() {
            Ok(backend) => match backend.destroy(id, row.caller(&member)).await {
                Ok(()) => {}
                Err(err) if err.code() == Some("not_found") => {}
                Err(err) => return Err(err.into()),
            },
            // Nothing can be left of a failed one on a host that is off.
            Err(_) if row.status == PlaygroundStatus::Failed => {}
            Err(err) => return Err(err.into()),
        }
    }
    let mut tx = state.db.begin().await?;
    sqlx::query!("DELETE FROM playgrounds WHERE id = $1", id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        &audit::Entry {
            org_id: member.org.id,
            database_id: Some(database.id),
            action: AuditAction::PlaygroundDeleted,
            actor_id: Some(member.user.id),
            actor_name: Some(member.user.name.clone()),
            version_number: Some(row.version_number),
            reason: None,
            details: details(&row),
        },
    )
    .await?;
    tx.commit().await?;
    state.playgrounds.tell(
        id,
        ConsoleNotice::Announcement {
            event: Announcement::Deleted,
            by: Person {
                id: member.user.id,
                name: member.user.name.clone(),
            },
        },
    );
    Ok(StatusCode::NO_CONTENT)
}

/// A ready playground, or why not.
pub(crate) fn ready(row: &Row) -> ApiResult<()> {
    match row.status {
        PlaygroundStatus::Ready => Ok(()),
        PlaygroundStatus::Building => Err(ApiError::Conflict {
            code: "playground_not_ready",
            message: "The playground is still being built.".into(),
        }),
        PlaygroundStatus::Failed => Err(ApiError::Conflict {
            code: "playground_not_ready",
            message: "The playground's build failed: delete it and create another.".into(),
        }),
        PlaygroundStatus::Expired => Err(ApiError::Conflict {
            code: "playground_expired",
            message: "The playground expired: create a new one.".into(),
        }),
    }
}

/// What the audit log keeps about a playground.
pub(crate) fn details(row: &Row) -> AuditDetails {
    AuditDetails {
        playground_id: Some(row.id),
        draft_id: row.draft_id,
        draft_name: row.draft_name.clone(),
        ..AuditDetails::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playgrounds_run_the_major_of_their_image() {
        let dockerfile = include_str!("../../../../deploy/playground/Dockerfile");
        assert!(
            dockerfile.contains(&format!("FROM postgres:{MAJOR}.")),
            "deploy/playground/Dockerfile and playgrounds::MAJOR disagree"
        );
    }
}
