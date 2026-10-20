//! romulus in the browser, for the homepage's live hero: no account, no
//! server, nothing sent anywhere.
//!
//! - [`compile`]: SQL to a remus model, without Postgres. The browser
//!   parses the text with `libpg_query` (Postgres 17's own parser, built to
//!   wasm: the npm `libpg-query`, the grammar the app's server reads with);
//!   this crate runs the statements against a catalog in miniature
//!   ([`catalog`]) and prints every definition as Postgres's `ruleutils`
//!   would ([`expr`], [`view`]). Errors are Postgres's where a parser and a
//!   catalog can tell (a missing table or column, a duplicate, a foreign key
//!   without a unique key, a type mismatch…); the app compiles in a real
//!   Postgres sandbox, which this does not replace.
//! - [`diff`] and [`migration`]: romulus-diff and romulus-migration, as the
//!   API serves them (`VersionDiff`'s `diff` and `sql`, `SchemaMigration`).
//!
//! The wasm functions ([`bindings`]) take and return JSON strings.

// A panic in the browser aborts the wasm instance (panic = "abort"): the
// engine returns errors instead, and these keep it so.
#![cfg_attr(
    not(test),
    deny(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        clippy::unreachable,
        clippy::unimplemented,
        clippy::indexing_slicing,
        clippy::string_slice
    )
)]

mod allow;
pub mod bindings;
pub mod catalog;
mod ddl;
pub mod diagnostic;
pub mod expr;
mod names;
mod objects;
mod table;
mod tree;
pub mod types;
pub mod view;

pub use diagnostic::{Code, Diagnostic, LineIndex, Severity, Span};
use remus_core::Schema;
use romulus_migration::{
    Candidate, File, Format, Meta, Options, RenameChoices, Statement, Warning,
};
use serde::{Deserialize, Serialize};

/// What a compile gives: a model, or the errors that block one; notes
/// either way.
#[derive(Debug, Clone, Serialize)]
pub struct Compiled {
    pub model: Option<Schema>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Compiled {
    pub(crate) fn failed(diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            model: None,
            diagnostics,
        }
    }
}

/// Compiles `sql` from its parse tree (`libpg_query`'s JSON).
pub fn compile(sql: &str, tree: &str) -> Compiled {
    ddl::compile(sql, tree)
}

/// The compile of a text that does not parse: `libpg_query`'s error, at its
/// `cursorpos` (1-based, in characters; 0 when it has none).
pub fn syntax_error(sql: &str, message: &str, cursor: u32) -> Compiled {
    let lines = LineIndex::new(sql);
    let mut d = Diagnostic::error(Code::SyntaxError, message);
    d.sqlstate = Some("42601".to_owned());
    if cursor > 0 {
        let start = lines.char_offset(usize::try_from(cursor - 1).unwrap_or(0));
        d.span = Some(lines.span(start, lines.token_end(start)));
    } else {
        d.span = Some(lines.span(sql.len(), sql.len()));
    }
    Compiled::failed(vec![d])
}

/// Two models compared, as the diff page gets them.
#[derive(Debug, Clone, Serialize)]
pub struct Compared {
    pub diff: romulus_diff::SchemaDiff,
    pub sql: CompareSql,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompareSql {
    pub before: String,
    pub after: String,
}

/// `before` compared with `after`: romulus-diff's diff, and both models as
/// SQL in canonical order with `after`'s header, so a line diff of the two
/// shows only what the schema changed.
pub fn diff(before: &Schema, after: &Schema) -> Compared {
    let mut like = before.clone();
    after.remus_version.clone_into(&mut like.remus_version);
    after.database.clone_into(&mut like.database);
    after.server_version.clone_into(&mut like.server_version);
    Compared {
        diff: romulus_diff::diff(before, after),
        sql: CompareSql {
            before: romulus_diff::render_sql(&like),
            after: romulus_diff::render_sql(after),
        },
    }
}

/// The model's other formats, as remus emits them (the text pane's JSON,
/// DBML and Mermaid tabs).
#[derive(Debug, Clone, Serialize)]
pub struct Texts {
    pub json: String,
    pub dbml: String,
    pub mermaid: String,
}

pub fn texts(schema: &Schema) -> Texts {
    let options = remus_core::emit::DiagramOptions::default();
    Texts {
        json: remus_core::emit::json(schema).unwrap_or_default(),
        dbml: remus_dbml::render(schema, &options),
        mermaid: remus_mermaid::render(schema, &options),
    }
}

/// What a migration is asked for: the tool and the names the API would
/// put in its files (the browser gives the date, it has the clock).
#[derive(Debug, Clone, Deserialize)]
pub struct MigrationRequest {
    pub format: Format,
    #[serde(default)]
    pub concurrently: bool,
    /// The first file's version; Flyway's defaults to `to`, the others' to
    /// `timestamp`.
    #[serde(default)]
    pub version: Option<String>,
    /// The description in file names.
    #[serde(default)]
    pub name: Option<String>,
    pub from: Option<i32>,
    pub to: i32,
    pub from_label: String,
    pub to_label: String,
    /// `org/db`.
    pub database: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// `YYYYMMDDHHMMSS`.
    pub timestamp: String,
    /// The database's Postgres major.
    #[serde(default = "default_major")]
    pub major: u32,
    /// Detected renames turned on or off, by key (the API's `renames`).
    #[serde(default)]
    pub renames: std::collections::BTreeMap<String, bool>,
}

fn default_major() -> u32 {
    17
}

/// The API's `SchemaMigration`, field for field.
#[derive(Debug, Clone, Serialize)]
pub struct SchemaMigration {
    pub from: Option<i32>,
    pub to: i32,
    pub format: Format,
    pub statements: Vec<Statement>,
    pub warnings: Vec<Warning>,
    pub destructive: usize,
    pub files: Vec<File>,
    pub archive: String,
    pub pg_major: u32,
    pub renames: Vec<Candidate>,
}

/// The migration from `old` to `new`, laid out for a tool, as the API's
/// `versions::migration::build` makes it.
pub fn migration(old: &Schema, new: &Schema, request: &MigrationRequest) -> SchemaMigration {
    let format = request.format;
    let name = request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map_or_else(
            || romulus_migration::slug(&format!("{}_to_{}", request.from_label, request.to_label)),
            romulus_migration::slug,
        );
    let version = match request
        .version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(v) => v.to_owned(),
        None if format == Format::Flyway => request.to.to_string(),
        None => request.timestamp.clone(),
    };
    let meta = Meta {
        database: request.database.clone(),
        from: request.from_label.clone(),
        to: request.to_label.clone(),
        date: request.date.clone(),
        version,
        name,
    };
    let major = Some(request.major);
    let up = romulus_migration::migration(
        old,
        new,
        &Options {
            concurrently: request.concurrently,
            major,
            renames: RenameChoices {
                decisions: request.renames.clone(),
                ..RenameChoices::default()
            },
        },
    );
    let down = if format.has_down() {
        romulus_migration::down(old, new, &up)
    } else {
        romulus_migration::Migration::default()
    };
    let files = romulus_migration::files(&up, &down, format, &meta);
    SchemaMigration {
        from: request.from,
        to: request.to,
        format,
        destructive: up.destructive(),
        statements: up.statements,
        warnings: up.warnings,
        files,
        archive: romulus_migration::archive_name(format, &meta),
        pg_major: request.major,
        renames: up.renames,
    }
}
