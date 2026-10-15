//! The compiler drafts use, and what a compile tells the people in a draft.
//!
//! [`DraftCompiler`] is a narrow seam over `romulus_sandbox::Compiler`:
//! [`DraftCompiler::compile_for`] (coalesced per draft, for compile-on-type)
//! and [`DraftCompiler::compile`] (one text, for publishing). Tests plug a
//! plain function in instead of a sandbox.
//!
//! TODO(M2 rebase): the versions work adds `SqlCompiler` (module `compile`)
//! with the same sandbox behind it and a `compile_for`. Only one process may
//! hold the sandbox pool's prefix, so once it lands this seam wraps
//! `state.compiler` and `main.rs` stops starting a compiler of its own.

use std::{collections::HashMap, sync::Arc};

use romulus_sandbox::{Compiler, Diagnostic, ObjectKind, Rejected, SandboxError};
use serde::Serialize;
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::domain::Schema;

/// What a compile produced: the canonical model, or the errors that prevent
/// one (then notes), located in the text.
#[derive(Debug, Clone)]
pub struct Compiled {
    pub model: Option<Schema>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    /// A newer text of the same draft replaced this one before it ran.
    #[error("superseded by a newer compile of the same draft")]
    Superseded,
    /// The sandbox is not configured, unreachable or failing.
    #[error("the compiler is unavailable: {0}")]
    Unavailable(String),
}

impl From<SandboxError> for CompileError {
    fn from(err: SandboxError) -> Self {
        Self::Unavailable(err.to_string())
    }
}

type CompileFn = dyn Fn(&str) -> Compiled + Send + Sync;

#[derive(Clone)]
enum Backend {
    None,
    Sandbox(Compiler),
    Function(Arc<CompileFn>),
}

/// Turns a draft's SQL into a model. Cheap to clone.
#[derive(Clone)]
pub struct DraftCompiler(Backend);

impl DraftCompiler {
    /// Every compile fails as unavailable (no sandbox configured).
    pub fn unavailable() -> Self {
        Self(Backend::None)
    }

    pub fn sandbox(compiler: Compiler) -> Self {
        Self(Backend::Sandbox(compiler))
    }

    /// A plain function (tests).
    pub fn from_fn(compile: impl Fn(&str) -> Compiled + Send + Sync + 'static) -> Self {
        Self(Backend::Function(Arc::new(compile)))
    }

    pub fn is_available(&self) -> bool {
        !matches!(self.0, Backend::None)
    }

    /// Compile a draft's text: requests for one draft are coalesced (a
    /// waiting one is replaced by a newer one, which answers
    /// [`CompileError::Superseded`]) and never run at once.
    pub async fn compile_for(&self, draft: Uuid, sql: String) -> Result<Compiled, CompileError> {
        match &self.0 {
            Backend::None => Err(unavailable()),
            Backend::Sandbox(sandbox) => match sandbox.compile_for(&draft.to_string(), sql).await {
                Ok(compilation) => Ok(Compiled {
                    model: compilation.model,
                    diagnostics: compilation.diagnostics,
                }),
                Err(Rejected::Superseded) => Err(CompileError::Superseded),
                Err(Rejected::Sandbox(err)) => Err(err.into()),
            },
            Backend::Function(_) => self.compile(&sql).await,
        }
    }

    /// Compile one text (publishing): results are cached by the text, so
    /// the text everyone just typed costs nothing more.
    pub async fn compile(&self, sql: &str) -> Result<Compiled, CompileError> {
        match &self.0 {
            Backend::None => Err(unavailable()),
            Backend::Sandbox(sandbox) => {
                let compilation = sandbox.compile(sql).await?;
                Ok(Compiled {
                    model: compilation.model,
                    diagnostics: compilation.diagnostics,
                })
            }
            Backend::Function(compile) => {
                let (compile, sql) = (Arc::clone(compile), sql.to_owned());
                let mut compiled = tokio::task::spawn_blocking(move || compile(&sql))
                    .await
                    .map_err(|err| CompileError::Unavailable(err.to_string()))?;
                compiled.model = compiled.model.map(Schema::canonical);
                Ok(compiled)
            }
        }
    }
}

fn unavailable() -> CompileError {
    CompileError::Unavailable("no sandbox is configured (ROMULUS_SANDBOX_URL)".into())
}

/// Hex SHA-256 of a text: how compile results name the text they are about.
pub fn sha256_hex(text: &str) -> String {
    use std::fmt::Write as _;
    Sha256::digest(text.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// Where each relation the graph draws (tables, views, materialized views,
/// foreign tables) is created: qualified name → index of its `CREATE`
/// statement. Two compiles in a row that lose a name and gain another at the
/// same index saw a rename typed in the text (docs/MVP.md "Renames").
pub fn created_at(sql: &str) -> HashMap<String, usize> {
    romulus_sandbox::parse(sql)
        .statements
        .iter()
        .filter(|statement| {
            statement.key.action() == romulus_sandbox::Action::Create
                && matches!(
                    statement.key.kind(),
                    ObjectKind::Table
                        | ObjectKind::View
                        | ObjectKind::MaterializedView
                        | ObjectKind::ForeignTable
                )
        })
        .filter_map(|statement| match statement.key.path() {
            [schema, name] => Some((format!("{schema}.{name}"), statement.index)),
            _ => None,
        })
        .collect()
}

/// Renames a compile implies: `(old name, new name)` for each relation that
/// disappeared while another appeared from the same statement.
#[expect(clippy::implicit_hasher, reason = "only ever std maps")]
pub fn renames(
    before: &HashMap<String, usize>,
    after: &HashMap<String, usize>,
) -> Vec<(String, String)> {
    let gained: HashMap<usize, &String> = after
        .iter()
        .filter(|(name, _)| !before.contains_key(*name))
        .map(|(name, &index)| (index, name))
        .collect();
    let mut out: Vec<(String, String)> = before
        .iter()
        .filter(|(name, _)| !after.contains_key(*name))
        .filter_map(|(old, index)| gained.get(index).map(|new| (old.clone(), (*new).clone())))
        .collect();
    out.sort();
    out
}

/// A compile's outcome, as everyone in the draft receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompileState {
    /// The text compiled: `model` is its model.
    Ok,
    /// The text has errors (in `diagnostics`); `model`, when sent, is the
    /// last one that compiled.
    Failed,
    /// The compiler is unavailable; the text is unchecked.
    Unavailable,
    /// Not compiled yet (just loaded); `model` is the last one that did.
    Pending,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_hex_sha256() {
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn relations_are_found_by_their_create_statement() {
        let sql = "CREATE SCHEMA app;\nCREATE TABLE users (id int);\nCOMMENT ON TABLE users IS 'x';\n\
                   CREATE VIEW app.v AS SELECT 1;\nCREATE INDEX ON users (id);";
        let found = created_at(sql);
        assert_eq!(found.get("public.users"), Some(&1));
        assert_eq!(found.get("app.v"), Some(&3));
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn a_name_changed_in_place_is_a_rename() {
        let before = created_at("CREATE TABLE a (id int); CREATE TABLE users (id int);");
        let after = created_at("CREATE TABLE a (id int); CREATE TABLE members (id int);");
        assert_eq!(
            renames(&before, &after),
            [("public.users".to_owned(), "public.members".to_owned())]
        );
        // Dropped, or added elsewhere: not a rename.
        let dropped = created_at("CREATE TABLE a (id int);");
        assert_eq!(renames(&before, &dropped), []);
        let moved = created_at("CREATE TABLE members (id int); CREATE TABLE a (id int);");
        assert_eq!(renames(&before, &moved), []);
    }
}
