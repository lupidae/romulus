//! The compiler: a pool of sandbox databases, and the compile itself.
//!
//! [`Compiler::start`] bootstraps the pool (see [`bootstrap`]) and loads the
//! [`Catalog`]. A compile then:
//!
//! 1. refuses text over [`SandboxConfig::max_sql_bytes`], parses it (all
//!    syntax errors at once) and checks it against the [`crate::allowlist`];
//! 2. waits for a free pool database (each runs one compile at a time, so
//!    identical table names in two drafts never block each other);
//! 3. runs the prepared script (see [`crate::prepare`]) in a transaction as
//!    the slot's role, runs remus's [`remus_core::INTROSPECT_SQL`] and rolls
//!    back; when Postgres rejects a stub's guessed types, corrects them from
//!    its error and tries again, up to [`SandboxConfig::max_stub_rounds`];
//! 4. restores the policy roles into the model and makes it canonical.
//!
//! The whole compile has a deadline ([`SandboxConfig::compile_timeout`]) on
//! top of the per-statement `statement_timeout`; past it, the slot's backend
//! is terminated and the slot reconnects. Results are cached by SHA-256 of
//! the text, so everyone in a draft costs one sandbox run.
//!
//! [`Compiler::compile`] runs one compile. [`Compiler::compile_for`] is for
//! drafts: requests are coalesced per key (a request waiting for its turn is
//! replaced by a newer one, and its caller gets [`Rejected::Superseded`]),
//! compiles of one key never overlap and start at most once per
//! [`SandboxConfig::min_interval`], and stub corrections learnt for a key are
//! reused by its next compile.

mod bootstrap;
mod session;

use std::{
    collections::{HashMap, VecDeque},
    str::FromStr,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use remus_core::Schema;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, postgres::PgConnectOptions};
use tokio::sync::{Semaphore, oneshot};

use self::{
    bootstrap::{Bootstrapped, hex},
    session::{AttemptError, Settings, Slot, SqlFailure},
};
use crate::{
    allowlist,
    catalog::Catalog,
    diagnostic::{Code, Diagnostic},
    parse::{ParsedSql, parse},
    prepare::{Injection, prepare},
    refine,
    stubs::{Overrides, Stub},
};

/// Fresh connections tried after losing one mid-compile (100 ms, then
/// doubling: about 6 s in all).
const CONNECTION_RETRIES: u32 = 6;

/// What the model's `database` field says until the caller sets it (the
/// sandbox database a compile ran in is meaningless outside it).
pub const MODEL_DATABASE: &str = "sandbox";

/// How to reach the sandbox and how hard to let compiles push it.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// A superuser connection URL to the sandbox cluster (it creates the
    /// roles and databases; compiles never use it).
    pub admin_url: String,
    /// Pool databases (and so concurrent compiles). Default 4.
    pub pool_size: usize,
    /// Names of the pool's roles and databases (`<prefix>_<n>`); one process
    /// per prefix. Default `romulus_sandbox`.
    pub prefix: String,
    /// Per statement. Default 5 s.
    pub statement_timeout: Duration,
    /// Per lock wait. Default 1 s.
    pub lock_timeout: Duration,
    /// The whole compile, stub rounds included. Default 20 s.
    pub compile_timeout: Duration,
    /// Waiting for a free pool database. Default 30 s.
    pub acquire_timeout: Duration,
    /// Larger texts are refused before parsing. Default 1 MiB.
    pub max_sql_bytes: usize,
    /// More statements are refused before running. Default 5 000.
    pub max_statements: usize,
    /// Between two compile starts of one key ([`Compiler::compile_for`]).
    /// Default 250 ms.
    pub min_interval: Duration,
    /// Retries after correcting a stub from Postgres's error. Default 4.
    pub max_stub_rounds: usize,
    /// Compile results kept by SHA-256 of their text. Default 256.
    pub cache_entries: usize,
    /// How often the admin connection (which holds the prefix lock) is
    /// checked, and re-established if lost. Default 10 s.
    pub admin_check_interval: Duration,
}

impl SandboxConfig {
    pub fn new(admin_url: impl Into<String>) -> Self {
        Self {
            admin_url: admin_url.into(),
            pool_size: 4,
            prefix: "romulus_sandbox".to_owned(),
            statement_timeout: Duration::from_secs(5),
            lock_timeout: Duration::from_secs(1),
            compile_timeout: Duration::from_secs(20),
            acquire_timeout: Duration::from_secs(30),
            max_sql_bytes: 1 << 20,
            max_statements: 5_000,
            min_interval: Duration::from_millis(250),
            max_stub_rounds: 4,
            cache_entries: 256,
            admin_check_interval: Duration::from_secs(10),
        }
    }
}

/// The sandbox could not compile at all (as opposed to the SQL being wrong,
/// which is a [`Compilation`] with errors).
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("the sandbox is unavailable: {0}")]
    Unavailable(String),
    #[error("sandbox setup failed: {0}")]
    Bootstrap(String),
    /// Another process holds the prefix: at start, or after this one lost
    /// its admin connection and could not take the prefix back (it then
    /// stops compiling for good; restart it once the other one is gone).
    #[error("another process already uses the sandbox prefix {0:?}")]
    PrefixInUse(String),
    #[error("the sandbox runs PostgreSQL {server}, but the parser speaks PostgreSQL {parser}")]
    VersionMismatch { server: String, parser: u32 },
    #[error("no sandbox database became free within {0:?}")]
    Busy(Duration),
}

/// Why [`Compiler::compile_for`] returned no compilation.
#[derive(Debug, thiserror::Error)]
pub enum Rejected {
    /// A newer request for the same key replaced this one before it ran;
    /// its result is the one to show.
    #[error("superseded by a newer compile of the same key")]
    Superseded,
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
}

/// The outcome of compiling a text: a model, or the errors that prevent one.
#[derive(Debug, Clone, Serialize)]
pub struct Compilation {
    /// The model, when the text compiled: canonical, its `database` set to
    /// [`MODEL_DATABASE`] (the caller sets the real name).
    pub model: Option<Schema>,
    /// Errors (when there is no model), then notes, each located in the
    /// text.
    pub diagnostics: Vec<Diagnostic>,
    /// Functions the text calls without defining them, stubbed to compile.
    pub stubs: Vec<Stub>,
    /// Schemas created because the text uses them without creating them.
    pub created_schemas: Vec<String>,
    /// Statements in the text.
    pub statements: usize,
    /// Hex SHA-256 of the text.
    pub sha256: String,
    /// Time spent, sandbox and parsing included (the original compile's for
    /// a cached result).
    pub duration_ms: u64,
    /// Served from the cache.
    pub cached: bool,
}

impl Compilation {
    pub fn is_ok(&self) -> bool {
        self.model.is_some()
    }

    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter().filter(|d| d.is_error())
    }

    fn failed(sha256: String, statements: usize, diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            model: None,
            diagnostics,
            stubs: Vec::new(),
            created_schemas: Vec::new(),
            statements,
            sha256,
            duration_ms: 0,
            cached: false,
        }
    }
}

/// Counters since start, for metrics endpoints and logs.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Metrics {
    /// Compile requests, cache hits and superseded ones included.
    pub requests: u64,
    pub cache_hits: u64,
    /// Requests replaced by a newer one of the same key before running.
    pub superseded: u64,
    /// Compiles that reached the sandbox.
    pub sandbox_runs: u64,
    /// Attempts beyond the first, after correcting a stub.
    pub stub_retries: u64,
    pub succeeded: u64,
    /// Compiles that ended with errors in the SQL.
    pub failed: u64,
    pub timeouts: u64,
    /// Compiles that could not run (sandbox down, pool busy).
    pub unavailable: u64,
    /// Time in the sandbox, total and worst, in milliseconds.
    pub sandbox_ms_total: u64,
    pub sandbox_ms_max: u64,
    /// Time waiting for a pool database, total.
    pub wait_ms_total: u64,
    pub pool_size: usize,
    pub pool_free: usize,
}

#[derive(Default)]
struct Counters {
    requests: AtomicU64,
    cache_hits: AtomicU64,
    superseded: AtomicU64,
    sandbox_runs: AtomicU64,
    stub_retries: AtomicU64,
    succeeded: AtomicU64,
    failed: AtomicU64,
    timeouts: AtomicU64,
    unavailable: AtomicU64,
    sandbox_ms_total: AtomicU64,
    sandbox_ms_max: AtomicU64,
    wait_ms_total: AtomicU64,
}

fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

/// `20 s`, `1.5 s`, `150 ms`.
fn readable(duration: Duration) -> String {
    if duration < Duration::from_secs(1) {
        format!("{} ms", duration.as_millis())
    } else if duration.subsec_millis() == 0 {
        format!("{} s", duration.as_secs())
    } else {
        format!("{:.1} s", duration.as_secs_f64())
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Compiles SQL in the sandbox. Cheap to clone; clones share the pool.
#[derive(Clone)]
pub struct Compiler {
    inner: Arc<Inner>,
}

struct Inner {
    config: SandboxConfig,
    options: PgConnectOptions,
    catalog: Arc<Catalog>,
    server_version: String,
    /// Holds the prefix lock; terminates runaway backends. `None` while
    /// lost and not yet re-established.
    admin: tokio::sync::Mutex<Option<PgConnection>>,
    /// The prefix was taken by another process while the admin connection
    /// was lost: compiling would share its databases.
    prefix_lost: std::sync::atomic::AtomicBool,
    slots: Mutex<Vec<Slot>>,
    permits: Semaphore,
    keys: Mutex<HashMap<String, KeyState>>,
    cache: Mutex<Cache>,
    counters: Counters,
}

#[derive(Default)]
struct KeyState {
    driving: bool,
    pending: Option<Pending>,
    last_start: Option<Instant>,
    learnt: Overrides,
}

struct Pending {
    sql: String,
    reply: oneshot::Sender<Result<Compilation, Rejected>>,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<String, Compilation>,
    order: VecDeque<String>,
}

/// Std mutexes guard plain data with no await inside; a panic elsewhere
/// cannot leave it half-written, so poisoning is ignored.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Compiler {
    /// Bootstrap the pool and connect every slot. Fails if the sandbox is
    /// unreachable, its major version differs from the parser's, or another
    /// process holds the prefix.
    pub async fn start(config: SandboxConfig) -> Result<Self, SandboxError> {
        let options = PgConnectOptions::from_str(&config.admin_url)
            .map_err(|e| SandboxError::Bootstrap(format!("admin_url: {e}")))?;
        let Bootstrapped {
            admin,
            slots,
            catalog,
            server_version,
        } = bootstrap::bootstrap(&config, &options).await?;
        let mut connected = Vec::with_capacity(slots.len());
        for spec in slots {
            let mut slot = Slot::new(spec);
            slot.connection(&options)
                .await
                .map_err(|e| SandboxError::Bootstrap(format!("slot connection: {e}")))?;
            connected.push(slot);
        }
        tracing::info!(
            pool = connected.len(),
            prefix = %config.prefix,
            server = %server_version,
            extensions = catalog.extensions().count(),
            "sandbox compiler ready"
        );
        let compiler = Self {
            inner: Arc::new(Inner {
                permits: Semaphore::new(connected.len()),
                slots: Mutex::new(connected),
                admin: tokio::sync::Mutex::new(Some(admin)),
                prefix_lost: std::sync::atomic::AtomicBool::new(false),
                options,
                catalog: Arc::new(catalog),
                server_version,
                keys: Mutex::new(HashMap::new()),
                cache: Mutex::new(Cache::default()),
                counters: Counters::default(),
                config,
            }),
        };
        tokio::spawn(watch_admin(Arc::downgrade(&compiler.inner)));
        Ok(compiler)
    }

    /// Check the admin connection that holds the prefix lock; if it was
    /// lost (sandbox restart, a backend crash makes Postgres restart them
    /// all), reconnect and take the lock again. Runs every
    /// [`SandboxConfig::admin_check_interval`] on its own; call it from a
    /// health check too. `PrefixInUse` means another process took the
    /// prefix meanwhile: this compiler refuses to compile from then on.
    pub async fn check_admin(&self) -> Result<(), SandboxError> {
        self.inner.check_admin().await
    }

    /// What the sandbox provides (built-ins, extensions).
    pub fn catalog(&self) -> &Catalog {
        &self.inner.catalog
    }

    /// The sandbox's `server_version`.
    pub fn server_version(&self) -> &str {
        &self.inner.server_version
    }

    pub fn config(&self) -> &SandboxConfig {
        &self.inner.config
    }

    pub fn metrics(&self) -> Metrics {
        let c = &self.inner.counters;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        Metrics {
            requests: get(&c.requests),
            cache_hits: get(&c.cache_hits),
            superseded: get(&c.superseded),
            sandbox_runs: get(&c.sandbox_runs),
            stub_retries: get(&c.stub_retries),
            succeeded: get(&c.succeeded),
            failed: get(&c.failed),
            timeouts: get(&c.timeouts),
            unavailable: get(&c.unavailable),
            sandbox_ms_total: get(&c.sandbox_ms_total),
            sandbox_ms_max: get(&c.sandbox_ms_max),
            wait_ms_total: get(&c.wait_ms_total),
            pool_size: self.inner.config.pool_size,
            pool_free: self.inner.permits.available_permits(),
        }
    }

    /// Compile one text. See the module documentation.
    pub async fn compile(&self, sql: &str) -> Result<Compilation, SandboxError> {
        bump(&self.inner.counters.requests);
        let mut overrides = Overrides::default();
        self.inner.compile(sql, &mut overrides).await
    }

    /// Compile a draft's text: coalesced per `key`, rate-limited, never two
    /// at once for one key. See the module documentation.
    pub async fn compile_for(&self, key: &str, sql: String) -> Result<Compilation, Rejected> {
        bump(&self.inner.counters.requests);
        let (reply, result) = oneshot::channel();
        let drive = {
            let mut keys = lock(&self.inner.keys);
            let state = keys.entry(key.to_owned()).or_default();
            if let Some(replaced) = state.pending.replace(Pending { sql, reply }) {
                bump(&self.inner.counters.superseded);
                let _ = replaced.reply.send(Err(Rejected::Superseded));
            }
            !std::mem::replace(&mut state.driving, true)
        };
        if drive {
            tokio::spawn(Inner::drive(self.inner.clone(), key.to_owned()));
        }
        result.await.unwrap_or(Err(Rejected::Superseded))
    }

    /// Close every connection, releasing the prefix.
    pub async fn close(&self) {
        let slots: Vec<Slot> = std::mem::take(&mut *lock(&self.inner.slots));
        for mut slot in slots {
            slot.close().await;
        }
        self.inner.permits.close();
        if let Some(admin) = self.inner.admin.lock().await.take() {
            // Closing the session releases the prefix lock.
            let _ = admin.close().await;
        }
    }
}

impl Inner {
    /// One key's compiles, one after the other, newest request first.
    async fn drive(self: Arc<Self>, key: String) {
        loop {
            let wait = {
                let keys = lock(&self.keys);
                keys.get(&key).and_then(|s| s.last_start).map(|t| {
                    (t + self.config.min_interval).saturating_duration_since(Instant::now())
                })
            };
            if let Some(wait) = wait.filter(|w| !w.is_zero()) {
                tokio::time::sleep(wait).await;
            }
            let (job, mut learnt) = {
                let mut keys = lock(&self.keys);
                let Some(state) = keys.get_mut(&key) else {
                    return;
                };
                let Some(job) = state.pending.take() else {
                    state.driving = false;
                    return;
                };
                state.last_start = Some(Instant::now());
                (job, state.learnt.clone())
            };
            let result = self.compile(&job.sql, &mut learnt).await;
            {
                let mut keys = lock(&self.keys);
                if let Some(state) = keys.get_mut(&key) {
                    state.learnt = learnt;
                }
                Self::forget_idle_keys(&mut keys);
            }
            let _ = job.reply.send(result.map_err(Rejected::Sandbox));
        }
    }

    /// Keep the per-key state from growing without bound: drop keys idle
    /// for ten minutes once there are many.
    fn forget_idle_keys(keys: &mut HashMap<String, KeyState>) {
        if keys.len() <= 4_096 {
            return;
        }
        let horizon = Duration::from_secs(600);
        keys.retain(|_, s| {
            s.driving || s.pending.is_some() || s.last_start.is_some_and(|t| t.elapsed() < horizon)
        });
    }

    async fn compile(
        &self,
        sql: &str,
        overrides: &mut Overrides,
    ) -> Result<Compilation, SandboxError> {
        if self.prefix_lost.load(Ordering::Relaxed) {
            return Err(SandboxError::PrefixInUse(self.config.prefix.clone()));
        }
        let started = Instant::now();
        let sha256 = hex(&Sha256::digest(sql.as_bytes()));
        if let Some(mut hit) = lock(&self.cache).entries.get(&sha256).cloned() {
            bump(&self.counters.cache_hits);
            hit.cached = true;
            return Ok(hit);
        }

        let mut compilation = self.compile_uncached(sql, sha256, overrides).await?;
        compilation.duration_ms = millis(started.elapsed());
        let transient = compilation
            .diagnostics
            .iter()
            .any(|d| matches!(d.code, Code::Timeout | Code::Unavailable));
        if compilation.is_ok() {
            bump(&self.counters.succeeded);
        } else if transient {
            bump(&self.counters.timeouts);
        } else {
            bump(&self.counters.failed);
        }
        if !transient && self.config.cache_entries > 0 {
            let mut cache = lock(&self.cache);
            if !cache.entries.contains_key(&compilation.sha256) {
                cache.order.push_back(compilation.sha256.clone());
                cache
                    .entries
                    .insert(compilation.sha256.clone(), compilation.clone());
                while cache.order.len() > self.config.cache_entries {
                    if let Some(old) = cache.order.pop_front() {
                        cache.entries.remove(&old);
                    }
                }
            }
        }
        tracing::info!(
            sha256 = %&compilation.sha256[..12],
            statements = compilation.statements,
            stubs = compilation.stubs.len(),
            ok = compilation.is_ok(),
            errors = compilation.errors().count(),
            ms = compilation.duration_ms,
            "sandbox compile"
        );
        Ok(compilation)
    }

    async fn compile_uncached(
        &self,
        sql: &str,
        sha256: String,
        overrides: &mut Overrides,
    ) -> Result<Compilation, SandboxError> {
        if sql.len() > self.config.max_sql_bytes {
            let diagnostic = Diagnostic::error(
                Code::TooLarge,
                format!(
                    "the SQL is {} KiB; the limit is {} KiB",
                    sql.len().div_ceil(1024),
                    self.config.max_sql_bytes / 1024
                ),
            )
            .with_hint("Split the schema, or drop data and generated noise from it.");
            return Ok(Compilation::failed(sha256, 0, vec![diagnostic]));
        }

        // Parsing a large text takes a few milliseconds of CPU: off the
        // async workers.
        let text = sql.to_owned();
        let catalog = Arc::clone(&self.catalog);
        let (parsed, problems) = tokio::task::spawn_blocking(move || {
            let parsed = parse(&text);
            let problems = if parsed.errors.is_empty() {
                allowlist::check(&parsed, &text, &catalog)
            } else {
                Vec::new()
            };
            (parsed, problems)
        })
        .await
        .map_err(|e| SandboxError::Unavailable(format!("parser task failed: {e}")))?;
        let statements = parsed.statements.len();
        if !parsed.errors.is_empty() {
            return Ok(Compilation::failed(sha256, statements, parsed.errors));
        }
        if !problems.is_empty() {
            return Ok(Compilation::failed(sha256, statements, problems));
        }
        if statements > self.config.max_statements {
            let diagnostic = Diagnostic::error(
                Code::TooManyStatements,
                format!(
                    "the SQL has {statements} statements; the limit is {}",
                    self.config.max_statements
                ),
            );
            return Ok(Compilation::failed(sha256, statements, vec![diagnostic]));
        }

        let waited = Instant::now();
        let permit = tokio::time::timeout(self.config.acquire_timeout, self.permits.acquire())
            .await
            .map_err(|_| {
                bump(&self.counters.unavailable);
                SandboxError::Busy(self.config.acquire_timeout)
            })?
            .map_err(|_| SandboxError::Unavailable("the compiler is closed".into()))?;
        self.counters
            .wait_ms_total
            .fetch_add(millis(waited.elapsed()), Ordering::Relaxed);
        let mut lease = Lease {
            inner: self,
            slot: lock(&self.slots).pop(),
        };
        let Some(slot) = lease.slot.as_mut() else {
            return Err(SandboxError::Unavailable("no pool database left".into()));
        };

        bump(&self.counters.sandbox_runs);
        let ran = Instant::now();
        let outcome = self.run(slot, sql, &parsed, overrides).await;
        let elapsed = millis(ran.elapsed());
        self.counters
            .sandbox_ms_total
            .fetch_add(elapsed, Ordering::Relaxed);
        self.counters
            .sandbox_ms_max
            .fetch_max(elapsed, Ordering::Relaxed);
        drop(lease);
        drop(permit);

        let mut compilation = match outcome {
            Ok(compilation) => compilation,
            Err(err) => {
                bump(&self.counters.unavailable);
                return Err(err);
            }
        };
        compilation.sha256 = sha256;
        compilation.statements = statements;
        Ok(compilation)
    }

    /// Run the attempts on a slot, under the compile deadline.
    async fn run(
        &self,
        slot: &mut Slot,
        sql: &str,
        parsed: &ParsedSql,
        overrides: &mut Overrides,
    ) -> Result<Compilation, SandboxError> {
        let pid = slot.pid;
        let mut work = Box::pin(self.attempts(slot, sql, parsed, overrides));
        tokio::select! {
            result = &mut work => return result,
            () = tokio::time::sleep(self.config.compile_timeout) => {}
        }
        // Past the deadline: stop the backend, so the attempt fails on its
        // connection, and give it a moment to notice.
        self.terminate(pid).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), &mut work).await;
        drop(work);
        slot.discard();
        let diagnostic = Diagnostic::error(
            Code::Timeout,
            format!(
                "the compile took longer than {} and was stopped",
                readable(self.config.compile_timeout)
            ),
        )
        .with_hint("Nothing in a schema should take this long: look for an expensive default, index or view.");
        Ok(Compilation::failed(String::new(), 0, vec![diagnostic]))
    }

    async fn terminate(&self, pid: Option<i32>) {
        let Some(pid) = pid else { return };
        let _ = self.check_admin().await;
        let mut admin = self.admin.lock().await;
        let Some(admin) = admin.as_mut() else {
            tracing::warn!(pid, "no admin connection to terminate a sandbox backend");
            return;
        };
        let terminated = sqlx::query("SELECT pg_terminate_backend($1)")
            .bind(pid)
            .execute(admin)
            .await;
        if let Err(err) = terminated {
            tracing::warn!(pid, error = %err, "could not terminate a sandbox backend");
        }
    }

    async fn check_admin(&self) -> Result<(), SandboxError> {
        let prefix = &self.config.prefix;
        if self.prefix_lost.load(Ordering::Relaxed) {
            return Err(SandboxError::PrefixInUse(prefix.clone()));
        }
        let mut admin = self.admin.lock().await;
        if let Some(conn) = admin.as_mut()
            && sqlx::query("SELECT 1").execute(conn).await.is_ok()
        {
            return Ok(());
        }
        // Lost: the lock went with the session.
        *admin = None;
        tracing::warn!(%prefix, "sandbox admin connection lost, reconnecting");
        let mut conn = bootstrap::connect_admin(&self.options, prefix)
            .await
            .map_err(|e| SandboxError::Unavailable(e.to_string()))?;
        let locked = bootstrap::lock_prefix(&mut conn, prefix)
            .await
            .map_err(|e| SandboxError::Unavailable(e.to_string()))?;
        if !locked {
            self.prefix_lost.store(true, Ordering::Relaxed);
            tracing::error!(%prefix, "another process took the sandbox prefix; this compiler stops");
            return Err(SandboxError::PrefixInUse(prefix.clone()));
        }
        tracing::info!(%prefix, "sandbox admin connection and prefix lock re-established");
        *admin = Some(conn);
        Ok(())
    }

    async fn attempts(
        &self,
        slot: &mut Slot,
        sql: &str,
        parsed: &ParsedSql,
        overrides: &mut Overrides,
    ) -> Result<Compilation, SandboxError> {
        let settings = Settings {
            statement_timeout_ms: self.config.statement_timeout.as_millis(),
            lock_timeout_ms: self.config.lock_timeout.as_millis(),
        };
        let mut round = 0;
        let mut connection_retries = 0;
        loop {
            let prepared = prepare(sql, parsed, &self.catalog, overrides);
            match slot.attempt(&self.options, &settings, &prepared).await {
                Ok((json, roles)) => {
                    let model = session::postprocess(&json, &roles, MODEL_DATABASE)
                        .map_err(|e| SandboxError::Unavailable(format!("unreadable model: {e}")))?;
                    return Ok(Compilation {
                        model: Some(model),
                        diagnostics: prepared.notes,
                        stubs: prepared.stubs,
                        created_schemas: prepared.created_schemas,
                        statements: 0,
                        sha256: String::new(),
                        duration_ms: 0,
                        cached: false,
                    });
                }
                Err(AttemptError::Sql(failure)) => {
                    // A stub Postgres could not create (its return type does
                    // not exist, say) is dropped: the user's statement then
                    // fails on its own, which is the error worth showing.
                    let failed_stub = failure
                        .failure
                        .position
                        .map(|p| prepared.locate(p, parsed))
                        .and_then(|l| match l.injection {
                            Some(Injection::Stub(k)) => prepared.stubs.get(k),
                            _ => None,
                        });
                    let learnt = match failed_stub {
                        Some(stub) => overrides.skip(stub),
                        None => refine::learn(
                            &failure.failure,
                            failure.statement,
                            &prepared,
                            parsed,
                            sql,
                            overrides,
                        ),
                    };
                    if round < self.config.max_stub_rounds && learnt {
                        round += 1;
                        bump(&self.counters.stub_retries);
                        continue;
                    }
                    return Ok(self.failure(&failure, &prepared, parsed, sql));
                }
                Err(AttemptError::Connection(err)) => {
                    slot.discard();
                    // The backend went away: terminated, killed by the OOM
                    // killer (the postmaster then restarts every backend
                    // and recovers for a moment), or the sandbox restarted.
                    // Try again on a fresh connection, backing off, for a
                    // few seconds; the deadline still applies.
                    if connection_retries < CONNECTION_RETRIES {
                        tokio::time::sleep(Duration::from_millis(100 << connection_retries)).await;
                        connection_retries += 1;
                        continue;
                    }
                    return Err(SandboxError::Unavailable(err.to_string()));
                }
            }
        }
    }

    fn failure(
        &self,
        failure: &SqlFailure,
        prepared: &crate::prepare::Prepared,
        parsed: &ParsedSql,
        sql: &str,
    ) -> Compilation {
        let mut diagnostics = vec![session::diagnostic(
            failure,
            Some(prepared),
            parsed,
            sql,
            &self.catalog,
        )];
        diagnostics.extend(prepared.notes.iter().cloned());
        Compilation {
            model: None,
            diagnostics,
            stubs: prepared.stubs.clone(),
            created_schemas: prepared.created_schemas.clone(),
            statements: 0,
            sha256: String::new(),
            duration_ms: 0,
            cached: false,
        }
    }
}

/// Check the admin connection periodically, for as long as the compiler
/// lives.
async fn watch_admin(inner: std::sync::Weak<Inner>) {
    loop {
        let Some(interval) = inner.upgrade().map(|i| i.config.admin_check_interval) else {
            return;
        };
        tokio::time::sleep(interval).await;
        let Some(inner) = inner.upgrade() else { return };
        if inner.permits.is_closed() {
            return;
        }
        if let Err(SandboxError::PrefixInUse(_)) = inner.check_admin().await {
            return;
        }
    }
}

/// A pool slot on loan; goes back to the pool when dropped.
struct Lease<'a> {
    inner: &'a Inner,
    slot: Option<Slot>,
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.take() {
            lock(&self.inner.slots).push(slot);
        }
    }
}
