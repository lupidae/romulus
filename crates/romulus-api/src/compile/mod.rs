//! Compiling SQL into a remus model, for uploads and SQL pushes (drafts
//! later).
//!
//! [`SqlCompiler`] is the one seam handlers call: [`SqlCompiler::compile`]
//! answers a [`Compiled`], the canonical model or [`Diagnostic`]s located in
//! the text. Only the compiler itself failing is an error (503 `unavailable`),
//! and nothing is ever stored from one. Its backends:
//!
//! - the sandbox compiler (`romulus-sandbox`: DDL allowlist, auto-stubs, a
//!   pool of sandbox databases, results cached by SHA-256), started from
//!   `Config::sandbox_url`. It lands separately; until it is wired here,
//!   [`SqlCompiler::unavailable`] stands in;
//! - none: every compile answers 503 (no sandbox configured);
//! - a function, for tests that need a model without a sandbox.
//!
//! [`render_sql`] is the SQL of a model that came as remus JSON, and the
//! [`render`] functions its other formats.
//!
//! Drafts (milestone 4) will compile here too, debounced and coalesced per
//! draft, with the last result cached in `draft_compiles`.

pub mod diagnostic;

use std::sync::Arc;

use remus_core::emit::DiagramOptions;
use utoipa_axum::router::OpenApiRouter;

pub use self::diagnostic::{Code, Diagnostic, Position, Severity, Span};
use crate::{ApiError, ApiResult, AppState, domain::Schema};

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

impl Compiled {
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter().filter(|d| d.is_error())
    }
}

type CompileFn = dyn Fn(&str) -> Compiled + Send + Sync;

#[derive(Clone)]
enum Backend {
    None,
    Function(Arc<CompileFn>),
}

/// Turns SQL into a model. Cheap to clone. See the module documentation.
#[derive(Clone)]
pub struct SqlCompiler(Backend);

impl SqlCompiler {
    /// No compiler: every compile answers 503 `unavailable`.
    pub fn unavailable() -> Self {
        Self(Backend::None)
    }

    /// A compiler that is a plain function (tests).
    pub fn from_fn(compile: impl Fn(&str) -> Compiled + Send + Sync + 'static) -> Self {
        Self(Backend::Function(Arc::new(compile)))
    }

    pub fn is_available(&self) -> bool {
        !matches!(self.0, Backend::None)
    }

    pub async fn compile(&self, sql: &str) -> ApiResult<Compiled> {
        match &self.0 {
            Backend::None => Err(ApiError::Unavailable(
                "The SQL compiler is not available right now. Try again in a minute.".into(),
            )),
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
}

/// The SQL of a model, for versions that came as remus JSON: what their
/// SQL tab shows and what a draft started from them holds.
///
/// `remus_sql::render` for now; `romulus_sandbox::render_sql` (the same,
/// reordered where remus-sql's output would not compile back) once the
/// sandbox lands.
pub fn render_sql(schema: &Schema) -> String {
    remus_sql::render(schema)
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
