//! The playgrounds this host holds: which run, who is in them, what each is
//! doing. One lifecycle operation at a time per playground (build, reset,
//! migration, stop, destroy): a second one is refused at once (`busy`),
//! never queued behind the first; destroy cancels a build or a migration in
//! progress. Console sessions come and go around them, paused while one
//! runs.
//!
//! Caps the service enforces, because only it sees them: playgrounds
//! running per organization and per host (starting one past a cap stops the
//! least recently used one with no statement running, or is refused),
//! statements running at once across the host (a queue:
//! [`Host::statements`]), and the idle stop ([`Host::reap`]): idle means no
//! statement for [`Config::idle`], whatever tabs are open. A stopped
//! playground's sessions hear why, and their next run starts it again.
//!
//! Roles changed from inside (`ALTER ROLE play_read PASSWORD …` or
//! `SET …`, which Postgres lets any role do to itself) are undone by a
//! restart: the entrypoint puts every role's password and settings back on
//! each start ([`Host::repair`], at most once per [`REPAIR_EVERY`] a
//! playground: a restart pauses everyone in it).

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use romulus_play_runner::{Runner, RunnerError, password, protocol::Endpoint};
use tokio::{
    sync::{OwnedMutexGuard, Semaphore, broadcast, watch},
    task::AbortHandle,
};
use uuid::Uuid;

use super::{
    pg::{Conn, PgError, Target, socket_dir},
    seed::Seeder,
};
use crate::protocol::{Listed, Listing, Status};

/// The service's configuration, from its environment only.
#[derive(Debug, Clone)]
pub struct Config {
    /// `ROMULUS_PLAY_ADDR`: where it listens (the private link).
    pub addr: std::net::SocketAddr,
    /// `ROMULUS_PLAY_KEY`: the API's signing key (at least 32 characters).
    pub key: Vec<u8>,
    /// `ROMULUS_PLAY_SECRET`: the playgrounds' passwords derive from it (the
    /// runner holds the same).
    pub secret: Vec<u8>,
    /// `ROMULUS_PLAY_RUNNER_SOCKET`.
    pub runner_socket: PathBuf,
    /// `ROMULUS_PLAY_SOCKETS`: the playground volume's `run/`, as mounted
    /// here (read-only): `<sockets>/<id>/.s.PGSQL.5432`.
    pub sockets: PathBuf,
    /// `ROMULUS_PLAY_MAX_RUNNING` (3): playgrounds running at once.
    pub max_running: usize,
    /// `ROMULUS_PLAY_MAX_RUNNING_PER_ORG` (2).
    pub max_running_per_org: usize,
    /// `ROMULUS_PLAY_STATEMENTS` (default 4): statements running at
    /// once, across playgrounds.
    pub statements: usize,
    /// `ROMULUS_PLAY_IDLE_MINUTES` (15): stopped after this long without a
    /// statement.
    pub idle: Duration,
    /// How long a console statement may run: read access (30 s), develop
    /// access (2 min). Tests shorten them.
    pub read_deadline: Duration,
    pub develop_deadline: Duration,
    /// How long a whole migration run may take (10 min).
    pub migration_deadline: Duration,
}

#[derive(Debug, thiserror::Error)]
#[error("{var}: {reason}")]
pub struct ConfigError {
    pub var: &'static str,
    pub reason: String,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
    }

    pub fn from_lookup(get: impl Fn(&'static str) -> Option<String>) -> Result<Self, ConfigError> {
        let err = |var, reason: &str| ConfigError {
            var,
            reason: reason.to_owned(),
        };
        let required = |var| get(var).ok_or_else(|| err(var, "not set"));
        let secret = |var| {
            let value = required(var)?;
            if value.len() < 32 {
                return Err(err(var, "at least 32 characters"));
            }
            Ok(value.into_bytes())
        };
        let number = |var, default: usize| match get(var) {
            None => Ok(default),
            Some(v) => v
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| err(var, "a positive number")),
        };
        Ok(Self {
            addr: required("ROMULUS_PLAY_ADDR")?
                .parse()
                .map_err(|_| err("ROMULUS_PLAY_ADDR", "host:port"))?,
            key: secret("ROMULUS_PLAY_KEY")?,
            secret: secret("ROMULUS_PLAY_SECRET")?,
            runner_socket: PathBuf::from(required("ROMULUS_PLAY_RUNNER_SOCKET")?),
            sockets: PathBuf::from(
                get("ROMULUS_PLAY_SOCKETS").unwrap_or_else(|| "/run/play".to_owned()),
            ),
            max_running: number("ROMULUS_PLAY_MAX_RUNNING", 3)?,
            max_running_per_org: number("ROMULUS_PLAY_MAX_RUNNING_PER_ORG", 2)?,
            statements: number("ROMULUS_PLAY_STATEMENTS", 4)?,
            idle: Duration::from_secs(60 * number("ROMULUS_PLAY_IDLE_MINUTES", 15)? as u64),
            read_deadline: Duration::from_secs(crate::protocol::limits::READ_DEADLINE_SECS),
            develop_deadline: Duration::from_secs(crate::protocol::limits::DEVELOP_DEADLINE_SECS),
            migration_deadline: Duration::from_secs(crate::protocol::limits::MIGRATION_SECS),
        })
    }
}

/// What a session hears about its playground.
#[derive(Debug, Clone)]
pub enum Notice {
    /// Let go of the database: someone resets or migrates it, or it
    /// restarts. [`Notice::Resume`] follows.
    Pause(String),
    /// It is back.
    Resume,
    /// It was stopped (idle, room for another): let go; the next run starts
    /// it again.
    Stop(String),
    /// It is being deleted: let go ([`Notice::Resume`] if that fails).
    Deleting,
}

/// A playground restarts for a changed role at most this often.
pub const REPAIR_EVERY: Duration = Duration::from_mins(1);

/// The settings each role has after a (re)start (`play-entrypoint.sh`):
/// anything else was set from inside, and a restart undoes it.
const ROLE_SETTINGS: &[(&str, &[&str])] = &[
    ("play_owner", &["idle_in_transaction_session_timeout=10min"]),
    (
        "play_read",
        &[
            "default_transaction_read_only=on",
            "idle_in_transaction_session_timeout=10min",
        ],
    ),
    ("play_service", &["search_path=pg_catalog"]),
];

/// `pg_db_role_setting` as the service reads it: (role, database-specific,
/// sorted settings).
const SETTINGS: &str = "SELECT coalesce(r.rolname::text, ''), (s.setdatabase <> 0)::text, \
     pg_catalog.array_to_string(s.setconfig, ',') \
     FROM pg_catalog.pg_db_role_setting s \
     LEFT JOIN pg_catalog.pg_roles r ON r.oid = s.setrole";

/// Whether `rows` (from [`SETTINGS`]) are exactly what a start leaves.
fn settings_untouched(rows: &[Vec<Option<String>>]) -> bool {
    let mut seen: Vec<(String, Vec<String>)> = Vec::new();
    for row in rows {
        let role = row.first().cloned().flatten().unwrap_or_default();
        let per_database = row.get(1).cloned().flatten().as_deref() == Some("true");
        if per_database || role.is_empty() {
            return false;
        }
        let mut settings: Vec<String> = row
            .get(2)
            .cloned()
            .flatten()
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        settings.sort();
        seen.push((role, settings));
    }
    seen.sort();
    let mut expected: Vec<(String, Vec<String>)> = ROLE_SETTINGS
        .iter()
        .map(|(role, settings)| {
            let mut settings: Vec<String> = settings.iter().map(|s| (*s).to_owned()).collect();
            settings.sort();
            ((*role).to_owned(), settings)
        })
        .collect();
    expected.sort();
    seen == expected
}

/// One playground.
pub struct Entry {
    pub id: Uuid,
    org: Mutex<Option<Uuid>>,
    /// Held by lifecycle operations ([`Host::try_op`]).
    op: Arc<tokio::sync::Mutex<()>>,
    /// The build or migration task holding `op`: destroy cancels it.
    job: Mutex<Option<AbortHandle>>,
    busy: Mutex<Option<&'static str>>,
    target: Mutex<Option<Target>>,
    sessions: AtomicUsize,
    /// Statements running now (console and migration).
    statements: AtomicUsize,
    /// A restart did not put the roles back (a container made from an
    /// older image, whose start does not): never restarted for it again.
    repair_futile: std::sync::atomic::AtomicBool,
    /// The last restart for a changed role.
    last_repair: Mutex<Option<Instant>>,
    /// The last statement's start or end, or operation: idleness counts
    /// from it.
    last_used: Mutex<Instant>,
    pub notices: broadcast::Sender<Notice>,
    /// The service's own session (`play_service`): type names, terminating
    /// statements past their deadline.
    control: tokio::sync::Mutex<Option<Conn>>,
    types: Mutex<HashMap<(u32, i32), (String, bool)>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Entry {
    /// Takes `org` as its organization unless it is known to be another's
    /// (false then): a playground never changes hands.
    pub fn same_org(&self, org: Option<Uuid>) -> bool {
        let mut owner = lock(&self.org);
        match (*owner, org) {
            (Some(theirs), Some(ours)) if theirs != ours => false,
            (None, Some(ours)) => {
                *owner = Some(ours);
                true
            }
            _ => true,
        }
    }

    fn new(id: Uuid) -> Self {
        Self {
            id,
            org: Mutex::new(None),
            op: Arc::new(tokio::sync::Mutex::new(())),
            job: Mutex::new(None),
            busy: Mutex::new(None),
            target: Mutex::new(None),
            sessions: AtomicUsize::new(0),
            statements: AtomicUsize::new(0),
            repair_futile: std::sync::atomic::AtomicBool::new(false),
            last_repair: Mutex::new(None),
            last_used: Mutex::new(Instant::now()),
            notices: broadcast::channel(16).0,
            control: tokio::sync::Mutex::new(None),
            types: Mutex::new(HashMap::new()),
        }
    }

    pub fn target(&self) -> Option<Target> {
        lock(&self.target).clone()
    }

    pub fn touch(&self) {
        *lock(&self.last_used) = Instant::now();
    }

    pub fn busy(&self) -> Option<&'static str> {
        *lock(&self.busy)
    }

    pub fn sessions(&self) -> usize {
        self.sessions.load(Ordering::Relaxed)
    }

    /// Statements running now.
    pub fn running_statements(&self) -> usize {
        self.statements.load(Ordering::Relaxed)
    }

    /// Unused for `idle`: no statement running, none for that long.
    pub fn idle_for(&self, idle: Duration) -> bool {
        self.running_statements() == 0 && lock(&self.last_used).elapsed() > idle
    }

    /// Counts a console session while the guard lives (sessions do not keep
    /// a playground running: statements do).
    pub fn session(self: &Arc<Self>) -> SessionGuard {
        self.sessions.fetch_add(1, Ordering::Relaxed);
        SessionGuard(Arc::clone(self))
    }

    /// Counts a statement while the guard lives: the playground is in use.
    pub fn statement(self: &Arc<Self>) -> StatementGuard {
        self.statements.fetch_add(1, Ordering::Relaxed);
        self.touch();
        StatementGuard(Arc::clone(self))
    }

    /// The task now holding the operation lock (build, migration), for
    /// destroy to cancel.
    pub fn set_job(&self, job: Option<AbortHandle>) {
        *lock(&self.job) = job;
    }

    /// Cached type names (`format_type`) and whether each is numeric.
    pub fn cached_types(&self, keys: &[(u32, i32)]) -> Option<Vec<(String, bool)>> {
        let types = lock(&self.types);
        keys.iter().map(|k| types.get(k).cloned()).collect()
    }

    pub fn cache_types(&self, entries: impl IntoIterator<Item = ((u32, i32), (String, bool))>) {
        lock(&self.types).extend(entries);
    }

    /// The control session, connected on first use.
    pub async fn control(
        &self,
        secret: &[u8],
    ) -> Result<tokio::sync::MutexGuard<'_, Option<Conn>>, PgError> {
        let mut control = self.control.lock().await;
        if control.is_none() {
            let target = self
                .target()
                .ok_or_else(|| PgError::Protocol("the playground is not running".into()))?;
            *control = Some(connect(&target, self.id, password::Role::Service, secret).await?);
        }
        Ok(control)
    }

    async fn forget_control(&self) {
        if let Some(conn) = self.control.lock().await.take() {
            conn.close().await;
        }
    }
}

pub struct SessionGuard(Arc<Entry>);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.sessions.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct StatementGuard(Arc<Entry>);

impl Drop for StatementGuard {
    fn drop(&mut self) {
        self.0.statements.fetch_sub(1, Ordering::Relaxed);
        self.0.touch();
    }
}

/// Marks an entry busy while it lives.
pub struct Busy<'a>(&'a Entry);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        *lock(&self.0.busy) = None;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("{0}")]
    Runner(#[from] RunnerError),
    #[error("{0}")]
    Pg(#[from] PgError),
    /// Another lifecycle operation runs (`building`, `migrating`…).
    #[error("the playground is busy {0}")]
    Busy(&'static str),
    /// It restarted for a changed role less than [`REPAIR_EVERY`] ago.
    #[error("it restarted a moment ago to undo a role changed from inside")]
    RepairedRecently,
    /// Every running playground of the organization is in use.
    #[error(
        "{0} playgrounds of this organization are in use right now: try again when one is free"
    )]
    OrgFull(usize),
    /// The host has no room.
    #[error("this server runs {0} playgrounds at most and all are in use: try again in a while")]
    HostFull(usize),
    #[error("the playground's database did not start: {0}")]
    NotStarting(String),
}

/// Connects to a playground's database as `role`.
pub async fn connect(
    target: &Target,
    id: Uuid,
    role: password::Role,
    secret: &[u8],
) -> Result<Conn, PgError> {
    Conn::connect(
        target,
        role.name(),
        &password::derive(secret, id, role),
        "playground",
        Duration::from_secs(10),
    )
    .await
}

/// Every playground this service knows of. Cheap to share (`Arc`).
pub struct Host {
    pub config: Config,
    pub runner: Runner,
    pub seeder: Arc<dyn Seeder>,
    entries: Mutex<HashMap<Uuid, Arc<Entry>>>,
    /// Statements running at once, host-wide.
    pub statements: Arc<Semaphore>,
    /// Starting playgrounds (and making room for them), one at a time.
    starting: tokio::sync::Mutex<()>,
    /// True once the service shuts down: sessions close.
    pub closing: watch::Sender<bool>,
}

impl Host {
    pub fn new(config: Config, runner: Runner, seeder: Arc<dyn Seeder>) -> Arc<Self> {
        Arc::new(Self {
            statements: Arc::new(Semaphore::new(config.statements)),
            config,
            runner,
            seeder,
            entries: Mutex::new(HashMap::new()),
            starting: tokio::sync::Mutex::new(()),
            closing: watch::channel(false).0,
        })
    }

    /// The playground's entry (made on first mention), noting its
    /// organization.
    pub fn entry(&self, id: Uuid, org: Option<Uuid>) -> Arc<Entry> {
        let entry = Arc::clone(
            lock(&self.entries)
                .entry(id)
                .or_insert_with(|| Arc::new(Entry::new(id))),
        );
        if let Some(org) = org {
            *lock(&entry.org) = Some(org);
        }
        entry
    }

    fn known(&self, id: Uuid) -> Option<Arc<Entry>> {
        lock(&self.entries).get(&id).cloned()
    }

    /// Marks `entry` busy with `what` (status, and the idle stop skips it).
    pub fn busy<'a>(entry: &'a Entry, what: &'static str) -> Busy<'a> {
        *lock(&entry.busy) = Some(what);
        Busy(entry)
    }

    /// The entry's operation lock, now, or [`HostError::Busy`] with what
    /// holds it: a second operation is refused, never queued (the API's
    /// calls answer within its timeout).
    pub fn try_op(entry: &Entry) -> Result<OwnedMutexGuard<()>, HostError> {
        Arc::clone(&entry.op)
            .try_lock_owned()
            .map_err(|_| HostError::Busy(entry.busy().unwrap_or("with another operation")))
    }

    /// Its database, started if needed (making room under the caps). Waits
    /// for an operation in progress (a session asking during a reset).
    pub async fn ensure_running(&self, entry: &Arc<Entry>) -> Result<Target, HostError> {
        if let Some(target) = entry.target() {
            return Ok(target);
        }
        let _op = Arc::clone(&entry.op).lock_owned().await;
        self.start_locked(entry).await
    }

    /// Waits until no operation runs on the playground (a restart, a
    /// reset): a session opening meanwhile connects to what it leaves.
    pub async fn settled(&self, entry: &Entry) {
        drop(Arc::clone(&entry.op).lock_owned().await);
    }

    /// Whether the roles are as a start left them (a role may change its
    /// own password or settings from inside: see [`Host::repair`]).
    pub async fn roles_untouched(&self, entry: &Entry) -> bool {
        if entry.repair_futile.load(Ordering::Relaxed) {
            return true;
        }
        let Ok(mut control) = entry.control(&self.config.secret).await else {
            return true;
        };
        let Some(conn) = control.as_mut() else {
            return true;
        };
        if let Ok(rows) = conn.query(SETTINGS).await {
            settings_untouched(&rows)
        } else {
            *control = None;
            true
        }
    }

    /// Restarts the playground to undo a role changed from inside (its
    /// password, its settings): the entrypoint puts them back on each
    /// start. Its sessions are paused meanwhile. `Busy` when an operation
    /// runs (it restarts the playground anyway).
    pub async fn repair(&self, entry: &Arc<Entry>) -> Result<Target, HostError> {
        let op = Self::try_op(entry)?;
        {
            let mut last = lock(&entry.last_repair);
            if last.is_some_and(|at| at.elapsed() < REPAIR_EVERY) {
                return Err(HostError::RepairedRecently);
            }
            *last = Some(Instant::now());
        }
        let busy = Self::busy(entry, "restarting");
        tracing::info!(playground = %entry.id, "a role was changed from inside: restarting");
        let _ = entry.notices.send(Notice::Pause(
            "a role was changed from inside the playground; it restarts to undo it".to_owned(),
        ));
        self.forget(entry).await;
        let restarted = async {
            self.runner.stop(entry.id).await?;
            self.start_locked(entry).await
        }
        .await;
        if restarted.is_ok() && !self.roles_untouched(entry).await {
            tracing::warn!(playground = %entry.id, "a restart did not put the roles back (an older image?): not restarting it for that again");
            entry.repair_futile.store(true, Ordering::Relaxed);
        }
        // Not busy any more before the others hear it is back: a session
        // opening then connects instead of waiting for a resume.
        drop(busy);
        drop(op);
        let _ = entry.notices.send(Notice::Resume);
        restarted
    }

    /// [`Host::ensure_running`] for a caller holding the operation lock.
    pub async fn start_locked(&self, entry: &Arc<Entry>) -> Result<Target, HostError> {
        if let Some(target) = entry.target() {
            return Ok(target);
        }
        let _starting = self.starting.lock().await;
        self.make_room(entry).await?;
        let started = self.runner.start(entry.id).await?;
        let target = match started.endpoint {
            Endpoint::Socket => Target::Socket(socket_dir(&self.config.sockets, entry.id)),
            Endpoint::Tcp { host, port } => Target::Tcp(host, port),
        };
        self.wait_ready(entry.id, &target).await?;
        *lock(&entry.target) = Some(target.clone());
        entry.touch();
        Ok(target)
    }

    /// Polls until the playground's Postgres accepts its service role.
    async fn wait_ready(&self, id: Uuid, target: &Target) -> Result<(), HostError> {
        // The entrypoint resets every role's password on each start: a
        // refused login can only be a start in progress.
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut checked = Instant::now();
        loop {
            match connect(target, id, password::Role::Service, &self.config.secret).await {
                Ok(conn) => {
                    conn.close().await;
                    return Ok(());
                }
                Err(err) if Instant::now() > deadline => {
                    return Err(HostError::NotStarting(err.to_string()));
                }
                Err(_) => {
                    if checked.elapsed() > Duration::from_secs(3) {
                        checked = Instant::now();
                        let state = self.runner.state(id).await?;
                        if !state.running {
                            return Err(HostError::NotStarting(if state.oom_killed {
                                "it ran out of memory".to_owned()
                            } else {
                                "it exited".to_owned()
                            }));
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    }

    fn running(&self) -> Vec<Arc<Entry>> {
        lock(&self.entries)
            .values()
            .filter(|e| e.target().is_some())
            .cloned()
            .collect()
    }

    /// Under the caps, or stops the least recently used playground with no
    /// statement or operation running to get there (of the same
    /// organization for its cap). Open tabs do not hold one: theirs hear
    /// why, and their next run starts it again.
    async fn make_room(&self, entry: &Arc<Entry>) -> Result<(), HostError> {
        let org = *lock(&entry.org);
        loop {
            let running: Vec<Arc<Entry>> = self
                .running()
                .into_iter()
                .filter(|e| e.id != entry.id)
                .collect();
            let same_org: Vec<Arc<Entry>> = running
                .iter()
                .filter(|e| org.is_some() && *lock(&e.org) == org)
                .cloned()
                .collect();
            let (pool, full) = if same_org.len() >= self.config.max_running_per_org {
                (
                    same_org,
                    HostError::OrgFull(self.config.max_running_per_org),
                )
            } else if running.len() >= self.config.max_running {
                (running, HostError::HostFull(self.config.max_running))
            } else {
                return Ok(());
            };
            let victim = pool
                .into_iter()
                .filter(|e| e.running_statements() == 0 && e.busy().is_none())
                .min_by_key(|e| *lock(&e.last_used));
            let Some(victim) = victim else {
                return Err(full);
            };
            tracing::info!(playground = %victim.id, for_ = %entry.id, "stopping the least recently used playground to make room");
            // Not its operation lock (the caller may hold another's; only
            // idle ones are picked).
            self.stop_entry(
                &victim,
                Notice::Stop("it was stopped to make room for another playground".to_owned()),
            )
            .await?;
        }
    }

    async fn stop_entry(&self, entry: &Arc<Entry>, notice: Notice) -> Result<(), HostError> {
        let _ = entry.notices.send(notice);
        self.forget(entry).await;
        self.runner.stop(entry.id).await?;
        Ok(())
    }

    /// Forgets that it runs (it stopped, or is about to): the next use
    /// starts it.
    pub async fn forget(&self, entry: &Entry) {
        entry.forget_control().await;
        *lock(&entry.target) = None;
    }

    /// Stops it (its sessions hear why; the next run starts it again).
    pub async fn stop(&self, entry: &Arc<Entry>, why: &str) -> Result<(), HostError> {
        let _op = Self::try_op(entry)?;
        let _busy = Self::busy(entry, "stopping");
        self.stop_entry(entry, Notice::Stop(why.to_owned())).await
    }

    /// Puts the generated rows back: stop, restore the copy, start.
    pub async fn reset_locked(&self, entry: &Arc<Entry>, why: &str) -> Result<Target, HostError> {
        let _ = entry.notices.send(Notice::Pause(why.to_owned()));
        self.forget(entry).await;
        self.runner.restore(entry.id).await?;
        self.start_locked(entry).await
    }

    /// Creates `into` as a copy of `source`'s rows (a migration test on
    /// someone's playground): `source` is stopped for the copy (its sessions
    /// paused, a couple of seconds), started again if it ran, and never
    /// changed. Refused at once while `source` is busy.
    pub async fn fork(
        &self,
        source: &Arc<Entry>,
        into: Uuid,
        fork: &romulus_play_runner::protocol::Fork,
    ) -> Result<(), HostError> {
        let op = Self::try_op(source)?;
        let busy = Self::busy(source, "copying");
        let was_running = source.target().is_some();
        let _ = source.notices.send(Notice::Pause(
            "it is being copied for a migration test (a few seconds)".to_owned(),
        ));
        self.forget(source).await;
        let forked = self.runner.fork(into, fork).await;
        if was_running && let Err(err) = self.start_locked(source).await {
            tracing::warn!(playground = %source.id, error = %err, "could not start a playground again after copying it");
        }
        // Not busy any more before its sessions hear it is back.
        drop(busy);
        drop(op);
        let _ = source.notices.send(Notice::Resume);
        forked.map_err(HostError::from)
    }

    /// The container and its files: gone. A build or a migration in
    /// progress is cancelled first; sessions are told.
    pub async fn destroy(&self, id: Uuid) -> Result<(), HostError> {
        let entry = self.entry(id, None);
        if let Some(job) = lock(&entry.job).take() {
            tracing::info!(playground = %id, "deleted: cancelling its build or migration");
            job.abort();
        }
        // An aborted task lets go of the lock at its next await point; a
        // reset or stop takes seconds.
        let _op = tokio::time::timeout(Duration::from_secs(60), Arc::clone(&entry.op).lock_owned())
            .await
            .map_err(|_| HostError::Busy(entry.busy().unwrap_or("with another operation")))?;
        // Nothing to start again: the API closes the consoles once the
        // deletion is done.
        let _ = entry.notices.send(Notice::Deleting);
        self.forget(&entry).await;
        if let Err(err) = self.runner.destroy(id).await {
            let _ = entry.notices.send(Notice::Resume);
            return Err(err.into());
        }
        lock(&self.entries).remove(&id);
        Ok(())
    }

    pub async fn status(&self, id: Uuid) -> Result<Status, HostError> {
        let entry = self.known(id);
        let state = self.runner.state(id).await?;
        Ok(Status {
            running: state.running && entry.as_ref().is_some_and(|e| e.target().is_some()),
            sessions: entry.as_ref().map_or(0, |e| e.sessions()),
            busy: entry.as_ref().and_then(|e| e.busy()).map(str::to_owned),
            oom_killed: state.oom_killed,
        })
    }

    pub async fn list(&self) -> Result<Listing, HostError> {
        let listing = self.runner.list().await?;
        Ok(Listing {
            playgrounds: listing
                .playgrounds
                .into_iter()
                .map(|p| Listed {
                    id: p.id,
                    running: p.running,
                })
                .collect(),
        })
    }

    /// At start: whatever runs was started by a previous process, whose
    /// sessions are gone. Stopped; each starts again on demand.
    pub async fn stop_strays(&self) {
        match self.runner.list().await {
            Ok(listing) => {
                for p in listing.playgrounds.into_iter().filter(|p| p.running) {
                    if self.known(p.id).is_none() {
                        tracing::info!(playground = %p.id, "stopping a playground left running");
                        let _ = self.runner.stop(p.id).await;
                    }
                }
            }
            Err(err) => tracing::warn!(error = %err, "cannot list playgrounds"),
        }
    }

    /// The idle stop: playgrounds running without a statement for
    /// [`Config::idle`], open tabs or not (theirs hear why; their next run
    /// starts it again). Run every minute.
    pub async fn reap(&self) {
        for entry in self.running() {
            if entry.idle_for(self.config.idle) && entry.busy().is_none() {
                let Ok(_op) = Self::try_op(&entry) else {
                    continue;
                };
                let minutes = self.config.idle.as_secs().div_ceil(60);
                tracing::info!(playground = %entry.id, "idle: stopping");
                let notice = Notice::Stop(format!(
                    "it was stopped after {minutes} minute{} without a query",
                    if minutes == 1 { "" } else { "s" }
                ));
                if let Err(err) = self.stop_entry(&entry, notice).await {
                    tracing::warn!(playground = %entry.id, error = %err, "idle stop failed");
                }
            }
        }
    }

    /// Terminates backend `pid` from the control session: a statement a
    /// cancel request did not stop.
    pub async fn terminate(&self, entry: &Entry, pid: i32) {
        match entry.control(&self.config.secret).await {
            Ok(mut control) => {
                if let Some(conn) = control.as_mut()
                    && conn
                        .query(&format!("SELECT pg_catalog.pg_terminate_backend({pid})"))
                        .await
                        .is_err()
                {
                    *control = None;
                }
            }
            Err(err) => tracing::warn!(playground = %entry.id, error = %err, "cannot terminate"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_has_safe_defaults_and_refuses_short_keys() {
        let env: HashMap<&str, &str> = [
            ("ROMULUS_PLAY_ADDR", "127.0.0.1:9000"),
            ("ROMULUS_PLAY_KEY", "k".repeat(32).leak() as &str),
            ("ROMULUS_PLAY_SECRET", "s".repeat(32).leak() as &str),
            ("ROMULUS_PLAY_RUNNER_SOCKET", "/run/runner/runner.sock"),
        ]
        .into_iter()
        .collect();
        let config = Config::from_lookup(|k| env.get(k).map(|v| (*v).to_owned())).unwrap();
        assert_eq!(config.max_running, 3);
        assert_eq!(config.max_running_per_org, 2);
        assert_eq!(config.idle, Duration::from_mins(15));
        assert_eq!(config.sockets, PathBuf::from("/run/play"));
        let short = Config::from_lookup(|k| {
            if k == "ROMULUS_PLAY_KEY" {
                Some("short".to_owned())
            } else {
                env.get(k).map(|v| (*v).to_owned())
            }
        });
        assert_eq!(short.unwrap_err().var, "ROMULUS_PLAY_KEY");
    }

    fn row(role: &str, per_database: bool, settings: &str) -> Vec<Option<String>> {
        vec![
            Some(role.to_owned()),
            Some(per_database.to_string()),
            Some(settings.to_owned()),
        ]
    }

    #[test]
    fn role_settings_must_be_exactly_what_a_start_leaves() {
        let clean = vec![
            row(
                "play_read",
                false,
                "idle_in_transaction_session_timeout=10min,default_transaction_read_only=on",
            ),
            row(
                "play_owner",
                false,
                "idle_in_transaction_session_timeout=10min",
            ),
            row("play_service", false, "search_path=pg_catalog"),
        ];
        assert!(settings_untouched(&clean));
        let mut changed = clean.clone();
        changed[0] = row(
            "play_read",
            false,
            "default_transaction_read_only=off,idle_in_transaction_session_timeout=10min",
        );
        assert!(!settings_untouched(&changed));
        let mut added = clean.clone();
        added.push(row("play_read", true, "statement_timeout=1"));
        assert!(!settings_untouched(&added));
        let mut database = clean.clone();
        database.push(row("", false, "work_mem=1GB"));
        assert!(!settings_untouched(&database));
        assert!(!settings_untouched(&clean[..2]));
    }
}
