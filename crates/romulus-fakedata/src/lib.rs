//! Fake rows for a schema: the generator of romulus's playgrounds
//! (docs/playground.md, section 2).
//!
//! Given a remus model, a size, a profile and a seed, [`plan`] decides how
//! many rows each table gets and how every column is filled, and returns a
//! [`Plan`]: an ordered list of [`Step`]s to run on the playground's
//! Postgres (rows streamed as `COPY … FROM STDIN` in text format, and SQL),
//! and a [`Report`] of what it decided. The same model, options and
//! [`GENERATOR_VERSION`] give the same rows: every value is a pure function
//! of (seed, table, column, row).
//!
//! No I/O, no async, no OS randomness: this crate builds for
//! `wasm32-unknown-unknown` (sample rows in the browser, phase 3).
//!
//! # The interface the playground runner uses
//!
//! ```text
//! pub fn plan(model: &remus_core::Schema, opts: &Options) -> Result<Plan, Error>
//! pub fn plan_with_rows(model, opts, rows: &BTreeMap<String, u64>)    // `schema.table` → rows
//!
//! pub struct Options { size: Size, profile: Profile, seed: u64, max_rows_per_table: Option<u64> }
//! pub enum Size { Small, Medium }
//! pub enum Profile { Realistic, EdgeCases }
//!
//! impl Plan {
//!     pub fn steps(&self) -> Vec<Step>      // fresh, lazy row iterators on each call
//!     pub fn report(&self) -> Report         // serde::Serialize
//! }
//! pub enum Step {
//!     CopyTable(CopyTable),   // COPY copy.statement() FROM STDIN, rows encoded by copy_text
//!     Sql(String),            // execute as is (one statement, or a DO block)
//!     Outcome(String),        // a query returning one text value: Report::record_outcome(&it)
//! }
//! pub struct CopyTable {
//!     schema, table: String,  // the COPY target (a staging table)
//!     columns: Vec<String>,
//!     rows: Rows,             // impl Iterator<Item = Vec<Option<String>>> + ExactSizeIterator + Send
//!     target_schema, target_table: String,  // the table these rows are for (progress)
//! }
//! ```
//!
//! Running a plan:
//!
//! ```no_run
//! # fn run(model: &remus_core::Schema) -> Result<(), romulus_fakedata::Error> {
//! use romulus_fakedata::{Options, Profile, Size, Step, copy_text, plan};
//!
//! let plan = plan(model, &Options::new(Size::Small, Profile::Realistic, 0x7f3a91))?;
//! let mut report = plan.report();
//! for step in plan.steps() {
//!     match step {
//!         Step::CopyTable(copy) => {
//!             let statement = copy.statement(); // COPY "romulus_fakedata_…"."t3_orders" (…) FROM STDIN
//!             let mut buffer = Vec::new();
//!             for row in copy.rows {
//!                 copy_text(&row, &mut buffer); // one line of COPY text format
//!                 // send `buffer` in chunks, clear it
//!             }
//!         }
//!         Step::Sql(sql) => { /* execute */ }
//!         Step::Outcome(query) => {
//!             let json: String = todo!("fetch the one text value of `query`");
//!             report.record_outcome(&json)?;
//!         }
//!     }
//! }
//! # Ok(()) }
//! ```
//!
//! **Contract for the runner.** Run every step, in order, on one session,
//! as the schema's owner (it creates and drops a staging schema, switches
//! FORCE ROW LEVEL SECURITY off for the load and back on at the end, and
//! moves sequences), on
//! PostgreSQL 16 or later (`pg_input_is_valid`), with `client_encoding`
//! `UTF8` (rows are UTF-8). Autocommit is fine, and so
//! is one transaction around everything. No step fails because of the
//! data: rows Postgres refuses are dropped and counted (the outcome); a
//! step that does fail is a bug or an environment problem (disk full,
//! timeout, privileges).
//!
//! # How rows are made
//!
//! - **Order.** Tables load parents first. A cycle of foreign keys is
//!   broken at a nullable one, loaded NULL and filled by an `UPDATE` once
//!   both sides are in; a cycle `NOT NULL` both ways loads in one block with
//!   all its keys held (deferred, or dropped and added back), the rows left
//!   without a parent removed in every direction until none is, and the
//!   cycle left empty only if that block still fails. A nullable
//!   self-reference is a tree filled the same way.
//! - **Keys** are unique by construction: each unique set gets one column
//!   whose values carry the row number (`rhoda.wilkinson17@example.com`),
//!   or is drawn as a combination of its foreign keys and small domains,
//!   from a permutation (no repeats). Foreign keys copy a parent row's key,
//!   computed for that row directly; identity and serial keys that others
//!   point at are written as `1, 2, 3…` and their sequence moved past them.
//! - **CHECKs** shaped as bounds, lists, lengths, patterns, column orders
//!   and `num_nonnulls` are satisfied by construction; others are
//!   evaluated here when readable (the row is drawn again), and judged by
//!   Postgres in the load either way.
//! - **Values** follow column-name heuristics checked against the type
//!   ([`Heuristic`]), else the type; partition keys land inside a
//!   partition the bounds describe.

#![allow(
    // A data generator converts between numeric domains all the time; the
    // values are bounded by construction before any cast.
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
)]

mod checks;
mod expr;
mod generate;
mod heuristics;
mod model;
mod partition;
mod report;
mod rng;
mod rows;
mod sql;
mod types;
mod value;
mod words;

use std::{collections::BTreeMap, sync::Arc};

pub use heuristics::Heuristic;
pub use model::{Fallback, FallbackKind, Role};
pub use report::{ColumnReport, DropReason, Dropped, Report, Skipped, Source, TableReport};
pub use rows::Rows;
use serde::{Deserialize, Serialize};

/// Bumped whenever the same model, options and seed would give other rows:
/// stored with a playground, which is rebuilt only with the same version.
pub const GENERATOR_VERSION: u32 = 1;

/// How many rows (docs/playground.md, "Volume presets").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Size {
    /// Lookups 10–50 rows, entities ~200, events ~2 000; 50 k rows at most.
    Small,
    /// Lookups 10–200, entities ~10 000, events ~100 000; 1 M rows at most.
    Medium,
}

/// What the rows aim at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Profile {
    /// Plausible data.
    Realistic,
    /// The rows that break migrations: many NULLs, strings at their
    /// maximum length, extreme numbers, empty and non-ASCII strings.
    EdgeCases,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Options {
    pub size: Size,
    pub profile: Profile,
    pub seed: u64,
    /// A cap on every table (the presets still apply below it).
    pub max_rows_per_table: Option<u64>,
}

impl Options {
    pub fn new(size: Size, profile: Profile, seed: u64) -> Self {
        Self {
            size,
            profile,
            seed,
            max_rows_per_table: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The text given to [`Report::record_outcome`] is not what the outcome
    /// query returns.
    #[error("the outcome is not the JSON the outcome query returns: {0}")]
    Outcome(String),
}

/// Rows to look at: the column names, then each row's values.
pub type Sample = (Vec<String>, Vec<Vec<Option<String>>>);

/// The load of one schema's rows.
#[derive(Clone)]
pub struct Plan {
    model: Arc<model::Model>,
}

/// One thing to run, in order.
pub enum Step {
    /// `COPY` these rows into a staging table.
    CopyTable(CopyTable),
    /// SQL to execute as is: exactly one statement (often a `DO` block),
    /// so the simple and the extended query protocol both take it. Its
    /// first line is a `-- romulus_fakedata: …` comment saying what it does.
    Sql(String),
    /// A query returning one row of one text column: give it to
    /// [`Report::record_outcome`]. It comes before the step that drops the
    /// staging schema, where the outcome is kept.
    Outcome(String),
}

impl std::fmt::Debug for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Step::CopyTable(c) => f
                .debug_struct("CopyTable")
                .field("schema", &c.schema)
                .field("table", &c.table)
                .field("columns", &c.columns)
                .field("rows", &c.rows.len())
                .finish(),
            Step::Sql(s) => f.debug_tuple("Sql").field(s).finish(),
            Step::Outcome(s) => f.debug_tuple("Outcome").field(s).finish(),
        }
    }
}

impl Step {
    /// What the step does, in a few words (for progress).
    pub fn label(&self) -> String {
        match self {
            Step::CopyTable(c) => format!(
                "rows of {}.{} ({})",
                c.target_schema,
                c.target_table,
                c.rows.len()
            ),
            Step::Sql(s) | Step::Outcome(s) => s
                .lines()
                .next()
                .and_then(|l| l.strip_prefix("-- romulus_fakedata: "))
                .unwrap_or("sql")
                .to_owned(),
        }
    }
}

/// Rows for `COPY schema.table (columns) FROM STDIN` (text format).
pub struct CopyTable {
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    /// Each row's values in column order, `None` for NULL, unescaped.
    pub rows: Rows,
    /// The table the rows are for.
    pub target_schema: String,
    pub target_table: String,
}

impl CopyTable {
    /// The `COPY … FROM STDIN` statement.
    pub fn statement(&self) -> String {
        let columns: Vec<String> = self.columns.iter().map(|c| sql::quote(c)).collect();
        format!(
            "COPY {}.{} ({}) FROM STDIN",
            sql::quote(&self.schema),
            sql::quote(&self.table),
            columns.join(", ")
        )
    }
}

/// Appends one row in `COPY` text format: tab-separated, `\N` for NULL,
/// backslash, tab, newline and carriage return escaped, then a newline. A
/// NUL, which Postgres text cannot hold, is left out (the generator never
/// makes one: this is a backstop). The bytes are UTF-8: the session's
/// `client_encoding` must be `UTF8`.
pub fn copy_text(row: &[Option<String>], out: &mut Vec<u8>) {
    for (i, value) in row.iter().enumerate() {
        if i > 0 {
            out.push(b'\t');
        }
        match value {
            None => out.extend_from_slice(b"\\N"),
            Some(v) => {
                for b in v.bytes() {
                    match b {
                        b'\\' => out.extend_from_slice(b"\\\\"),
                        b'\t' => out.extend_from_slice(b"\\t"),
                        b'\n' => out.extend_from_slice(b"\\n"),
                        b'\r' => out.extend_from_slice(b"\\r"),
                        0 => {}
                        other => out.push(other),
                    }
                }
            }
        }
    }
    out.push(b'\n');
}

/// Plans the rows of `model`.
///
/// It does not fail on a model remus produced: what it cannot generate is a
/// fallback in the report (a table skipped, a constraint left to Postgres).
pub fn plan(model: &remus_core::Schema, opts: &Options) -> Result<Plan, Error> {
    plan_with_rows(model, opts, &BTreeMap::new())
}

/// [`plan`], with some tables' row counts set by hand (the create dialog's
/// "Rows per table…"): `rows` maps `schema.table` to a count. The size's
/// cap still holds: past it, the other tables shrink first, then these.
/// Keys and parents still cap a table, and the report says why.
pub fn plan_with_rows(
    model: &remus_core::Schema,
    opts: &Options,
    rows: &BTreeMap<String, u64>,
) -> Result<Plan, Error> {
    Ok(Plan {
        model: Arc::new(model::analyze(model, opts, rows)),
    })
}

impl Plan {
    /// The steps, in order. Each call starts the row iterators afresh.
    pub fn steps(&self) -> Vec<Step> {
        let m = &self.model;
        let mut steps: Vec<Step> = sql::setup(m).into_iter().map(Step::Sql).collect();
        // Row level security forced on the owner stays off for the whole
        // load: a child's parent check and a materialized view read the
        // parent's rows as the owner, past the policies.
        let forced: Vec<&model::Table> = m.tables.iter().filter(|t| t.rls_forced).collect();
        for t in &forced {
            steps.push(Step::Sql(sql::row_security(t, false)));
        }
        for unit in &m.units {
            let tables: Vec<usize> = unit
                .tables
                .iter()
                .copied()
                .filter(|&t| m.tables[t].skip.is_none() && m.tables[t].rows > 0)
                .collect();
            if tables.is_empty() {
                continue;
            }
            for &ti in &tables {
                let t = &m.tables[ti];
                steps.push(Step::Sql(sql::create_staging(m, t)));
                steps.push(Step::CopyTable(CopyTable {
                    schema: m.staging.clone(),
                    table: t.staging.clone(),
                    columns: sql::copy_columns(t),
                    rows: Rows::new(Arc::clone(&self.model), ti),
                    target_schema: t.schema.clone(),
                    target_table: t.name.clone(),
                }));
            }
            if unit.held.is_empty() {
                for &ti in &tables {
                    steps.push(Step::Sql(sql::load_table(m, &m.tables[ti])));
                }
            } else {
                steps.push(Step::Sql(sql::load_held(m, unit)));
            }
            for &ti in &tables {
                let t = &m.tables[ti];
                for (fi, fk) in t.fks.iter().enumerate() {
                    if fk.deferred
                        && let Some(s) = sql::fill_deferred(m, t, fi)
                    {
                        steps.push(Step::Sql(s));
                    }
                }
                if let Some(s) = sql::sequences(m, t) {
                    steps.push(Step::Sql(s));
                }
            }
            let staged: Vec<&model::Table> = tables.iter().map(|&t| &m.tables[t]).collect();
            steps.push(Step::Sql(sql::drop_staging(m, &staged)));
        }
        for matview in &m.matviews {
            steps.push(Step::Sql(sql::refresh(m, matview)));
        }
        for t in &forced {
            steps.push(Step::Sql(sql::row_security(t, true)));
        }
        steps.push(Step::Outcome(sql::outcome_query(m)));
        steps.push(Step::Sql(sql::cleanup(m)));
        if let Some(s) = sql::analyze(m) {
            steps.push(Step::Sql(s));
        }
        steps
    }

    /// What the plan decided; [`Report::record_outcome`] adds what Postgres
    /// judged.
    pub fn report(&self) -> Report {
        report::build(&self.model)
    }

    /// Rows planned in all.
    pub fn rows(&self) -> u64 {
        self.model.tables.iter().map(|t| t.rows).sum()
    }

    /// The rows of one table as they will be loaded, by name, to look at
    /// (`None` for a table not planned). Values in Postgres text form, for
    /// every column written; `romulus_ord` first.
    pub fn sample(&self, schema: &str, table: &str, limit: u64) -> Option<Sample> {
        let ti = self
            .model
            .tables
            .iter()
            .position(|t| t.schema == schema && t.name == table)?;
        let t = &self.model.tables[ti];
        let rows = Rows::new(Arc::clone(&self.model), ti)
            .take(usize::try_from(limit).unwrap_or(usize::MAX));
        Some((sql::copy_columns(t), rows.collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_text_escapes_what_copy_reads_specially() {
        let mut out = Vec::new();
        copy_text(
            &[
                Some("a\tb\0".into()),
                None,
                Some("c\\d\ne".into()),
                Some(String::new()),
            ],
            &mut out,
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "a\\tb\t\\N\tc\\\\d\\ne\t\n"
        );
    }
}
