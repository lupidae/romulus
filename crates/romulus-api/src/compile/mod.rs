//! Compiling SQL into a remus model: uploads, SQL pushes, drafts, imports.
//!
//! [`SqlCompiler`] is the one seam handlers call: [`SqlCompiler::compile`]
//! answers a [`Compiled`], the canonical model or [`Diagnostic`]s located in
//! the text. Only the compiler itself failing (`romulus_sandbox::SandboxError`)
//! is an error, a 503 `unavailable`, and nothing is ever stored from one.
//!
//! **One compiler per Postgres major.** Every call names the major
//! ([`PgMajor`]) of the database it compiles for: its sandbox runs that
//! major (docs/MVP.md, "Several Postgres majors"). Caches are per major
//! (each sandbox compiler keeps its own), and so are the draft keys of
//! `compile_for`. A major whose sandbox is not configured, or down, answers
//! 503 for its databases only. Its backends:
//!
//! - the sandbox compilers (`romulus-sandbox`: DDL allowlist, auto-stubs, a
//!   pool of sandbox databases, results cached by SHA-256), one per
//!   `Config::sandbox_urls` entry. [`SqlCompiler::sandbox`] starts each in
//!   the background and retries until its sandbox answers, so the API never
//!   waits on them at startup; compiles answer 503 until it is ready. Uploads
//!   use `Compiler::compile` (cancel-safe, at most `upload_concurrency` of
//!   the pool); the per-person rate limit is `rate::Limits::compile`;
//! - none: every compile answers 503 (no sandbox configured);
//! - a function of the major and the text, for tests that need a model
//!   without a sandbox.
//!
//! [`render_sql`] is the SQL of a model that came as remus JSON, and the
//! [`render`] functions its other formats.
//!
//! Drafts (milestone 4) compile with [`SqlCompiler::compile_for`] (coalesced
//! per draft; a superseded request answers [`Rejected::Superseded`]).

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock, PoisonError},
    time::{Duration, Instant},
};

use remus_core::emit::DiagramOptions;
pub use romulus_sandbox::{
    Code, Diagnostic, LineIndex, Position, Replay, ReplayEvent, ReplayFailure, Severity, Span,
};
use romulus_sandbox::{Compiler, SandboxConfig, SandboxError};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    ApiError, ApiResult, AppState, Config,
    auth::CurrentUser,
    domain::{PgMajor, Schema},
    error::{Chain, ErrorBody},
    extract::Json,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(majors))
}

/// The outcome of compiling a text: a model, or the errors that prevent one.
#[derive(Debug, Clone)]
pub struct Compiled {
    /// Canonical. Its `database` is the sandbox's: the caller sets the real
    /// name.
    pub model: Option<Schema>,
    /// Errors (when there is no model), then notes.
    pub diagnostics: Vec<Diagnostic>,
}

type CompileFn = dyn Fn(PgMajor, &str) -> Compiled + Send + Sync;

#[derive(Clone)]
enum Backend {
    None,
    /// The configured majors' compilers.
    Sandbox(Arc<BTreeMap<PgMajor, Arc<Sandbox>>>),
    Function(Arc<CompileFn>),
}

struct Sandbox {
    major: PgMajor,
    /// Set once the background start succeeds.
    ready: OnceLock<Compiler>,
    /// The last health check, reused for [`HEALTH_TTL`].
    health: Mutex<Option<(Instant, CompilerStatus)>>,
}

/// `/api/health` asks the sandbox at most this often.
const HEALTH_TTL: Duration = Duration::from_secs(5);

/// Turns SQL into a model. Cheap to clone. See the module documentation.
#[derive(Clone)]
pub struct SqlCompiler(Backend);

/// What the health check reports about a major's compiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompilerStatus {
    Ok,
    /// Configured, not reachable yet (or any more).
    Unavailable,
    /// No sandbox configured.
    Off,
}

impl CompilerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Unavailable => "unavailable",
            Self::Off => "off",
        }
    }
}

/// A draft's text with conflict blocks from "Update draft" (or stray
/// markers) is never compiled: it is not what anyone means yet, and
/// refusing it is what keeps such a draft from being published. Only
/// drafts: an uploaded or pushed text is compiled whatever its comments say.
fn refused(sql: &str) -> Option<Compiled> {
    let diagnostics = romulus_sandbox::merge::conflict_diagnostics(sql);
    (!diagnostics.is_empty()).then_some(Compiled {
        model: None,
        diagnostics,
    })
}

/// Between start attempts: doubling from 1 s, at most this.
const MAX_RETRY: Duration = Duration::from_secs(30);

impl SqlCompiler {
    /// No compiler: every compile answers 503 `unavailable`.
    pub fn unavailable() -> Self {
        Self(Backend::None)
    }

    /// A compiler that is a plain function of the text, whatever the major
    /// (tests).
    pub fn from_fn(compile: impl Fn(&str) -> Compiled + Send + Sync + 'static) -> Self {
        Self(Backend::Function(Arc::new(move |_, sql: &str| {
            compile(sql)
        })))
    }

    /// A compiler that is a plain function of the major and the text
    /// (tests of what is compiled where).
    pub fn from_fn_per_major(
        compile: impl Fn(PgMajor, &str) -> Compiled + Send + Sync + 'static,
    ) -> Self {
        Self(Backend::Function(Arc::new(compile)))
    }

    /// The sandbox compilers for `config`, one per configured major (none
    /// without any), started in the background: see the module
    /// documentation. Call from within a Tokio runtime.
    pub fn sandbox(config: &Config) -> Self {
        if config.sandbox_urls.is_empty() {
            return Self::unavailable();
        }
        let mut sandboxes = BTreeMap::new();
        for (&major, url) in &config.sandbox_urls {
            let mut sandbox = SandboxConfig::new(url.clone());
            sandbox.major = Some(major.number());
            sandbox.prefix.clone_from(&config.sandbox_prefix);
            let state = Arc::new(Sandbox {
                major,
                ready: OnceLock::new(),
                health: Mutex::new(None),
            });
            tokio::spawn(start(sandbox, Arc::clone(&state)));
            sandboxes.insert(major, state);
        }
        Self(Backend::Sandbox(Arc::new(sandboxes)))
    }

    /// Whether this server compiles `major` at all: it has a sandbox for it
    /// (up or not). Without any sandbox (or with a test function), every
    /// major is alike: yes.
    pub fn serves(&self, major: PgMajor) -> bool {
        match &self.0 {
            Backend::Sandbox(sandboxes) => sandboxes.contains_key(&major),
            Backend::None | Backend::Function(_) => true,
        }
    }

    /// 422 `major_unavailable` unless this server compiles `major`
    /// ([`SqlCompiler::serves`]): creating a database on it, or moving one
    /// there, would leave it unable to compile.
    pub fn require_served(&self, major: PgMajor) -> ApiResult<()> {
        if self.serves(major) {
            return Ok(());
        }
        Err(ApiError::Unprocessable {
            code: "major_unavailable",
            message: not_served(major),
            details: None,
        })
    }

    /// The major's sandbox compiler, once it is ready.
    fn ready(&self, major: PgMajor) -> ApiResult<&Compiler> {
        let Backend::Sandbox(sandboxes) = &self.0 else {
            return Err(unavailable(major));
        };
        let Some(sandbox) = sandboxes.get(&major) else {
            return Err(ApiError::Unavailable(not_served(major)));
        };
        sandbox.ready.get().ok_or_else(|| {
            ApiError::Unavailable(format!(
                "The Postgres {major} compiler is starting, or its sandbox is down. Try again in a moment."
            ))
        })
    }

    pub async fn compile(&self, major: PgMajor, sql: &str) -> ApiResult<Compiled> {
        match &self.0 {
            Backend::None => Err(unavailable(major)),
            Backend::Sandbox(_) => {
                let compiler = self.ready(major)?;
                let compilation = compiler.compile(sql).await.map_err(|err| {
                    tracing::warn!(error = %Chain(&err), %major, "sandbox compile failed");
                    match err {
                        SandboxError::Busy(_) => ApiError::Unavailable(format!(
                            "The Postgres {major} compiler is busy. Try again in a moment."
                        )),
                        _ => unavailable(major),
                    }
                })?;
                Ok(Compiled {
                    model: compilation.model,
                    diagnostics: compilation.diagnostics,
                })
            }
            Backend::Function(compile) => {
                let (compile, sql) = (Arc::clone(compile), sql.to_owned());
                let mut compiled = tokio::task::spawn_blocking(move || compile(major, &sql))
                    .await
                    .map_err(ApiError::internal)?;
                compiled.model = compiled.model.map(Schema::canonical);
                Ok(compiled)
            }
        }
    }

    /// [`SqlCompiler::compile`] of a draft's text: refused while it has
    /// conflict blocks (publishing, Tidy).
    pub async fn compile_draft(&self, major: PgMajor, sql: &str) -> ApiResult<Compiled> {
        match refused(sql) {
            Some(refused) => Ok(refused),
            None => self.compile(major, sql).await,
        }
    }

    /// A draft's compile: coalesced per `key` (the draft id) and major,
    /// never two at once for one key, rate-limited per key by the sandbox. A
    /// request still waiting when a newer one for the same key arrives
    /// answers [`Rejected::Superseded`]. Without a sandbox (tests), a plain
    /// compile.
    pub async fn compile_for(
        &self,
        major: PgMajor,
        key: &str,
        sql: String,
    ) -> Result<Compiled, Rejected> {
        if let Some(refused) = refused(&sql) {
            return Ok(refused);
        }
        self.coalesced(major, key, sql).await
    }

    /// A draft in migration mode: its base version's SQL (`base`, empty
    /// without a base version), then its `migration`, compiled as
    /// [`SqlCompiler::compile_for`] does (coalesced per `key`), with the
    /// diagnostics located in `migration`
    /// ([`romulus_sandbox::draft_migration`]). Refused while the migration
    /// has conflict blocks, like a draft's SQL.
    pub async fn compile_migration_for(
        &self,
        major: PgMajor,
        key: &str,
        base: &str,
        migration: &str,
    ) -> Result<Compiled, Rejected> {
        if let Some(refused) = refused(migration) {
            return Ok(refused);
        }
        let script = romulus_sandbox::draft_migration::script(base, migration);
        let compiled = self.coalesced(major, key, script.text.clone()).await?;
        Ok(Compiled {
            model: compiled.model,
            diagnostics: script.relocate(migration, compiled.diagnostics),
        })
    }

    /// [`SqlCompiler::compile_migration_for`], not coalesced: publishing,
    /// updating and switching modes compile what they hold, once.
    pub async fn compile_migration(
        &self,
        major: PgMajor,
        base: &str,
        migration: &str,
    ) -> ApiResult<Compiled> {
        if let Some(refused) = refused(migration) {
            return Ok(refused);
        }
        let script = romulus_sandbox::draft_migration::script(base, migration);
        let compiled = self.compile(major, &script.text).await?;
        Ok(Compiled {
            model: compiled.model,
            diagnostics: script.relocate(migration, compiled.diagnostics),
        })
    }

    async fn coalesced(
        &self,
        major: PgMajor,
        key: &str,
        sql: String,
    ) -> Result<Compiled, Rejected> {
        let Backend::Sandbox(_) = &self.0 else {
            return Ok(self.compile(major, &sql).await?);
        };
        let compiler = self.ready(major)?;
        match compiler.compile_for(key, sql).await {
            Ok(compilation) => Ok(Compiled {
                model: compilation.model,
                diagnostics: compilation.diagnostics,
            }),
            Err(romulus_sandbox::Rejected::Superseded) => Err(Rejected::Superseded),
            Err(romulus_sandbox::Rejected::Sandbox(err)) => {
                tracing::warn!(error = %Chain(&err), key, %major, "sandbox draft compile failed");
                Err(Rejected::Api(unavailable(major)))
            }
        }
    }

    /// A migration import's replay (`romulus_sandbox::Compiler::replay`):
    /// the migrations from an empty database, a model after each one from
    /// `from` on, sent on `events` one at a time (the replay waits while
    /// the receiver is busy, and stops if it goes away). Without a sandbox
    /// (tests), the function compiles each migration's replayed text alone
    /// (its errors) and the texts so far joined (the model).
    pub async fn replay(
        &self,
        major: PgMajor,
        migrations: Vec<String>,
        from: usize,
        events: tokio::sync::mpsc::Sender<ReplayEvent>,
    ) -> ApiResult<Replay> {
        match &self.0 {
            Backend::None => Err(unavailable(major)),
            Backend::Sandbox(_) => {
                let compiler = self.ready(major)?;
                compiler
                    .replay(migrations, from, events)
                    .await
                    .map_err(|err| match err {
                        SandboxError::Abandoned => abandoned(),
                        SandboxError::Busy(_) => {
                            tracing::warn!(error = %Chain(&err), %major, "sandbox import replay failed");
                            ApiError::Unavailable(format!(
                                "The Postgres {major} compiler is busy with another import. Try again in a minute."
                            ))
                        }
                        _ => {
                            tracing::warn!(error = %Chain(&err), %major, "sandbox import replay failed");
                            unavailable(major)
                        }
                    })
            }
            Backend::Function(compile) => {
                let compile = Arc::clone(compile);
                tokio::task::spawn_blocking(move || {
                    replay_with(&|sql: &str| compile(major, sql), &migrations, from, &events)
                })
                .await
                .map_err(ApiError::internal)?
            }
        }
    }

    /// For `/api/health`: whether `major`'s compiles can run now, asked of
    /// its sandbox at most every [`HEALTH_TTL`]. A sandbox compiler that
    /// lost its prefix to another process (`PrefixInUse`) has stopped
    /// compiling for good and says so here.
    pub async fn status(&self, major: PgMajor) -> CompilerStatus {
        let sandbox = match &self.0 {
            Backend::None => return CompilerStatus::Off,
            Backend::Function(_) => return CompilerStatus::Ok,
            Backend::Sandbox(sandboxes) => match sandboxes.get(&major) {
                Some(sandbox) => sandbox,
                None => return CompilerStatus::Off,
            },
        };
        let Some(compiler) = sandbox.ready.get() else {
            return CompilerStatus::Unavailable;
        };
        let lock = || {
            sandbox
                .health
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
        };
        if let Some(status) = fresh(*lock(), Instant::now()) {
            return status;
        }
        let status = match compiler.check_admin().await {
            Ok(()) => CompilerStatus::Ok,
            Err(err) => {
                tracing::warn!(error = %Chain(&err), %major, "health: the sandbox compiler is down");
                CompilerStatus::Unavailable
            }
        };
        *lock() = Some((Instant::now(), status));
        status
    }

    /// Every supported major's status, oldest first, with its sandbox's
    /// `server_version` once it answered.
    pub async fn statuses(&self) -> Vec<(PgMajor, CompilerStatus, Option<String>)> {
        let mut out = Vec::with_capacity(PgMajor::SUPPORTED.len());
        for major in PgMajor::SUPPORTED {
            let status = self.status(major).await;
            let server = match &self.0 {
                Backend::Sandbox(sandboxes) => sandboxes
                    .get(&major)
                    .and_then(|s| s.ready.get())
                    .map(|c| c.server_version().to_owned()),
                Backend::Function(_) => Some(format!("{major}.0")),
                Backend::None => None,
            };
            out.push((major, status, server));
        }
        out
    }

    /// Releases every sandbox pool (and its prefix), on shutdown.
    pub async fn close(&self) {
        if let Backend::Sandbox(sandboxes) = &self.0 {
            for sandbox in sandboxes.values() {
                if let Some(compiler) = sandbox.ready.get() {
                    compiler.close().await;
                }
            }
        }
    }
}

/// [`SqlCompiler::replay`] with a function for a compiler (tests).
fn replay_with(
    compile: &dyn Fn(&str) -> Compiled,
    migrations: &[String],
    from: usize,
    events: &tokio::sync::mpsc::Sender<ReplayEvent>,
) -> ApiResult<Replay> {
    let analyses: Vec<_> = migrations
        .iter()
        .map(|m| romulus_sandbox::import::analyze(m))
        .collect();
    let skipped: Vec<_> = analyses.iter().map(|a| a.skipped.clone()).collect();
    let failed = |index: usize, diagnostics: Vec<Diagnostic>| Replay {
        skipped: skipped.clone(),
        notes: vec![Vec::new(); migrations.len()],
        stubs: Vec::new(),
        failure: Some(ReplayFailure { index, diagnostics }),
        duration_ms: 0,
    };
    // Like the sandbox: everything checked first, then a model at a time.
    for (k, analysis) in analyses.iter().enumerate() {
        if !analysis.errors.is_empty() {
            return Ok(failed(k, analysis.errors.clone()));
        }
        let alone = compile(&analysis.sql);
        if alone.model.is_none() && !analysis.sql.trim().is_empty() {
            return Ok(failed(k, alone.diagnostics));
        }
    }
    events
        .blocking_send(ReplayEvent::Checked)
        .map_err(|_| abandoned())?;
    for k in from..analyses.len() {
        let so_far: Vec<&str> = analyses[..=k].iter().map(|a| a.sql.as_str()).collect();
        let Some(model) = compile(&so_far.join("\n;\n")).model else {
            return Ok(failed(k, Vec::new()));
        };
        let event = ReplayEvent::Replayed {
            index: k,
            model: Box::new(model.canonical()),
        };
        events.blocking_send(event).map_err(|_| abandoned())?;
    }
    Ok(Replay {
        notes: vec![Vec::new(); migrations.len()],
        skipped,
        stubs: Vec::new(),
        failure: None,
        duration_ms: 0,
    })
}

/// The replay stopped because its receiver went away: whatever made it go
/// away is the answer, this only stands in for it.
fn abandoned() -> ApiError {
    ApiError::Internal("the import stopped receiving models".into())
}

/// Why [`SqlCompiler::compile_for`] returned no compilation.
#[derive(Debug, thiserror::Error)]
pub enum Rejected {
    /// A newer compile of the same key replaced this one before it ran: its
    /// result is the one to show, drop this request.
    #[error("superseded by a newer compile of the same key")]
    Superseded,
    /// The compiler is unavailable (503), as for [`SqlCompiler::compile`].
    #[error(transparent)]
    Api(#[from] ApiError),
}

/// A cached health status still young enough to answer with.
fn fresh(cached: Option<(Instant, CompilerStatus)>, now: Instant) -> Option<CompilerStatus> {
    cached
        .filter(|(at, _)| now.saturating_duration_since(*at) < HEALTH_TTL)
        .map(|(_, status)| status)
}

/// What a major this server has no sandbox for is told.
fn not_served(major: PgMajor) -> String {
    format!(
        "This server does not compile Postgres {major}: it has no sandbox for it. Use another version, or ask whoever runs romulus to add it."
    )
}

fn unavailable(major: PgMajor) -> ApiError {
    ApiError::Unavailable(format!(
        "The Postgres {major} compiler is not available right now. Try again in a minute."
    ))
}

/// Starts a major's compiler, retrying while its sandbox is not there yet
/// (or another process holds the prefix); a sandbox on another major is a
/// configuration error, for good.
async fn start(config: SandboxConfig, sandbox: Arc<Sandbox>) {
    let major = sandbox.major;
    let mut wait = Duration::from_secs(1);
    loop {
        match Compiler::start(config.clone()).await {
            Ok(compiler) => {
                tracing::info!(
                    %major,
                    prefix = %config.prefix,
                    server = compiler.server_version(),
                    "sandbox compiler ready"
                );
                let _ = sandbox.ready.set(compiler);
                return;
            }
            Err(
                err
                @ (SandboxError::VersionMismatch { .. } | SandboxError::UnsupportedMajor { .. }),
            ) => {
                tracing::error!(error = %Chain(&err), %major, "sandbox compiler disabled: check its URL");
                return;
            }
            Err(err) => {
                // Said once; a major whose sandbox is not started (dev
                // starts them on demand) would fill the log otherwise.
                if wait == Duration::from_secs(1) {
                    tracing::warn!(error = %Chain(&err), %major, "sandbox compiler not ready: retrying until it answers");
                } else {
                    tracing::debug!(error = %Chain(&err), %major, retry_in = ?wait, "sandbox compiler still not ready");
                }
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(MAX_RETRY);
            }
        }
    }
}

/// A Postgres major romulus compiles on, as this server runs it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PostgresMajor {
    pub major: PgMajor,
    /// What a new database targets.
    pub default: bool,
    /// Whether its compiles run now: `ok`, `unavailable` (starting, or its
    /// sandbox is down) or `off` (this server has no sandbox for it).
    pub status: CompilerStatus,
    /// The exact version compiles run on (`17.11 (Debian …)`), once its
    /// sandbox answered.
    pub server_version: Option<String>,
}

/// The Postgres majors a database can target, and whether this server
/// compiles each one now (the major picker, the settings).
#[utoipa::path(
    get,
    path = "/postgres-majors",
    operation_id = "list_postgres_majors",
    tag = "databases",
    responses(
        (status = 200, body = Vec<PostgresMajor>),
        (status = 401, body = ErrorBody),
    )
)]
async fn majors(
    _user: CurrentUser,
    axum::extract::State(compiler): axum::extract::State<SqlCompiler>,
) -> Json<Vec<PostgresMajor>> {
    let statuses = tokio::time::timeout(Duration::from_secs(2), compiler.statuses())
        .await
        .unwrap_or_else(|_| {
            PgMajor::SUPPORTED
                .into_iter()
                .map(|major| (major, CompilerStatus::Unavailable, None))
                .collect()
        });
    Json(
        statuses
            .into_iter()
            .map(|(major, status, server_version)| PostgresMajor {
                major,
                default: major == PgMajor::DEFAULT,
                status,
                server_version,
            })
            .collect(),
    )
}

/// The SQL of a model, for versions that came as remus JSON: what their
/// SQL tab shows and what a draft started from them holds. remus-sql,
/// reordered where it would not compile back (`romulus_sandbox::render_sql`).
pub fn render_sql(schema: &Schema) -> String {
    romulus_sandbox::render_sql(schema)
}

/// The other formats of a model, as remus emits them.
pub mod render {
    use super::{DiagramOptions, Schema};

    /// Pretty-printed remus JSON, as `remus -f json` prints it.
    pub fn json(schema: &Schema) -> String {
        remus_core::emit::json(schema).unwrap_or_else(|err| {
            // Serialising plain data cannot fail; keep the page usable anyway.
            tracing::error!(error = %err, "rendering a model as JSON failed");
            String::new()
        })
    }

    pub fn dbml(schema: &Schema) -> String {
        remus_dbml::render(schema, &DiagramOptions::default())
    }

    pub fn mermaid(schema: &Schema) -> String {
        remus_mermaid::render(schema, &DiagramOptions::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_health_status_is_reused_for_a_few_seconds() {
        let at = Instant::now();
        let cached = Some((at, CompilerStatus::Unavailable));
        assert_eq!(
            fresh(cached, at + Duration::from_secs(1)),
            Some(CompilerStatus::Unavailable)
        );
        assert_eq!(fresh(cached, at + HEALTH_TTL), None);
        assert_eq!(fresh(None, at), None);
    }
}
