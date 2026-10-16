//! Importing migration files as a database's version history.
//!
//! A folder of migrations (or the SQL a code-based tool prints) becomes one
//! version per migration: [`plan`] detects the tool's layout ([`Format`]),
//! orders the migrations and keeps their up sections; [`analyze`] decides
//! what of each one is replayed (data, code and session statements are
//! skipped and reported, see [`statements`]); the compiler's
//! [`crate::Compiler::replay`] runs them one after the other in a single
//! sandbox session, from an empty database, and reads the model after each.
//!
//! Nothing here runs a project's code: functions, procedures and triggers
//! are replayed with a fixed body (see [`statements`]).

mod export;
mod files;
mod lexer;
pub mod statements;

pub use files::{
    Format, IgnoreReason, Ignored, Migration, Plan, PlanError, SourceFile, detect, normalize, plan,
};
pub use statements::{Analysis, SkipReason, Skipped, analyze};
