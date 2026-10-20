//! What a run says: each statement measured ([`StatementResult`]), each
//! phase and its checks ([`PhaseReport`]), the data that did not come back
//! ([`DataLoss`]), and a verdict. Serializable as is: the API stores it,
//! the page shows it.
//!
//! The statement types are the playground's migration run's too (`Run
//! migration`): one runner, one shape (`romulus_playground::protocol`
//! re-exports them under the names its OpenAPI document has always used).

use serde::{Deserialize, Serialize};
#[cfg(feature = "utoipa")]
use utoipa::ToSchema;

/// How a statement of a run went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationRunStatus))]
#[serde(rename_all = "snake_case")]
pub enum StatementStatus {
    Passed,
    Failed,
    /// Not run: an earlier statement failed.
    NotRun,
    /// Transaction control a run does not need (`BEGIN`…).
    Skipped,
}

/// A lock a statement held, from `pg_locks` before its `COMMIT`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationRunLock))]
pub struct Lock {
    /// `schema.name` of the relation.
    pub relation: String,
    /// `AccessExclusiveLock`…
    pub mode: String,
}

/// A Postgres error, located.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = PlaygroundSqlError))]
pub struct SqlError {
    pub sqlstate: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Where in the text the request sent (this statement for a migration,
    /// the whole console text for a run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<TextSpan>,
}

/// A range of a text: bytes, and UTF-16 code units (the browser's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = PlaygroundTextSpan))]
pub struct TextSpan {
    pub start: usize,
    pub end: usize,
    pub utf16_start: usize,
    pub utf16_end: usize,
}

impl TextSpan {
    /// The span of `range` in `text`.
    pub fn of(text: &str, range: std::ops::Range<usize>) -> Self {
        let utf16 = |byte: usize| {
            text.get(..byte)
                .map_or(0, |prefix| prefix.encode_utf16().count())
        };
        Self {
            start: range.start,
            end: range.end,
            utf16_start: utf16(range.start),
            utf16_end: utf16(range.end),
        }
    }
}

/// What one statement of a run did.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationRunStatement))]
pub struct StatementResult {
    pub index: usize,
    pub sql: String,
    pub status: StatementStatus,
    /// `deletes_data`, `can_fail`, `widens_access` (romulus-migration's
    /// hazards), when the statement has one.
    #[serde(default)]
    pub hazard: Option<String>,
    pub duration_ms: u64,
    /// Exact, for statements in a transaction.
    #[serde(default)]
    pub locks: Vec<Lock>,
    /// Outside a transaction (`CONCURRENTLY`): the documented lock, not a
    /// measured one.
    #[serde(default)]
    pub locks_documented: bool,
    /// Tables (and materialized views) rewritten: their files changed.
    #[serde(default)]
    pub rewrote: Vec<String>,
    /// Rows inserted, updated or deleted, from `pg_stat_xact_user_tables`.
    #[serde(default)]
    pub rows_changed: Option<i64>,
    /// Postgres's command tag (`ALTER TABLE`, `UPDATE 12`).
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub error: Option<SqlError>,
    /// Rows of the table that broke it (a NOT NULL, a unique key…), when the
    /// error says which, at most 5, as `column = value` text.
    #[serde(default)]
    pub offending: Option<Offending>,
}

/// Rows that make a statement fail.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationRunOffending))]
pub struct Offending {
    /// How many rows break it.
    pub count: i64,
    /// A query that lists them (run it in the console).
    pub query: String,
    /// The first few, each as Postgres prints its key.
    pub sample: Vec<String>,
}

/// The statements of one run (a migration, up or down), whole.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationRunReport))]
pub struct MigrationReport {
    pub passed: bool,
    pub statements: Vec<StatementResult>,
    /// The index of the statement that failed.
    pub failed_at: Option<usize>,
    pub duration_ms: u64,
}

/// Which way a migration runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationDirection))]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Up,
    Down,
}

impl Direction {
    pub fn word(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

/// The parts of a test, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestPhase))]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Rows put in (generated, provided), or found there.
    Seed,
    /// The database is at the base schema; its rows counted and sampled.
    Before,
    Up,
    /// The schema is the target's; the rows kept their values.
    AfterUp,
    Down,
    /// The schema is the base's again; the rows are back as they were.
    AfterDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationCheckStatus))]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Passed,
    /// Passed, with something to look at (data that does not come back,
    /// values a check could not compare).
    Warning,
    /// Data changed that the migration does not declare (a statement
    /// written by hand that does not say it loses data, a `DO` block whose
    /// effects cannot be read): a developer acknowledges it, or it does not
    /// pass.
    NeedsAcknowledgement,
    Failed,
    /// Not run: an earlier part failed, or there is nothing to run (no
    /// down migration).
    Skipped,
}

impl Status {
    /// The worse of two.
    #[must_use]
    pub fn and(self, other: Self) -> Self {
        let rank = |s: Self| match s {
            Self::Skipped => 0,
            Self::Passed => 1,
            Self::Warning => 2,
            Self::NeedsAcknowledgement => 3,
            Self::Failed => 4,
        };
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

/// What a check looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestCheckKind))]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    /// The rows went in.
    Rows,
    /// The database introspects as the model expected.
    Schema,
    /// Rows per table.
    RowCounts,
    /// Sampled rows, column by column (by aggregates for a table without
    /// a key).
    Values,
    /// The tables the migration touches: which held rows to check on.
    Coverage,
    /// Sequences: none went back (a down migration restarting a serial).
    Sequences,
}

/// A statement of the run, as a check names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestStatementRef))]
pub struct StatementRef {
    pub direction: Direction,
    pub index: usize,
    /// Its hazard, as the migration flagged it (or as the check read it
    /// from a hand-written statement: [`StatementRef::flag`]).
    pub hazard: Option<String>,
    pub flag: Flag,
}

/// Where a statement's hazard comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestFlag))]
#[serde(rename_all = "snake_case")]
pub enum Flag {
    /// romulus-migration flagged it `deletes_data`.
    Flagged,
    /// romulus-migration planned it without `deletes_data`: data it loses
    /// was not warned about.
    NotFlagged,
    /// A hand-written statement that says it does this (a `DROP COLUMN`
    /// for a dropped column, an `UPDATE` for changed values, a `DELETE` for
    /// rows gone): its text stands for the flag.
    Written,
    /// A hand-written statement that does this without saying so (a
    /// conversion that rounds, a `DROP TYPE … CASCADE` taking columns):
    /// a developer acknowledges it.
    WrittenUndeclared,
    /// A statement whose effects cannot be read (`DO`, `CALL`, `MERGE`): the
    /// change is not verifiable, and a developer acknowledges it.
    Opaque,
    /// Rows moved, not lost: a partition detached (its rows are its own
    /// table's now) or attached.
    Moved,
}

/// One row as an example: its key, a value before and after.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestExample))]
pub struct Example {
    /// The row's primary key, as Postgres prints it (`(42)`, `(7,en)`).
    pub key: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// What happened to one column (or one table's rows) of the sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestNoteKind))]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    /// Its values followed a rename.
    Renamed,
    /// Converted to another type, values as the migration's cast gives.
    Converted,
    /// The conversion changed values (rounded, cut): they do not convert
    /// back.
    Rounded,
    /// Values changed by a statement that changes data (an `UPDATE`, a
    /// renamed enum label).
    Changed,
    /// Values changed and no statement explains it.
    Unexplained,
    /// The column was dropped.
    Dropped,
    /// Rows of the sample are gone.
    RowsMissing,
    /// Values the check could not compare (why in the message).
    NotCompared,
    /// What a table holds could not be verified (no key and columns that
    /// change, rows no key finds again): never a clean pass.
    NotVerified,
    /// The table was empty: nothing of it was checked on rows.
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestNote))]
pub struct Note {
    pub kind: NoteKind,
    /// The column (as the phase names it); `null` for the table's rows.
    pub column: Option<String>,
    /// Its name before (a rename).
    pub from: Option<String>,
    /// Sampled rows it concerns, of [`TableCheck::sampled`].
    pub rows: usize,
    pub status: Status,
    pub message: String,
    #[serde(default)]
    pub statement: Option<StatementRef>,
    pub examples: Vec<Example>,
}

/// One table in a check of counts or values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestTable))]
pub struct TableCheck {
    /// `schema.name`, as the phase names it.
    pub table: String,
    /// Its name in the base schema, when that is another (renamed).
    pub from: Option<String>,
    /// Rows before (in the base schema) and now.
    pub before: Option<i64>,
    pub after: Option<i64>,
    /// Rows sampled by primary key (0: none, see the notes).
    pub sampled: usize,
    pub status: Status,
    pub notes: Vec<Note>,
}

/// One check of a phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestCheck))]
pub struct CheckReport {
    pub kind: CheckKind,
    pub status: Status,
    /// One sentence: `Schema matches v8.`, `250 sampled rows kept their
    /// values.`
    pub summary: String,
    /// More lines when there is more to say (the schema's differences).
    pub details: Vec<String>,
    /// The tables with something to say (all of them for row counts),
    /// at most [`crate::MAX_TABLES_LISTED`].
    pub tables: Vec<TableCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestPhaseReport))]
pub struct PhaseReport {
    pub phase: Phase,
    pub status: Status,
    pub duration_ms: u64,
    pub summary: String,
    pub checks: Vec<CheckReport>,
}

/// How a run's data was lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestLossKind))]
#[serde(rename_all = "snake_case")]
pub enum LossKind {
    /// A column dropped: its values are gone (the down migration adds it
    /// back empty).
    ColumnDropped,
    /// Values not back as they were (a rounding conversion, an `UPDATE`).
    ValuesChanged,
    /// Rows gone (a table dropped and created again, a `DELETE`).
    RowsDeleted,
}

/// Data a run did not give back: after the down migration (or after the
/// up migration when there is none to run).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestDataLoss))]
pub struct DataLoss {
    /// `schema.name` in the base schema.
    pub table: String,
    pub column: Option<String>,
    pub kind: LossKind,
    /// Sampled rows it concerns, of `sampled`.
    pub rows: usize,
    pub sampled: usize,
    /// Rows counted before and after, for lost rows.
    #[serde(default)]
    pub count_before: Option<i64>,
    #[serde(default)]
    pub count_after: Option<i64>,
    pub examples: Vec<Example>,
    /// The statement that explains it.
    pub cause: Option<StatementRef>,
    /// Explained by a statement flagged `deletes_data` (or a hand-written
    /// one that says it drops, deletes or changes it): what the migration
    /// said it would do. Unexpected loss fails the test.
    pub expected: bool,
    /// `warning` (expected), `needs_acknowledgement` (a hand-written
    /// statement does it without saying so, or a `DO` block may have), or
    /// `failed`.
    pub status: Status,
    pub message: String,
}

/// The rows a test ran on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestData))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DataSource {
    /// Generated by romulus-fakedata.
    Generated {
        size: String,
        profile: String,
        /// Hex: the same model, size, profile, seed and generator give the
        /// same rows.
        seed: String,
        generator: String,
    },
    /// Rows already there (a playground's, copied).
    Existing { label: String },
    /// Rows the caller inserted (a test's own).
    Provided,
}

/// The down migration's part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestDownStatus))]
#[serde(rename_all = "snake_case")]
pub enum DownStatus {
    /// Ran (see the phases).
    Ran,
    /// None was given (a migration written by hand without one).
    NotProvided,
    /// Not run: the up migration or its checks failed first.
    NotRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestVerdict))]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Everything ran, up and down; schemas as expected; data kept, or lost
    /// only where the migration says it is lost.
    Passed,
    /// As [`Verdict::Passed`], but there was no down migration to run.
    PassedUpOnly,
    /// It ran, but data changed that the migration does not declare: a
    /// developer acknowledges it, or it does not pass.
    NeedsAcknowledgement,
    Failed,
}

/// A test, whole.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestReport))]
pub struct Report {
    pub verdict: Verdict,
    /// One sentence for the top of the page.
    pub headline: String,
    pub data: DataSource,
    /// Rows sampled per table, at most.
    pub sample: usize,
    pub tables: usize,
    pub rows: i64,
    pub phases: Vec<PhaseReport>,
    pub up: MigrationReport,
    pub down_status: DownStatus,
    pub down: Option<MigrationReport>,
    pub data_loss: Vec<DataLoss>,
    /// Every table the migration changes (something rows matter to touches
    /// it: data changed, a constraint validated…) was empty: nothing was
    /// checked on rows (a publish gate does not count it).
    #[serde(default)]
    #[cfg_attr(feature = "utoipa", schema(required = true))]
    pub nothing_checked: bool,
    /// Tables the migration touches that held no rows (`schema.name`).
    #[serde(default)]
    #[cfg_attr(feature = "utoipa", schema(required = true))]
    pub empty_tables: Vec<String>,
    /// What the checks do not compare (grants, owners…), to say so.
    #[serde(default)]
    #[cfg_attr(feature = "utoipa", schema(required = true))]
    pub not_compared: Vec<String>,
    /// Where the lock list of each statement comes from: `pg_locks` before
    /// its `COMMIT` (exact), documented for those outside a transaction.
    pub locks_from: String,
    pub duration_ms: u64,
}

impl Report {
    /// The phase `phase`, if the run got there.
    pub fn phase(&self, phase: Phase) -> Option<&PhaseReport> {
        self.phases.iter().find(|p| p.phase == phase)
    }

    /// The statement that failed, if one did: its direction and result.
    pub fn failed_statement(&self) -> Option<(Direction, &StatementResult)> {
        let up = self
            .up
            .failed_at
            .and_then(|i| self.up.statements.get(i))
            .map(|s| (Direction::Up, s));
        up.or_else(|| {
            let down = self.down.as_ref()?;
            down.failed_at
                .and_then(|i| down.statements.get(i))
                .map(|s| (Direction::Down, s))
        })
    }
}

/// What a run tells as it goes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = MigrationTestEvent))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A phase starts.
    Phase { phase: Phase },
    /// Statement `index` (of `total`) of a direction starts.
    Running {
        direction: Direction,
        index: usize,
        total: usize,
    },
    Statement {
        direction: Direction,
        result: Box<StatementResult>,
    },
    /// A phase ended.
    PhaseDone { phase: Box<PhaseReport> },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_count_utf16_for_the_browser() {
        let text = "SELECT 'é😀', 1";
        let span = TextSpan::of(text, 14..17);
        assert_eq!((span.start, span.end), (14, 17));
        assert_eq!((span.utf16_start, span.utf16_end), (11, 14));
    }

    #[test]
    fn the_worse_status_wins() {
        assert_eq!(Status::Passed.and(Status::Warning), Status::Warning);
        assert_eq!(
            Status::Warning.and(Status::NeedsAcknowledgement),
            Status::NeedsAcknowledgement
        );
        assert_eq!(
            Status::Failed.and(Status::NeedsAcknowledgement),
            Status::Failed
        );
        assert_eq!(Status::Failed.and(Status::Warning), Status::Failed);
        assert_eq!(Status::Skipped.and(Status::Passed), Status::Passed);
    }
}
