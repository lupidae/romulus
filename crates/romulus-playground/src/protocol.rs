//! What the API and the playground service say to each other (HTTP on the
//! private link, every call signed: [`crate::signing`]).
//!
//! | call | body → answer |
//! |---|---|
//! | `POST /playgrounds/{id}/build` | [`Build`] → NDJSON [`BuildEvent`]s |
//! | `POST /playgrounds/{id}/migration` | [`Migrate`] → NDJSON [`MigrationEvent`]s |
//! | `POST /playgrounds/{id}/test` | [`Test`] → NDJSON [`TestEvent`]s |
//! | `POST /playgrounds/{id}/fork` | [`Fork`] → 204 (a copy of another's rows) |
//! | `POST /playgrounds/{id}/reset` | → [`Status`] |
//! | `POST /playgrounds/{id}/stop` | → [`Status`] |
//! | `DELETE /playgrounds/{id}` | → 204 |
//! | `GET /playgrounds/{id}` | → [`Status`] |
//! | `GET /playgrounds` | → [`Listing`] (every playground the host holds) |
//! | `GET /playgrounds/{id}/console` | websocket: [`ConsoleRequest`] in, [`ConsoleEvent`] out |
//! | `GET /health` | → [`Health`] (unsigned) |
//!
//! Errors are [`ErrorBody`] JSON with a stable `code`.

use serde::{Deserialize, Serialize};
#[cfg(feature = "utoipa")]
use utoipa::ToSchema;
use uuid::Uuid;

/// How many rows, roughly (docs/playground.md, "Volume presets").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Size {
    /// ~50 k rows at most, ~10 s.
    Small,
    /// ~1 M rows at most, ~1 min.
    Medium,
}

impl Size {
    /// The cap on rows across tables.
    pub fn max_rows(self) -> u64 {
        match self {
            Self::Small => 50_000,
            Self::Medium => 1_000_000,
        }
    }

    /// The disk quota of a playground of this size (the runner's
    /// `Size::disk_bytes`; a test keeps them equal).
    pub fn disk_bytes(self) -> u64 {
        match self {
            Self::Small => 1024 * 1024 * 1024,
            Self::Medium => 2 * 1024 * 1024 * 1024,
        }
    }
}

/// What the generated rows aim at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// Plausible values, skewed like real data.
    #[default]
    Realistic,
    /// The rows that break migrations: NULLs, maximum lengths, duplicates,
    /// extremes, empty and non-ASCII strings.
    EdgeCases,
}

/// Who is asking, as the API decided it (`Member`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// Reviewers: the read-only console, 30 s per statement.
    Read,
    /// Developers and admins: also migration runs, 2 min per statement.
    Develop,
}

/// An extension the playground needs (from the model), installed at init.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extension {
    pub name: String,
    pub schema: String,
}

/// Build a playground: create its cluster, apply the schema, generate rows,
/// keep the generated copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Build {
    pub size: Size,
    pub profile: Profile,
    pub seed: u64,
    /// What the schema's owner runs (`romulus_sandbox::playground::script`).
    pub schema_sql: String,
    /// The remus model the rows are generated for.
    pub model: serde_json::Value,
    pub extensions: Vec<Extension>,
}

/// Where a build is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = PlaygroundStage))]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Its cluster: init, extensions.
    Starting,
    Schema,
    Rows,
    Analyze,
    /// The generated copy Reset restores.
    Snapshot,
}

/// One table's rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = PlaygroundTableRows))]
pub struct TableRows {
    pub schema: String,
    pub table: String,
    pub rows: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BuildEvent {
    Stage {
        stage: Stage,
    },
    /// A table's rows are in (`done` of `total` tables).
    Table {
        schema: String,
        table: String,
        rows: u64,
        done: usize,
        total: usize,
    },
    Ready {
        tables: Vec<TableRows>,
        /// The generator's report (what it could not satisfy), as it wrote it.
        report: serde_json::Value,
        generator: String,
        duration_ms: u64,
    },
    Failed {
        stage: Stage,
        message: String,
    },
}

/// One statement of a migration run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStatement {
    pub sql: String,
    /// `deletes_data`, `can_fail`, `widens_access` (romulus-migration's
    /// hazards), when the statement has one.
    #[serde(default)]
    pub hazard: Option<String>,
    /// Runs outside a transaction (`CREATE INDEX CONCURRENTLY`).
    #[serde(default)]
    pub outside_transaction: bool,
}

/// Create a playground as a copy of another's rows, for a migration test on
/// them: the source pauses for the copy (its sessions are told and come
/// back) and is never changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fork {
    pub source: Uuid,
    pub size: Size,
    #[serde(default)]
    pub extensions: Vec<Extension>,
}

/// Why a playground is stopped, for the people in its console (the API
/// stopping it at the end of an organization's playground hours). A stop
/// without a body says it was stopped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stop {
    pub reason: String,
}

/// Run a migration on the generated rows: reset, then each statement in its
/// own transaction, measured.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Migrate {
    pub statements: Vec<MigrationStatement>,
}

// The statements' results, one shape for Run migration and Test migration:
// romulus-migration-check's runner makes them.
pub use romulus_migration_check::report::{
    Lock, MigrationReport, Offending, SqlError, StatementResult, StatementStatus, TextSpan,
};

/// Test a migration on the playground's rows (romulus-migration-check):
/// the database is checked to be at `base` and its rows counted and
/// sampled, `up` runs (each statement in its own transaction, measured),
/// the schema and the rows are checked against `target`, then `down` (if
/// any) and the same checks against `base`. Develop access. The playground
/// is left as the test leaves it: a test runs on a playground made for it
/// (or a copy of someone's), which the API deletes after.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Test {
    /// The remus models before and after.
    pub base: serde_json::Value,
    pub target: serde_json::Value,
    pub up: Vec<romulus_migration_check::Statement>,
    /// Tables whose columns `up` warned Postgres puts last.
    #[serde(default)]
    pub up_reordered: Vec<String>,
    /// `None`: there is no down migration (one written without it).
    #[serde(default)]
    pub down: Option<Vec<romulus_migration_check::Statement>>,
    #[serde(default)]
    pub down_reordered: Vec<String>,
    /// Rows sampled per table.
    pub sample: usize,
    /// What the report calls the two schemas (`v7`, `the draft`).
    pub base_label: String,
    pub target_label: String,
    /// What the report says the rows are.
    pub data: romulus_migration_check::DataSource,
}

/// What a test tells as it runs; ends with `done` or `failed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TestEvent {
    /// A phase or a statement, as the engine tells it.
    Progress {
        event: romulus_migration_check::Event,
    },
    Done {
        report: Box<romulus_migration_check::Report>,
    },
    /// The test could not run (the playground did not start).
    Failed { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = PlaygroundMigrationEvent))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MigrationEvent {
    /// The generated rows are being put back first.
    Resetting,
    /// Statement `index` (of `total`) started.
    Running {
        index: usize,
        total: usize,
    },
    Statement {
        result: Box<StatementResult>,
    },
    Done {
        report: MigrationReport,
    },
    /// The run itself could not happen (the playground failed).
    Failed {
        message: String,
    },
}

/// A playground as the host sees it now.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = PlaygroundLive))]
pub struct Status {
    /// Its Postgres runs (it stops after a while without a statement).
    pub running: bool,
    /// Console sessions open.
    pub sessions: usize,
    /// What it is doing, if anything: `building`, `resetting`, `migrating`,
    /// `stopping`, `restarting`. Reset, Run migration and Delete wait.
    #[serde(default)]
    pub busy: Option<String>,
    /// The last stop was its memory limit.
    #[serde(default)]
    pub oom_killed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Listed {
    pub id: Uuid,
    pub running: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Listing {
    pub playgrounds: Vec<Listed>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Health {
    pub ok: bool,
    pub runner: bool,
    pub running: usize,
    pub max_running: usize,
    /// Free and total bytes of the playground volume, when the runner could
    /// read them (the API refuses new playgrounds when it is nearly full).
    #[serde(default)]
    pub free_bytes: Option<u64>,
    #[serde(default)]
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    /// `bad_signature`, `not_found`, `busy`, `host_full`, `org_full`,
    /// `unavailable`, `bad_request`, `too_long`, `failed`.
    pub code: String,
    pub message: String,
}

/// From the browser, through the API (which checks and re-encodes it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = ConsoleRequest))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConsoleRequest {
    /// Run every statement of `sql`, in order, stopping at the first error.
    /// With `explain`, `sql` must be one statement: its plan, analyzed.
    Run {
        id: String,
        sql: String,
        #[serde(default)]
        explain: bool,
    },
    /// Stop the run `id`.
    Cancel { id: String },
    /// Start the session again after `stopped` or `unavailable` (a `run`
    /// does it too).
    Open,
}

/// A result column: its name and type, as `format_type` prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = ConsoleColumn))]
pub struct Column {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    /// Numbers align right.
    #[serde(default)]
    pub numeric: bool,
}

/// The session's transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = ConsoleTxn))]
#[serde(rename_all = "snake_case")]
pub enum Txn {
    Idle,
    InTransaction,
    /// An error ended the transaction's usefulness: `ROLLBACK`.
    Failed,
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = ConsoleRunStatus))]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Ok,
    Error,
    Cancelled,
}

/// To the browser. Every result belongs to a run (`id`) and a statement of
/// it (`index`, in the text the run sent).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(ToSchema), schema(as = ConsoleEvent))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConsoleEvent {
    /// The playground's database is starting (it stops when unused): a few
    /// seconds.
    Starting,
    /// The session is up (again, after a reset): a fresh transaction.
    Ready {
        txn: Txn,
    },
    /// The session is gone for now: someone is resetting or migrating the
    /// playground (`reason` says so). `Ready` follows by itself when it is
    /// back.
    Paused {
        reason: String,
    },
    /// The playground's database was stopped (nothing ran for a while, or
    /// another playground needed the room): a `run` or `open` starts it
    /// again, in a few seconds.
    Stopped {
        reason: String,
    },
    /// The playground is being deleted: the API closes the socket (4404)
    /// once it is done.
    Deleting,
    /// This session found a role changed from inside the playground (its
    /// password or settings) and restarted it to undo that (the API audits
    /// it; the others heard a pause).
    Restarted {
        reason: String,
    },
    /// It could not start or be reached: `code` is `host_full`, `org_full`,
    /// `not_starting` or `unreachable`. `open` (Try again) or a `run`
    /// tries again.
    Unavailable {
        code: String,
        reason: String,
    },
    /// Waiting for a free slot (statements run a few at a time per host).
    Queued {
        id: String,
    },
    /// Statement `index` of the run starts; `span` is where it is in `sql`.
    Statement {
        id: String,
        index: usize,
        span: TextSpan,
    },
    Columns {
        id: String,
        index: usize,
        columns: Vec<Column>,
    },
    /// At most 500 rows each; values as Postgres prints them, NULL as null.
    Rows {
        id: String,
        index: usize,
        rows: Vec<Vec<Option<String>>>,
    },
    /// A statement's plan (Explain).
    Plan {
        id: String,
        index: usize,
        text: String,
    },
    Notice {
        id: String,
        index: usize,
        severity: String,
        message: String,
    },
    /// A statement finished.
    Done {
        id: String,
        index: usize,
        /// Postgres's command tag (`SELECT 212`).
        tag: String,
        rows: u64,
        /// It returned more rows than were kept (1 000, or 16 MiB a run).
        truncated: bool,
        /// Some values were longer than 64 KiB and are cut.
        #[serde(default)]
        cells_cut: bool,
        ms: u64,
    },
    Error {
        id: String,
        index: usize,
        error: SqlError,
    },
    /// The run is over (the history entry the API records).
    Finished {
        id: String,
        status: RunStatus,
        statements: usize,
        /// The last statement's rows.
        rows: Option<u64>,
        ms: u64,
        #[serde(default)]
        error: Option<String>,
    },
    Txn {
        txn: Txn,
    },
}

/// Limits of the console, the same on both sides.
pub mod limits {
    /// Rows kept per statement.
    pub const ROWS: usize = 1_000;
    /// Rows per `rows` message.
    pub const ROWS_PER_MESSAGE: usize = 500;
    /// Bytes per value; longer ones are cut.
    pub const CELL_BYTES: usize = 64 * 1024;
    /// Bytes of results per run; past them the statement is stopped.
    pub const RUN_BYTES: usize = 16 * 1024 * 1024;
    /// The SQL of one run.
    pub const SQL_BYTES: usize = 256 * 1024;
    /// Statements of one run.
    pub const STATEMENTS: usize = 100;
    /// Seconds a statement may run: reviewers, then developers.
    pub const READ_DEADLINE_SECS: u64 = 30;
    pub const DEVELOP_DEADLINE_SECS: u64 = 120;
    /// Statements of one migration run.
    pub const MIGRATION_STATEMENTS: usize = 500;
    /// Seconds a whole migration run may take (each statement keeps its
    /// own deadline within it).
    pub const MIGRATION_SECS: u64 = 600;
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
    fn console_messages_are_tagged() {
        let run: ConsoleRequest =
            serde_json::from_str(r#"{"type":"run","id":"r1","sql":"select 1"}"#).unwrap();
        assert_eq!(
            run,
            ConsoleRequest::Run {
                id: "r1".to_owned(),
                sql: "select 1".to_owned(),
                explain: false
            }
        );
        let done = serde_json::to_value(ConsoleEvent::Txn { txn: Txn::Idle }).unwrap();
        assert_eq!(done, serde_json::json!({ "type": "txn", "txn": "idle" }));
    }
}
