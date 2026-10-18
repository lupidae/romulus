//! Compiles draft SQL into a remus schema model on a sandbox Postgres, and
//! the SQL tooling around it.
//!
//! romulus keeps a schema as SQL text and needs the model remus would read
//! from a real database. Rather than translate SQL itself, it runs the text
//! in a sandbox Postgres, introspects it with remus's own query, and rolls
//! back (see `docs/MVP.md`, "The core design").
//!
//! # Pure SQL tooling (no database)
//!
//! - [`parse`] splits a text into [`Statement`]s with exact [`Span`]s (bytes,
//!   UTF-16 for the browser, line/column) and a [`Key`] naming the object
//!   each one defines (`table:public.users`, `fk:public.orders.orders_user_id_fkey`,
//!   `comment:column:public.users.email`…). It never fails: every statement
//!   that does not parse is a located syntax error, the others are still
//!   returned. The statement-level merge and graph edits build on it.
//! - [`allowlist::check`] refuses everything that is not schema DDL, with a
//!   located error and a hint (its module documents the threat model).
//! - [`render_sql`] renders a model as SQL (remus-sql, reordered where it
//!   would not compile back), for drafts started from a version pushed as
//!   remus JSON.
//! - [`draft_migration::script`] is what a draft written as a migration
//!   compiles: its base version's SQL, then the migration, with what a
//!   schema cannot hold (data, `BEGIN`/`COMMIT`, `CONCURRENTLY`) blanked
//!   in place; [`draft_migration::Script::relocate`] puts the diagnostics
//!   back in the migration.
//! - [`playground::script`] is what a playground runs to hold a schema
//!   that compiled: the stubs the compile settled on, without extensions
//!   (the playground's init installs them).
//! - [`merge::merge`] is "Update draft": the statement-level three-way merge
//!   of a draft with a newer version, conflicts as blocks the compile
//!   refuses ([`merge::conflict_diagnostics`]).
//!
//! # The compiler
//!
//! [`Compiler::start`] sets up a pool of sandbox databases (privileged,
//! once); [`Compiler::compile`] and the per-draft, coalesced
//! [`Compiler::compile_for`] return a [`Compilation`]: the canonical model,
//! or [`Diagnostic`]s located in the user's text. On the way, functions the
//! text calls but does not define are [stubbed](stubs), policy roles are
//! rewritten to PUBLIC and restored, missing schemas are created, and
//! objects extensions already provide are skipped ([`prepare`] details the
//! script the sandbox runs). None of that reaches the model.
//!
//! ```no_run
//! use romulus_sandbox::{Compiler, SandboxConfig};
//!
//! # async fn example() -> Result<(), romulus_sandbox::SandboxError> {
//! let compiler = Compiler::start(SandboxConfig::new(
//!     "postgres://sandbox_admin:sandbox_admin@localhost:5432/postgres",
//! ))
//! .await?;
//! let compiled = compiler
//!     .compile("CREATE TABLE users (id bigint PRIMARY KEY, org uuid DEFAULT auth.org())")
//!     .await?;
//! let model = compiled.model.expect("compiles");
//! assert_eq!(model.entities[0].name, "users");
//! assert_eq!(compiled.stubs[0].signature(), "auth.org() → uuid");
//! # Ok(())
//! # }
//! ```

pub mod allowlist;
pub mod catalog;
mod compiler;
pub mod diagnostic;
pub mod draft_migration;
pub mod import;
pub mod key;
pub mod merge;
mod names;
mod parse;
pub mod playground;
pub mod prepare;
mod refine;
mod render;
pub mod source;
pub mod stubs;
mod walk;

pub use compiler::{
    Compilation, Compiler, MAJORS, MODEL_DATABASE, Metrics, Rejected, Replay, ReplayEvent,
    ReplayFailure, SandboxConfig, SandboxError,
};
pub use diagnostic::{Code, Diagnostic, Severity};
pub use key::{Action, Key, KeyParseError, ObjectKind};
pub use parse::{ParsedSql, Statement, parse};
/// The `pg_query` this crate parses with, for callers that walk
/// [`Statement::node`].
pub use pg_query;
pub use render::render_sql;
pub use source::{LineIndex, Position, Span};
pub use stubs::Stub;
