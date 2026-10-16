//! Compiling SQL into a remus model, for uploads and SQL pushes (drafts
//! later).
//!
//! [`SqlCompiler`] is the one seam handlers call: [`SqlCompiler::compile`]
//! answers a [`Compiled`], the canonical model or [`Diagnostic`]s located in
//! the text. Only the compiler itself failing (`romulus_sandbox::SandboxError`)
//! is an error, a 503 `unavailable`, and nothing is ever stored from one.
//! Its backends:
//!
//! - the sandbox compiler (`romulus-sandbox`: DDL allowlist, auto-stubs, a
//!   pool of sandbox databases, results cached by SHA-256), when
//!   `Config::sandbox_url` is set. [`SqlCompiler::sandbox`] starts it in the
//!   background and retries until the sandbox answers, so the API never
//!   waits on it at startup; compiles answer 503 until it is ready. Uploads
//!   use `Compiler::compile` (cancel-safe, at most `upload_concurrency` of
//!   the pool); the per-person rate limit is `rate::Limits::compile`;
//! - none: every compile answers 503 (no sandbox configured);
//! - a function, for tests that need a model without a sandbox.
//!
//! [`render_sql`] is the SQL of a model that came as remus JSON, and the
//! [`render`] functions its other formats.
//!
//! Drafts (milestone 4) compile with [`SqlCompiler::compile_for`] (coalesced
//! per draft; a superseded request answers [`Rejected::Superseded`]).

use std::{
    sync::{Arc, Mutex, OnceLock, PoisonError},
    time::{Duration, Instant},
};

use remus_core::emit::DiagramOptions;
pub use romulus_sandbox::{
    Code, Diagnostic, LineIndex, Position, Replay, ReplayEvent, ReplayFailure, Severity, Span,
};
use romulus_sandbox::{Compiler, SandboxConfig, SandboxError};
use utoipa_axum::router::OpenApiRouter;

use crate::{ApiError, ApiResult, AppState, Config, domain::Schema, error::Chain};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
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

type CompileFn = dyn Fn(&str) -> Compiled + Send + Sync;

#[derive(Clone)]
enum Backend {
    None,
    Sandbox(Arc<Sandbox>),
    Function(Arc<CompileFn>),
}

struct Sandbox {
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

/// What the health check reports about the compiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    /// A compiler that is a plain function (tests).
    pub fn from_fn(compile: impl Fn(&str) -> Compiled + Send + Sync + 'static) -> Self {
        Self(Backend::Function(Arc::new(compile)))
    }

    /// The sandbox compiler for `config` (none without `sandbox_url`),
    /// started in the background: see the module documentation. Call from
    /// within a Tokio runtime.
    pub fn sandbox(config: &Config) -> Self {
        let Some(url) = config.sandbox_url.clone() else {
            return Self::unavailable();
        };
        let mut sandbox = SandboxConfig::new(url);
        sandbox.prefix.clone_from(&config.sandbox_prefix);
        let state = Arc::new(Sandbox {
            ready: OnceLock::new(),
            health: Mutex::new(None),
        });
        tokio::spawn(start(sandbox, Arc::clone(&state)));
        Self(Backend::Sandbox(state))
    }

    pub async fn compile(&self, sql: &str) -> ApiResult<Compiled> {
        match &self.0 {
            Backend::None => Err(unavailable()),
            Backend::Sandbox(sandbox) => {
                let compiler = sandbox.ready.get().ok_or_else(|| {
                    ApiError::Unavailable(
                        "The SQL compiler is starting. Try again in a moment.".into(),
                    )
                })?;
                let compilation = compiler.compile(sql).await.map_err(|err| {
                    tracing::warn!(error = %Chain(&err), "sandbox compile failed");
                    match err {
                        SandboxError::Busy(_) => ApiError::Unavailable(
                            "The SQL compiler is busy. Try again in a moment.".into(),
                        ),
                        _ => unavailable(),
                    }
                })?;
                Ok(Compiled {
                    model: compilation.model,
                    diagnostics: compilation.diagnostics,
                })
            }
            Backend::Function(compile) => {
                let (compile, sql) = (Arc::clone(compile), sql.to_owned());
                let mut compiled = tokio::task::spawn_blocking(move || compile(&sql))
                    .await
                    .map_err(ApiError::internal)?;
                compiled.model = compiled.model.map(Schema::canonical);
                Ok(compiled)
            }
        }
    }

    /// [`SqlCompiler::compile`] of a draft's text: refused while it has
    /// conflict blocks (publishing, Tidy).
    pub async fn compile_draft(&self, sql: &str) -> ApiResult<Compiled> {
        match refused(sql) {
            Some(refused) => Ok(refused),
            None => self.compile(sql).await,
        }
    }

    /// A draft's compile: coalesced per `key` (the draft id), never two at
    /// once for one key, rate-limited per key by the sandbox. A request still
    /// waiting when a newer one for the same key arrives answers
    /// [`Rejected::Superseded`]. Without a sandbox (tests), a plain compile.
    pub async fn compile_for(&self, key: &str, sql: String) -> Result<Compiled, Rejected> {
        if let Some(refused) = refused(&sql) {
            return Ok(refused);
        }
        let Backend::Sandbox(sandbox) = &self.0 else {
            return Ok(self.compile(&sql).await?);
        };
        let compiler = sandbox.ready.get().ok_or_else(|| {
            ApiError::Unavailable("The SQL compiler is starting. Try again in a moment.".into())
        })?;
        match compiler.compile_for(key, sql).await {
            Ok(compilation) => Ok(Compiled {
                model: compilation.model,
                diagnostics: compilation.diagnostics,
            }),
            Err(romulus_sandbox::Rejected::Superseded) => Err(Rejected::Superseded),
            Err(romulus_sandbox::Rejected::Sandbox(err)) => {
                tracing::warn!(error = %Chain(&err), key, "sandbox draft compile failed");
                Err(Rejected::Api(unavailable()))
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
        migrations: Vec<String>,
        from: usize,
        events: tokio::sync::mpsc::Sender<ReplayEvent>,
    ) -> ApiResult<Replay> {
        match &self.0 {
            Backend::None => Err(unavailable()),
            Backend::Sandbox(sandbox) => {
                let compiler = sandbox.ready.get().ok_or_else(|| {
                    ApiError::Unavailable(
                        "The SQL compiler is starting. Try again in a moment.".into(),
                    )
                })?;
                compiler
                    .replay(migrations, from, events)
                    .await
                    .map_err(|err| match err {
                        SandboxError::Abandoned => abandoned(),
                        SandboxError::Busy(_) => {
                            tracing::warn!(error = %Chain(&err), "sandbox import replay failed");
                            ApiError::Unavailable(
                                "The SQL compiler is busy with another import. Try again in a minute."
                                    .into(),
                            )
                        }
                        _ => {
                            tracing::warn!(error = %Chain(&err), "sandbox import replay failed");
                            unavailable()
                        }
                    })
            }
            Backend::Function(compile) => {
                let compile = Arc::clone(compile);
                tokio::task::spawn_blocking(move || {
                    replay_with(&*compile, &migrations, from, &events)
                })
                .await
                .map_err(ApiError::internal)?
            }
        }
    }

    /// For `/api/health`: whether compiles can run now, asked of the
    /// sandbox at most every [`HEALTH_TTL`]. A sandbox compiler that lost
    /// its prefix to another process (`PrefixInUse`) has stopped compiling
    /// for good and says so here.
    pub async fn status(&self) -> CompilerStatus {
        let sandbox = match &self.0 {
            Backend::None => return CompilerStatus::Off,
            Backend::Function(_) => return CompilerStatus::Ok,
            Backend::Sandbox(sandbox) => sandbox,
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
                tracing::warn!(error = %Chain(&err), "health: the sandbox compiler is down");
                CompilerStatus::Unavailable
            }
        };
        *lock() = Some((Instant::now(), status));
        status
    }

    /// Releases the sandbox pool (and its prefix), on shutdown.
    pub async fn close(&self) {
        if let Backend::Sandbox(sandbox) = &self.0
            && let Some(compiler) = sandbox.ready.get()
        {
            compiler.close().await;
        }
    }
}

/// [`SqlCompiler::replay`] with a function for a compiler (tests).
fn replay_with(
    compile: &CompileFn,
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

fn unavailable() -> ApiError {
    ApiError::Unavailable(
        "The SQL compiler is not available right now. Try again in a minute.".into(),
    )
}

/// Starts the compiler, retrying while the sandbox is not there yet (or
/// another process holds the prefix); a version mismatch is for good.
async fn start(config: SandboxConfig, sandbox: Arc<Sandbox>) {
    let mut wait = Duration::from_secs(1);
    loop {
        match Compiler::start(config.clone()).await {
            Ok(compiler) => {
                tracing::info!(
                    prefix = %config.prefix,
                    server = compiler.server_version(),
                    "sandbox compiler ready"
                );
                let _ = sandbox.ready.set(compiler);
                return;
            }
            Err(err @ SandboxError::VersionMismatch { .. }) => {
                tracing::error!(error = %Chain(&err), "sandbox compiler disabled");
                return;
            }
            Err(err) => {
                tracing::warn!(error = %Chain(&err), retry_in = ?wait, "sandbox compiler not ready");
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(MAX_RETRY);
            }
        }
    }
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
