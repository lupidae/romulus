//! What a run found, and what it says: each new migration with its
//! outcome, the findings (what fails the job, what only warns), the test's
//! verdict and its page; as text for a person, JSON for a program, and the
//! exit status.
//!
//! The job fails (exit 1) when romulus's test does not pass (a statement
//! failed, the schema is not what the migrations make, data changed that no
//! statement declares and nobody acknowledged it), when the migrations do
//! not apply to the database's latest version, when the files cannot be
//! imported as they are (a migration changed after it was imported), and
//! when a migration locks dangerously: it rewrites a table that exists
//! (`ACCESS EXCLUSIVE`, reads and writes wait for the whole copy) or builds
//! an index on one without `CONCURRENTLY` (writes wait), unless
//! `--allow-locks`. Data a statement says it removes (a `DROP COLUMN`) is a
//! warning. Anything that kept the test from running is exit 2.
//!
//! The shapes below are the API's JSON, read leniently: only what is used,
//! every field optional.

use std::{collections::BTreeMap, fmt::Write as _};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What `test_migration` answers for an upload (`outcome`, the migrations,
/// the draft, the compile's diagnostics, the test).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Taken {
    pub outcome: String,
    pub imported: usize,
    pub migrations: Vec<NewMigration>,
    pub draft: Option<Draft>,
    pub diagnostics: Vec<Diagnostic>,
    /// Where the draft's compile stands (`state`: `pending`, `unavailable`…).
    pub compile: Option<Value>,
    pub test: Option<Test>,
    pub page: Option<String>,
    pub reused_test: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct NewMigration {
    pub path: String,
    pub line: u32,
    pub first_statement: usize,
    pub statements: usize,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Draft {
    pub id: String,
    pub name: String,
    pub base_version: Option<i64>,
    pub reused: bool,
    pub page: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Diagnostic {
    pub path: Option<String>,
    pub line: Option<u32>,
    pub severity: Option<String>,
    pub message: String,
    pub hint: Option<String>,
}

/// A migration test (`migration_tests::MigrationTest`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Test {
    pub id: String,
    pub status: String,
    pub verdict: Option<String>,
    pub from: Option<i64>,
    pub rows_checked: bool,
    pub acknowledged: Option<Value>,
    pub data: Option<TestData>,
    pub report: Option<Report>,
    pub failure: Option<String>,
    pub position: Option<usize>,
}

impl Test {
    pub fn is_final(&self) -> bool {
        !matches!(self.status.as_str(), "queued" | "running" | "")
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TestData {
    pub size: String,
}

/// A test's report (`romulus_migration_check::Report`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Report {
    pub verdict: String,
    pub headline: String,
    pub rows: i64,
    pub phases: Vec<PhaseReport>,
    pub up: MigrationRun,
    pub data_loss: Vec<DataLoss>,
    pub nothing_checked: bool,
    pub empty_tables: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PhaseReport {
    pub phase: String,
    pub status: String,
    pub summary: String,
    pub checks: Vec<CheckReport>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CheckReport {
    pub kind: String,
    pub status: String,
    pub summary: String,
    pub details: Vec<String>,
    pub tables: Vec<TableCheck>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TableCheck {
    pub table: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MigrationRun {
    pub statements: Vec<StatementResult>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StatementResult {
    pub index: usize,
    pub sql: String,
    pub status: String,
    pub duration_ms: u64,
    pub locks: Vec<Lock>,
    pub rewrote: Vec<String>,
    pub error: Option<SqlError>,
    pub offending: Option<Offending>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Lock {
    pub relation: String,
    pub mode: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SqlError {
    pub sqlstate: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Offending {
    pub count: i64,
    pub sample: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DataLoss {
    pub expected: bool,
    pub status: String,
    pub message: String,
    pub cause: Option<Cause>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Cause {
    pub direction: String,
    pub index: usize,
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunResult {
    /// The new migrations passed.
    Passed,
    /// No migration of the folder is new to the database.
    NothingNew,
    /// A check failed.
    Failed,
    /// Something kept the test from running (a token, the network, romulus).
    Error,
    /// The time given ran out first.
    Timeout,
}

impl RunResult {
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Passed | Self::NothingNew => 0,
            Self::Failed => 1,
            Self::Error | Self::Timeout => 2,
        }
    }
}

/// How one migration went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Passed,
    Failed,
    /// An earlier statement failed.
    NotRun,
    /// It does not apply to the latest version (the compile's errors).
    Invalid,
    /// The test did not get to it (it could not run, or is still running).
    Untested,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Self::Passed => "PASS",
            Self::Failed | Self::Invalid => "FAIL",
            Self::NotRun => "SKIP",
            Self::Untested => "----",
        }
    }
}

/// A finding, failing the job or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// `statement_failed`, `does_not_apply`, `rewrite`, `blocking_index`,
    /// `undeclared_data_loss`, `data_loss`, `check_failed`, `not_checked`.
    pub kind: &'static str,
    /// It fails the job.
    pub fails: bool,
    /// The migration's file (as given: under the folder), and the line.
    pub path: Option<String>,
    pub line: Option<u32>,
    pub message: String,
}

/// The statement of a migration that failed.
#[derive(Debug, Clone, Serialize)]
pub struct Failure {
    /// From 1, within the migration.
    pub statement: usize,
    pub sql: String,
    pub error: Option<SqlError>,
    pub offending: Option<Offending>,
}

/// One new migration's outcome.
#[derive(Debug, Clone, Serialize)]
pub struct MigrationOutcome {
    pub path: String,
    pub status: Status,
    pub statements: usize,
    pub duration_ms: u64,
    pub failure: Option<Failure>,
}

/// A run's outcome, whole: what is printed and what the exit status says.
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub result: RunResult,
    pub exit_code: u8,
    /// `org/db`.
    pub database: String,
    pub label: Option<String>,
    pub commit: Option<String>,
    /// Migrations of the folder the database had imported already.
    pub imported: usize,
    /// The version the new migrations were tested on.
    pub base_version: Option<i64>,
    pub migrations: Vec<MigrationOutcome>,
    pub findings: Vec<Finding>,
    /// The test's verdict (`passed`, `passed_up_only`,
    /// `needs_acknowledgement`, `failed`) and its one sentence.
    pub verdict: Option<String>,
    pub headline: Option<String>,
    pub rows: Option<i64>,
    /// The test of this exact text ran before: its report.
    pub reused_test: bool,
    pub test_id: Option<String>,
    pub test_page: Option<String>,
    pub draft: Option<Draft>,
    /// Folder files that are not text, left out.
    pub skipped_files: Vec<String>,
    /// Why it could not run (`Error`, `Timeout`), with romulus's code.
    pub error: Option<ErrorInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorInfo {
    pub code: String,
    pub message: String,
}

/// What [`Outcome`]'s builders need to know besides romulus's answer.
pub struct Reporting<'a> {
    pub database: String,
    pub label: Option<String>,
    pub commit: Option<String>,
    /// The folder as given (`db/migrations`): paths are shown under it.
    pub dir: String,
    /// Each file's content, by its path in the folder: to find the line of
    /// a statement.
    pub files: &'a BTreeMap<String, String>,
    pub allow_locks: bool,
    pub skipped_files: Vec<String>,
}

impl Reporting<'_> {
    fn shown(&self, path: &str) -> String {
        if self.dir.is_empty() || self.dir == "." {
            path.to_owned()
        } else {
            format!("{}/{path}", self.dir.trim_end_matches('/'))
        }
    }

    fn blank(&self, result: RunResult) -> Outcome {
        Outcome {
            result,
            exit_code: result.exit_code(),
            database: self.database.clone(),
            label: self.label.clone(),
            commit: self.commit.clone(),
            imported: 0,
            base_version: None,
            migrations: Vec::new(),
            findings: Vec::new(),
            verdict: None,
            headline: None,
            rows: None,
            reused_test: false,
            test_id: None,
            test_page: None,
            draft: None,
            skipped_files: self.skipped_files.clone(),
            error: None,
        }
    }

    /// It could not run.
    pub fn error(&self, result: RunResult, code: &str, message: &str) -> Outcome {
        let mut outcome = self.blank(result);
        outcome.error = Some(ErrorInfo {
            code: code.to_owned(),
            message: message.to_owned(),
        });
        outcome
    }

    /// The files could not be taken as migrations (a changed one, one that
    /// does not parse): a failed check. `details` is romulus's (an
    /// `import_failed`'s `ImportFailure`).
    pub fn refused(&self, code: &str, message: &str, details: Option<&Value>) -> Outcome {
        let mut outcome = self.blank(RunResult::Failed);
        let failure = details.filter(|_| code == "import_failed");
        let path = failure
            .and_then(|d| d["path"].as_str())
            .map(|p| self.shown(p));
        let errors = failure
            .and_then(|d| d["errors"].as_array())
            .cloned()
            .unwrap_or_default();
        if errors.is_empty() {
            outcome.findings.push(Finding {
                kind: "cannot_import",
                fails: true,
                path: path.clone(),
                line: None,
                message: format!("{message} ({code})"),
            });
        }
        for error in errors {
            outcome.findings.push(Finding {
                kind: "does_not_parse",
                fails: true,
                path: path.clone(),
                line: error["line"].as_u64().and_then(|l| u32::try_from(l).ok()),
                message: error["message"].as_str().unwrap_or(message).to_owned(),
            });
        }
        outcome
    }

    /// What romulus answered: no new migration, migrations that do not
    /// apply, or a test (finished or not).
    pub fn taken(&self, taken: &Taken) -> Outcome {
        let mut outcome = self.blank(RunResult::Passed);
        outcome.imported = taken.imported;
        outcome.draft.clone_from(&taken.draft);
        outcome.base_version = taken.draft.as_ref().and_then(|d| d.base_version);
        outcome.reused_test = taken.reused_test;
        outcome.migrations = taken
            .migrations
            .iter()
            .map(|m| MigrationOutcome {
                path: self.shown(&m.path),
                status: Status::Untested,
                statements: m.statements,
                duration_ms: 0,
                failure: None,
            })
            .collect();
        if taken.outcome == "nothing_new" {
            outcome.set(RunResult::NothingNew);
            return outcome;
        }
        if matches!(taken.outcome.as_str(), "compile_failed" | "not_compiled") {
            self.does_not_apply(&mut outcome, taken);
            return outcome;
        }
        match &taken.test {
            Some(test) => self.tested(&mut outcome, taken, test),
            None => outcome.set(RunResult::Error),
        }
        outcome
    }

    /// The migrations do not apply to the latest version: the compile's
    /// errors, in their files.
    fn does_not_apply(&self, outcome: &mut Outcome, taken: &Taken) {
        if taken.outcome == "not_compiled" {
            outcome.set(RunResult::Error);
            let state = taken
                .compile
                .as_ref()
                .and_then(|c| c["state"].as_str())
                .unwrap_or("pending");
            outcome.error = Some(ErrorInfo {
                code: "not_compiled".into(),
                message: format!(
                    "romulus could not compile the migrations just now (compile {state}: its \
                     compiler is busy or unavailable): run it again in a moment"
                ),
            });
            return;
        }
        outcome.set(RunResult::Failed);
        for d in taken
            .diagnostics
            .iter()
            .filter(|d| d.severity.as_deref() != Some("info"))
        {
            let path = d.path.as_deref().map(|p| self.shown(p));
            if let Some(migration) = path
                .as_ref()
                .and_then(|p| outcome.migrations.iter_mut().find(|m| &m.path == p))
            {
                migration.status = Status::Invalid;
            }
            let mut message = d.message.clone();
            if let Some(hint) = &d.hint {
                let _ = write!(message, " (hint: {hint})");
            }
            outcome.findings.push(Finding {
                kind: "does_not_apply",
                fails: d.severity.as_deref() != Some("warning"),
                path,
                line: d.line,
                message,
            });
        }
        if !outcome.findings.iter().any(|f| f.fails) {
            outcome.findings.push(Finding {
                kind: "does_not_apply",
                fails: true,
                path: None,
                line: None,
                message: "The migrations do not apply to the latest version.".into(),
            });
        }
    }

    #[allow(clippy::too_many_lines, reason = "the report's parts, in order")]
    fn tested(&self, outcome: &mut Outcome, taken: &Taken, test: &Test) {
        outcome.test_id = Some(test.id.clone());
        outcome.test_page.clone_from(&taken.page);
        outcome.base_version = test.from.or(outcome.base_version);
        outcome.verdict.clone_from(&test.verdict);
        if !test.is_final() {
            outcome.set(RunResult::Timeout);
            return;
        }
        if matches!(test.status.as_str(), "error" | "cancelled") {
            outcome.set(RunResult::Error);
            outcome.error = Some(ErrorInfo {
                code: test.status.clone(),
                message: test
                    .failure
                    .clone()
                    .unwrap_or_else(|| format!("the test was {}", test.status)),
            });
            return;
        }
        let Some(report) = &test.report else {
            outcome.set(RunResult::Error);
            return;
        };
        outcome.headline = Some(report.headline.clone());
        outcome.rows = Some(report.rows);
        let statements = &report.up.statements;
        // Which migration a statement of the text is.
        let owner = |index: usize| {
            taken.migrations.iter().position(|m| {
                (m.first_statement..m.first_statement + m.statements).contains(&index)
            })
        };
        let line_of = |k: usize, sql: &str| {
            let m = &taken.migrations[k];
            self.files
                .get(&m.path)
                .and_then(|content| line_in(content, sql))
        };
        for (k, migration) in taken.migrations.iter().enumerate() {
            let own = statements
                .iter()
                .filter(|s| owner(s.index) == Some(k))
                .collect::<Vec<_>>();
            let shown = &mut outcome.migrations[k];
            shown.duration_ms = own.iter().map(|s| s.duration_ms).sum();
            shown.status = if let Some(failed) = own.iter().find(|s| s.status == "failed") {
                shown.failure = Some(Failure {
                    statement: failed.index - migration.first_statement + 1,
                    sql: failed.sql.clone(),
                    error: failed.error.clone(),
                    offending: failed.offending.clone(),
                });
                outcome.findings.push(Finding {
                    kind: "statement_failed",
                    fails: true,
                    path: Some(shown.path.clone()),
                    line: line_of(k, &failed.sql),
                    message: failed.error.as_ref().map_or_else(
                        || "This statement failed.".to_owned(),
                        |e| format!("{} (SQLSTATE {})", e.message, e.sqlstate),
                    ),
                });
                Status::Failed
            } else if own.iter().any(|s| s.status == "not_run")
                || (own.is_empty() && migration.statements > 0)
            {
                Status::NotRun
            } else {
                Status::Passed
            };
        }

        // Locks on tables that hold rows in production: those the base
        // schema has (counted before the migration).
        let existing: Vec<&str> = report
            .phases
            .iter()
            .filter(|p| p.phase == "before")
            .flat_map(|p| &p.checks)
            .filter(|c| c.kind == "row_counts")
            .flat_map(|c| &c.tables)
            .map(|t| t.table.as_str())
            .collect();
        let exists = |relation: &str| existing.is_empty() || existing.contains(&relation);
        for statement in statements.iter().filter(|s| s.status == "passed") {
            let Some(k) = owner(statement.index) else {
                continue;
            };
            let path = Some(outcome.migrations[k].path.clone());
            let line = line_of(k, &statement.sql);
            for table in statement.rewrote.iter().filter(|t| exists(t)) {
                outcome.findings.push(Finding {
                    kind: "rewrite",
                    fails: !self.allow_locks,
                    path: path.clone(),
                    line,
                    message: format!(
                        "rewrites {table}: an ACCESS EXCLUSIVE lock holds every read and write \
                         of it for as long as the copy takes"
                    ),
                });
            }
            if statement.rewrote.is_empty()
                && let Some(table) = blocking_index(statement, &exists)
            {
                outcome.findings.push(Finding {
                    kind: "blocking_index",
                    fails: !self.allow_locks,
                    path: path.clone(),
                    line,
                    message: format!(
                        "builds an index on {table} without CONCURRENTLY: writes to it wait \
                         until the index is built"
                    ),
                });
            }
        }

        for loss in &report.data_loss {
            let at = loss
                .cause
                .as_ref()
                .filter(|c| c.direction == "up")
                .and_then(|c| owner(c.index).map(|k| (k, c.index)));
            let path = at.map(|(k, _)| outcome.migrations[k].path.clone());
            let line = at.and_then(|(k, index)| {
                statements
                    .iter()
                    .find(|s| s.index == index)
                    .and_then(|s| line_of(k, &s.sql))
            });
            let acknowledged = test.acknowledged.is_some();
            let (kind, fails) = if loss.expected {
                ("data_loss", false)
            } else {
                (
                    "undeclared_data_loss",
                    !acknowledged && loss.status != "warning",
                )
            };
            outcome.findings.push(Finding {
                kind,
                fails,
                path,
                line,
                message: if loss.expected {
                    format!("{} (the statement says it does this)", loss.message)
                } else if acknowledged {
                    format!("{} (acknowledged in romulus)", loss.message)
                } else {
                    format!(
                        "{} No statement says it changes this data: if it is meant, \
                         acknowledge it on the test's page, then run again.",
                        loss.message
                    )
                },
            });
        }

        // Checks that failed without a statement failing (the schema after
        // the migration is not what they make).
        if !outcome
            .migrations
            .iter()
            .any(|m| m.status == Status::Failed)
        {
            for phase in report.phases.iter().filter(|p| p.status == "failed") {
                for check in phase.checks.iter().filter(|c| c.status == "failed") {
                    let mut message = check.summary.clone();
                    for detail in check.details.iter().take(5) {
                        let _ = write!(message, "\n{detail}");
                    }
                    outcome.findings.push(Finding {
                        kind: "check_failed",
                        fails: true,
                        path: None,
                        line: None,
                        message,
                    });
                }
            }
        }
        if report.nothing_checked || !test.rows_checked {
            outcome.findings.push(Finding {
                kind: "not_checked",
                fails: false,
                path: None,
                line: None,
                message: format!(
                    "Every table the migrations change was empty{}: no rows were checked.",
                    if report.empty_tables.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", report.empty_tables.join(", "))
                    }
                ),
            });
        }

        let failed = matches!(test.status.as_str(), "failed" | "needs_acknowledgement")
            || outcome.findings.iter().any(|f| f.fails);
        outcome.set(if failed {
            RunResult::Failed
        } else {
            RunResult::Passed
        });
        // A failed verdict always says why, even when no finding above did.
        if failed && !outcome.findings.iter().any(|f| f.fails) {
            outcome.findings.push(Finding {
                kind: "check_failed",
                fails: true,
                path: None,
                line: None,
                message: report.headline.clone(),
            });
        }
    }
}

impl Outcome {
    fn set(&mut self, result: RunResult) {
        self.result = result;
        self.exit_code = result.exit_code();
    }

    /// The time given ran out before the verdict.
    #[must_use]
    pub fn timed_out(mut self, message: String) -> Self {
        self.set(RunResult::Timeout);
        self.error = Some(ErrorInfo {
            code: "timeout".into(),
            message,
        });
        self
    }

    /// The outcome as text, for a person reading a CI log.
    #[allow(clippy::too_many_lines, reason = "the report's parts, in order")]
    pub fn text(&self) -> String {
        let mut out = String::new();
        let _ = write!(out, "romulus test: {}", self.database);
        match (&self.label, &self.commit) {
            (Some(label), Some(commit)) => {
                let _ = write!(out, ", {label} ({})", short(commit));
            }
            (Some(label), None) => {
                let _ = write!(out, ", {label}");
            }
            (None, Some(commit)) => {
                let _ = write!(out, " ({})", short(commit));
            }
            (None, None) => {}
        }
        out.push('\n');
        if let Some(error) = &self.error
            && self.migrations.is_empty()
        {
            let _ = writeln!(out, "\nERROR: {}", error.message);
            return out;
        }
        if self.result == RunResult::NothingNew {
            let _ = writeln!(
                out,
                "\nNo migration in the folder is new to {} ({} imported already): nothing to \
                 test.",
                self.database, self.imported
            );
            return out;
        }
        if !self.migrations.is_empty() {
            let n = self.migrations.len();
            let _ = write!(
                out,
                "{n} new migration{} ({} imported already)",
                plural(n),
                self.imported
            );
            if let Some(v) = self.base_version {
                let _ = write!(out, ", on v{v}");
            }
            if let Some(rows) = self.rows {
                let _ = write!(out, " with {rows} generated rows");
            }
            out.push_str(if self.reused_test {
                " (this exact text was tested before: its result)"
            } else {
                ""
            });
            out.push_str(":\n\n");
        }
        for m in &self.migrations {
            let _ = write!(out, "  {}  {}", m.status.word(), m.path);
            match (m.status, &m.failure) {
                (Status::Failed, Some(f)) => {
                    let _ = writeln!(
                        out,
                        ": statement {} of {} failed",
                        f.statement, m.statements
                    );
                    for line in f.sql.lines().take(6) {
                        let _ = writeln!(out, "          {line}");
                    }
                    if let Some(e) = &f.error {
                        let _ = writeln!(out, "        ERROR {}: {}", e.sqlstate, e.message);
                        for more in [&e.detail, &e.hint].into_iter().flatten() {
                            let _ = writeln!(out, "        {more}");
                        }
                    }
                    if let Some(o) = f.offending.as_ref().filter(|o| o.count > 0) {
                        let _ = writeln!(
                            out,
                            "        {} row{} break{} it: {}",
                            o.count,
                            plural(usize::try_from(o.count).unwrap_or(2)),
                            if o.count == 1 { "s" } else { "" },
                            o.sample.join(", ")
                        );
                    }
                }
                (Status::NotRun, _) => out.push_str(" (not run: a statement before it failed)\n"),
                (Status::Invalid, _) => out.push_str(" (does not apply: see below)\n"),
                (Status::Untested, _) => out.push('\n'),
                _ => {
                    let _ = writeln!(
                        out,
                        " ({} statement{}, {} ms)",
                        m.statements,
                        plural(m.statements),
                        m.duration_ms
                    );
                }
            }
        }
        let failing: Vec<&Finding> = self
            .findings
            .iter()
            .filter(|f| f.fails && f.kind != "statement_failed")
            .collect();
        let warning: Vec<&Finding> = self.findings.iter().filter(|f| !f.fails).collect();
        for (title, list) in [("Problems", failing), ("Warnings", warning)] {
            if list.is_empty() {
                continue;
            }
            let _ = writeln!(out, "\n{title}:");
            for f in list {
                let at = match (&f.path, f.line) {
                    (Some(p), Some(l)) => format!("{p}:{l}: "),
                    (Some(p), None) => format!("{p}: "),
                    _ => String::new(),
                };
                let mut lines = f.message.lines();
                let _ = writeln!(out, "  - {at}{}", lines.next().unwrap_or(""));
                for line in lines {
                    let _ = writeln!(out, "    {line}");
                }
            }
        }
        out.push('\n');
        let headline = self.headline.as_deref().unwrap_or("");
        match self.result {
            RunResult::Passed => {
                let _ = writeln!(out, "PASSED{}", sentence(headline));
            }
            RunResult::Failed => {
                let _ = writeln!(out, "FAILED{}", sentence(headline));
            }
            RunResult::Timeout => {
                let _ = writeln!(
                    out,
                    "TIMED OUT: the test is still running in romulus; its page will show the \
                     result."
                );
            }
            RunResult::Error => {
                let message = self
                    .error
                    .as_ref()
                    .map_or("the test could not run", |e| e.message.as_str());
                let _ = writeln!(out, "ERROR: {message}");
            }
            RunResult::NothingNew => {}
        }
        if let Some(page) = &self.test_page {
            let _ = writeln!(out, "Report: {page}");
        }
        if let Some(draft) = &self.draft {
            let _ = writeln!(out, "Draft:  {} ({})", draft.page, draft.name);
        }
        if !self.skipped_files.is_empty() {
            let _ = writeln!(
                out,
                "Left out (not text): {}",
                self.skipped_files.join(", ")
            );
        }
        out
    }

    /// GitHub Actions' annotations: each finding on its file and line, in
    /// the pull request's diff.
    pub fn annotations(&self) -> String {
        let mut out = String::new();
        for f in &self.findings {
            let level = if f.fails { "error" } else { "warning" };
            let mut place = String::new();
            if let Some(path) = &f.path {
                let _ = write!(place, " file={}", escape_property(path));
                if let Some(line) = f.line {
                    let _ = write!(place, ",line={line}");
                }
            }
            let _ = writeln!(
                out,
                "::{level}{place}::{}",
                escape_data(&format!("romulus: {}", f.message))
            );
        }
        out
    }

    pub fn json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }
}

/// The index a `CREATE INDEX` without `CONCURRENTLY` builds on an existing
/// table: the table it holds a `SHARE` lock on.
fn blocking_index<'a>(
    statement: &'a StatementResult,
    exists: &dyn Fn(&str) -> bool,
) -> Option<&'a str> {
    let sql = statement.sql.to_ascii_uppercase();
    let words: Vec<&str> = sql
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .flat_map(str::split_whitespace)
        .collect();
    let creates_index = matches!(
        words.as_slice(),
        ["CREATE", "INDEX", ..] | ["CREATE", "UNIQUE", "INDEX", ..]
    );
    if !creates_index || words.contains(&"CONCURRENTLY") {
        return None;
    }
    statement
        .locks
        .iter()
        .find(|l| l.mode == "ShareLock" && exists(&l.relation))
        .map(|l| l.relation.as_str())
}

/// The line `sql` (a statement as the test ran it, with its `;`) starts on
/// in `content`.
fn line_in(content: &str, sql: &str) -> Option<u32> {
    let needle = sql.trim().trim_end_matches(';').trim_end();
    if needle.is_empty() {
        return None;
    }
    let at = content.find(needle).or_else(|| {
        // A line the text changed (`--- x` written `-- - x`): the statement's
        // first line that is not a comment.
        let first = needle
            .lines()
            .find(|l| !l.trim_start().starts_with("--") && !l.trim().is_empty())?;
        content.find(first)
    })?;
    u32::try_from(content[..at].matches('\n').count() + 1).ok()
}

fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn sentence(headline: &str) -> String {
    if headline.is_empty() {
        String::new()
    } else {
        format!(": {headline}")
    }
}

/// A workflow command's message (`%`, line breaks).
fn escape_data(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// A workflow command's property (`file=`).
fn escape_property(text: &str) -> String {
    escape_data(text).replace(':', "%3A").replace(',', "%2C")
}

#[cfg(test)]
mod tests;
