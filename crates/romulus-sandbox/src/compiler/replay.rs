//! Replaying migrations: one version per migration, from an empty
//! database, in one sandbox session.
//!
//! 1. Each migration is analysed on its own ([`crate::import::analyze`]):
//!    skipped statements are blanked, syntax errors stop the import there.
//! 2. The migrations are joined into one text (each followed by a reset of
//!    `search_path`: every migration starts like a fresh session), parsed,
//!    rewritten for the replay ([`crate::import::statements::rewrite`]) and
//!    checked by the allowlist in import mode. A repeated `CREATE EXTENSION
//!    IF NOT EXISTS` of an installed extension is not run.
//! 3. The script is prepared once, like a compile's (stubs, policy roles,
//!    progress markers), and run whole without reading models, correcting
//!    stubs from Postgres's errors as a compile does.
//! 4. It is run again a migration at a time, the model read after each one
//!    from `from` on (earlier ones are already versions: they run as one
//!    step). A migration with nothing left to run has its predecessor's
//!    model.
//!
//! Everything is rolled back; a failure names its migration, located in
//! that migration's text. The whole replay has a deadline
//! ([`SandboxConfig::replay_timeout`]); at most one runs at a time, and it
//! counts as an upload for the pool's share.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use pg_query::NodeEnum;
use remus_core::Schema;
use serde::Serialize;
use tokio::sync::mpsc;

use super::{
    Inner, Lease, MODEL_DATABASE, SandboxError, bump, millis, readable,
    session::{self, AttemptError, Delivery, Ran, Settings, Slot, Step},
};
use crate::{
    allowlist,
    diagnostic::{Code, Diagnostic},
    import::{
        self, SkipReason, Skipped,
        statements::{self, Rewrite},
    },
    names::QualifiedName,
    parse::{ParsedSql, parse},
    prepare::{Edits, Injection, Prepared, prepare_with},
    refine,
    source::LineIndex,
    stubs::{Overrides, Stub},
};

/// Installs of one extension an import may make (a history may drop one and
/// create it again).
const MAX_INSTALLS: usize = 2;

/// What runs between two migrations: a fresh session's search path.
const SEPARATOR: &str = "\n;\nSET search_path TO \"$user\", public;\n";

/// What a replay reports as it goes.
#[derive(Debug)]
pub enum ReplayEvent {
    /// Every migration ran once: the import replays; models come next.
    Checked,
    /// Migration `index` replayed; its model (canonical, `database` set to
    /// [`MODEL_DATABASE`]).
    Replayed { index: usize, model: Box<Schema> },
}

/// Why a replay stopped at a migration.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayFailure {
    /// The migration (index into the replayed list).
    pub index: usize,
    /// Errors first, located in that migration's text.
    pub diagnostics: Vec<Diagnostic>,
}

/// A replay's outcome. Models arrive as [`ReplayEvent::Replayed`].
#[derive(Debug, Clone, Serialize)]
pub struct Replay {
    /// Per migration: what was not replayed.
    pub skipped: Vec<Vec<Skipped>>,
    /// Per migration: info notes (statements an extension made redundant).
    pub notes: Vec<Vec<Diagnostic>>,
    /// Functions called but defined nowhere, stubbed.
    pub stubs: Vec<Stub>,
    /// `None` when every migration replayed.
    pub failure: Option<ReplayFailure>,
    pub duration_ms: u64,
}

impl Replay {
    fn failed(skipped: Vec<Vec<Skipped>>, index: usize, diagnostics: Vec<Diagnostic>) -> Self {
        let notes = vec![Vec::new(); skipped.len()];
        Self {
            skipped,
            notes,
            stubs: Vec::new(),
            failure: Some(ReplayFailure { index, diagnostics }),
            duration_ms: 0,
        }
    }
}

/// The joined text and where each migration sits in it.
struct Joined {
    text: String,
    /// Byte range of each migration in `text`.
    ranges: Vec<Range<usize>>,
}

impl Joined {
    fn new(texts: &[String]) -> Self {
        let mut text = String::with_capacity(texts.iter().map(|t| t.len() + SEPARATOR.len()).sum());
        let mut ranges = Vec::with_capacity(texts.len());
        for migration in texts {
            let start = text.len();
            text.push_str(migration);
            ranges.push(start..text.len());
            text.push_str(SEPARATOR);
        }
        Self { text, ranges }
    }

    /// The migration a byte offset of the joined text belongs to (a
    /// separator belongs to the migration before it).
    fn migration_at(&self, offset: usize) -> usize {
        self.ranges
            .partition_point(|r| r.start <= offset)
            .saturating_sub(1)
    }

    /// A diagnostic located in the joined text, moved into migration `k`'s
    /// own text (`original`: the migration as given, same offsets).
    fn localize(
        &self,
        k: usize,
        original: &str,
        parsed: &ParsedSql,
        mut d: Diagnostic,
    ) -> Diagnostic {
        let range = &self.ranges[k];
        if let Some(span) = d.span {
            let start = span.start.clamp(range.start, range.end) - range.start;
            let end = span.end.clamp(range.start, range.end) - range.start;
            d.span = Some(LineIndex::new(original).span(original, start..end.max(start)));
        }
        d.statement = d.statement.map(|i| {
            parsed
                .statements
                .iter()
                .take(i)
                .filter(|s| range.contains(&s.span.start))
                .count()
        });
        d
    }
}

impl Inner {
    /// See the module documentation.
    #[allow(clippy::too_many_lines)]
    pub(super) async fn replay(
        &self,
        migrations: Vec<String>,
        from: usize,
        events: &mpsc::Sender<ReplayEvent>,
    ) -> Result<Replay, SandboxError> {
        if self.prefix_lost.load(Ordering::Relaxed) {
            return Err(SandboxError::PrefixInUse(self.config.prefix.clone()));
        }
        let started = Instant::now();
        let total: usize = migrations.iter().map(String::len).sum();
        if total > self.config.max_import_bytes {
            let diagnostic = Diagnostic::error(
                Code::TooLarge,
                format!(
                    "the migrations hold {} KiB of SQL; an import replays at most {} KiB",
                    total.div_ceil(1024),
                    self.config.max_import_bytes / 1024
                ),
            );
            let last = migrations.len().saturating_sub(1);
            return Ok(Replay::failed(
                vec![Vec::new(); migrations.len()],
                last,
                vec![diagnostic],
            ));
        }

        // Parsing and checking megabytes of SQL: off the async workers.
        let catalog = Arc::clone(&self.catalog);
        let max_statements = self.config.max_import_statements;
        let checked =
            tokio::task::spawn_blocking(move || check(migrations, &catalog, max_statements))
                .await
                .map_err(|e| SandboxError::Unavailable(format!("parser task failed: {e}")))?;
        let Checked {
            migrations,
            analyses,
            joined,
            parsed,
            edits,
        } = match checked {
            Ok(checked) => checked,
            Err(failed) => return Ok(*failed),
        };
        let skipped: Vec<Vec<Skipped>> = analyses.iter().map(|a| a.skipped.clone()).collect();

        let permit = tokio::time::timeout(self.config.acquire_timeout, self.permits.acquire())
            .await
            .map_err(|_| {
                bump(&self.counters.unavailable);
                SandboxError::Busy(self.config.acquire_timeout)
            })?
            .map_err(|_| SandboxError::Unavailable("the compiler is closed".into()))?;
        let mut lease = Lease {
            inner: self,
            slot: super::lock(&self.slots).pop(),
        };
        let Some(slot) = lease.slot.as_mut() else {
            return Err(SandboxError::Unavailable("no pool database left".into()));
        };
        self.reap_orphans().await;
        bump(&self.counters.sandbox_runs);

        let pid = Arc::clone(&slot.pid);
        // The deadline counts the sandbox's work, not the time spent
        // waiting for the receiver to take a model (the caller's: comparing
        // and storing it); a receiver that stops taking them is caught by
        // the wall-clock cap.
        let clock = Arc::new(Stopwatch::default());
        let mut work = Box::pin(self.replay_on(
            slot,
            &migrations,
            &skipped,
            &joined,
            &parsed,
            &edits,
            from,
            events,
            &clock,
        ));
        let limit = self.config.replay_timeout;
        let wall = limit * WALL_FACTOR;
        let outcome = loop {
            let left = limit.saturating_sub(clock.working());
            let wall_left = wall.saturating_sub(clock.started.elapsed());
            if left.is_zero() || wall_left.is_zero() {
                break Err(left.is_zero());
            }
            tokio::select! {
                outcome = &mut work => break Ok(outcome),
                () = tokio::time::sleep(left.min(wall_left)) => {}
            }
        };
        let outcome = match outcome {
            Ok(outcome) => {
                drop(work);
                outcome
            }
            Err(sandbox_time) => {
                let backend = pid.load(Ordering::Relaxed);
                self.terminate((backend != 0).then_some(backend)).await;
                let _ = tokio::time::timeout(Duration::from_secs(2), &mut work).await;
                drop(work);
                if let Some(still) = slot.discard().filter(|p| *p != backend) {
                    self.terminate(Some(still)).await;
                }
                bump(&self.counters.timeouts);
                let diagnostic = if sandbox_time {
                    Diagnostic::error(
                        Code::Timeout,
                        format!(
                            "the migrations took longer than {} to replay and were stopped",
                            readable(limit)
                        ),
                    )
                    .with_hint(
                        "Import fewer migrations at once, or look for a migration that takes long to run.",
                    )
                } else {
                    Diagnostic::error(
                        Code::Timeout,
                        format!(
                            "the import was stopped after {}: its models were not taken in time",
                            readable(wall)
                        ),
                    )
                };
                Ok(Replay::failed(
                    skipped,
                    migrations.len().saturating_sub(1),
                    vec![diagnostic],
                ))
            }
        };
        drop(lease);
        drop(permit);
        let mut replay = match outcome {
            Ok(replay) => replay,
            Err(err) => {
                bump(&self.counters.unavailable);
                return Err(err);
            }
        };
        replay.duration_ms = millis(started.elapsed());
        if replay.failure.is_some() {
            bump(&self.counters.failed);
        } else {
            bump(&self.counters.succeeded);
        }
        tracing::info!(
            migrations = migrations.len(),
            from,
            ok = replay.failure.is_none(),
            failed_at = replay.failure.as_ref().map(|f| f.index),
            ms = replay.duration_ms,
            "sandbox import replay"
        );
        Ok(replay)
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn replay_on(
        &self,
        slot: &mut Slot,
        migrations: &[String],
        skipped: &[Vec<Skipped>],
        joined: &Joined,
        parsed: &ParsedSql,
        edits: &Edits,
        from: usize,
        events: &mpsc::Sender<ReplayEvent>,
        clock: &Arc<Stopwatch>,
    ) -> Result<Replay, SandboxError> {
        let settings = Settings {
            statement_timeout_ms: self.config.statement_timeout.as_millis(),
            lock_timeout_ms: self.config.lock_timeout.as_millis(),
        };
        let fail = |failure: &session::SqlFailure, prepared: &Prepared| {
            let d =
                session::diagnostic(failure, Some(prepared), parsed, &joined.text, &self.catalog);
            let at = d
                .span
                .map(|s| s.start)
                .or_else(|| {
                    d.statement
                        .and_then(|i| parsed.statements.get(i))
                        .map(|s| s.span.start)
                })
                .unwrap_or(0);
            let k = joined.migration_at(at);
            let mut d = joined.localize(k, &migrations[k], parsed, d);
            if d.hint.is_none() {
                d.hint = skipped_hint(&d, skipped, k);
            }
            Replay::failed(skipped.to_vec(), k, vec![d])
        };

        let max_locks = self.config.max_import_locks;
        // Where phase 1 stopped over the budget: migration `k`, after
        // statement `last`.
        let too_many_locks = |k: usize, last: Option<usize>, locks: i64| {
            let mut d = Diagnostic::error(
                Code::TooLarge,
                format!(
                    "the import holds {locks} locks by this point; it may hold at most {max_locks}"
                ),
            )
            .with_hint("Everything an import creates or changes stays locked until it ends, in a lock table every compile shares: this history touches too many objects to be replayed.");
            if let Some(statement) = last.and_then(|i| parsed.statements.get(i)) {
                d = d.with_span(statement.span).with_statement(statement.index);
            }
            let d = joined.localize(k, &migrations[k], parsed, d);
            Replay::failed(skipped.to_vec(), k, vec![d])
        };

        // Phase 1: the whole script, in parts of at most `LOCK_CHECK_EVERY`
        // statements within each migration (the lock budget is checked
        // after each part), correcting stubs. Phase 2 runs the same script,
        // and locks only accumulate in a transaction: what phase 1 checked
        // holds for it.
        let mut overrides = Overrides::default();
        let mut assumed: Vec<String> = Vec::new();
        let mut round = 0;
        let mut connection_retries = 0;
        let prepared = loop {
            let prepared = prepare_with(&joined.text, parsed, &self.catalog, &overrides, edits);
            let starts = migration_starts(joined, parsed, &prepared);
            let mut each = Vec::new();
            let mut part_of = Vec::new();
            for k in 0..migrations.len() {
                for (range, last) in parts(joined, parsed, &prepared, &starts, k) {
                    each.push(Step {
                        range,
                        introspect: None,
                    });
                    part_of.push((k, last));
                }
            }
            match slot
                .replay(
                    &self.options,
                    &settings,
                    &assumed,
                    &prepared.script,
                    &each,
                    false,
                    max_locks,
                    &mut |_, _, _| Box::pin(std::future::ready(true)),
                )
                .await
            {
                Ok(Ran::All) => break prepared,
                // Phase 1 hands nothing on.
                Ok(Ran::Abandoned) => return Err(SandboxError::Abandoned),
                Ok(Ran::TooManyLocks { step, locks }) => {
                    let (k, last) = part_of[step];
                    return Ok(too_many_locks(k, last, locks));
                }
                Err(AttemptError::Sql(failure)) => {
                    // An allowlisted extension's object the history uses
                    // but never creates: the database it ran on had it.
                    if let Some(extension) = missing_extension(&failure.failure, &self.catalog)
                        && !assumed.contains(&extension)
                        && assumed.len() < MAX_ASSUMED
                    {
                        assumed.push(extension);
                        continue;
                    }
                    let failed_stub = failure
                        .failure
                        .position
                        .map(|p| prepared.locate(p, parsed))
                        .and_then(|l| match l.injection {
                            Some(Injection::Stub(k)) => prepared.stubs.get(k),
                            _ => None,
                        });
                    let learnt = match failed_stub {
                        Some(stub) => overrides.skip(stub),
                        None => refine::learn(
                            &failure.failure,
                            failure.statement,
                            &prepared,
                            parsed,
                            &joined.text,
                            &mut overrides,
                        ),
                    };
                    if round < self.config.max_stub_rounds && learnt {
                        round += 1;
                        bump(&self.counters.stub_retries);
                        continue;
                    }
                    return Ok(fail(&failure, &prepared));
                }
                Err(AttemptError::Connection { error, retry }) => {
                    slot.discard();
                    if retry && connection_retries < super::CONNECTION_RETRIES {
                        tokio::time::sleep(Duration::from_millis(100 << connection_retries)).await;
                        connection_retries += 1;
                        continue;
                    }
                    return Err(SandboxError::Unavailable(error.to_string()));
                }
            }
        };
        if clock
            .waiting(events.send(ReplayEvent::Checked))
            .await
            .is_err()
        {
            return Err(SandboxError::Abandoned);
        }

        // Phase 2: a migration at a time, models from `from` on, sent as
        // they are read.
        let starts = migration_starts(joined, parsed, &prepared);
        let mut steps = Vec::new();
        // The migration each step ends with.
        let mut step_migration = Vec::new();
        let first = from.min(migrations.len());
        if first > 0 {
            steps.push(Step {
                range: 0..starts[first],
                introspect: None,
            });
            step_migration.push(first - 1);
        }
        // A migration with nothing to run still runs its part of the
        // script (the search path reset after it), without a model read.
        let mut empty = HashSet::new();
        for k in first..migrations.len() {
            let runs = statements_in(joined, parsed, k).any(|i| !edits.omit.contains(&i));
            let shares = !runs && k > first;
            if shares {
                empty.insert(k);
            }
            steps.push(Step {
                range: starts[k]..starts[k + 1],
                introspect: (!shares).then_some(k),
            });
            step_migration.push(k);
        }
        let mut read = 0;
        let mut unreadable = None;
        let mut emit = |k: usize, json: String, roles: Vec<session::PolicyRoles>| -> Delivery {
            match session::postprocess(&json, &roles, MODEL_DATABASE) {
                Ok(model) => {
                    // Migrations with nothing to run after it share it.
                    let shared = (k + 1..migrations.len())
                        .take_while(|j| empty.contains(j))
                        .count();
                    read += shared + 1;
                    let events = events.clone();
                    let clock = Arc::clone(clock);
                    let model = Box::new(model);
                    Box::pin(async move {
                        for index in k..=k + shared {
                            let event = ReplayEvent::Replayed {
                                index,
                                model: model.clone(),
                            };
                            if clock.waiting(events.send(event)).await.is_err() {
                                return false;
                            }
                        }
                        true
                    })
                }
                Err(e) => {
                    unreadable = Some(e.to_string());
                    Box::pin(std::future::ready(true))
                }
            }
        };
        let ran = slot
            .replay(
                &self.options,
                &settings,
                &assumed,
                &prepared.script,
                &steps,
                prepared.records_policy_roles,
                max_locks,
                &mut emit,
            )
            .await;
        match ran {
            Ok(Ran::All) => {}
            Ok(Ran::Abandoned) => return Err(SandboxError::Abandoned),
            Ok(Ran::TooManyLocks { step, locks }) => {
                // Cannot happen after phase 1 (the same script); kept as
                // a stop all the same.
                return Ok(too_many_locks(step_migration[step], None, locks));
            }
            Err(AttemptError::Sql(failure)) => return Ok(fail(&failure, &prepared)),
            Err(AttemptError::Connection { error, .. }) => {
                slot.discard();
                return Err(SandboxError::Unavailable(error.to_string()));
            }
        }
        if let Some(error) = unreadable {
            return Err(SandboxError::Unavailable(format!(
                "unreadable model: {error}"
            )));
        }
        if read != migrations.len() - first {
            return Err(SandboxError::Unavailable(format!(
                "{read} models were read for {} migrations",
                migrations.len() - first
            )));
        }

        let mut notes = vec![Vec::new(); migrations.len()];
        for note in &prepared.notes {
            let at = note.span.map_or(0, |s| s.start);
            let k = joined.migration_at(at);
            notes[k].push(joined.localize(k, &migrations[k], parsed, note.clone()));
        }
        Ok(Replay {
            skipped: skipped.to_vec(),
            notes,
            stubs: prepared.stubs.clone(),
            failure: None,
            duration_ms: 0,
        })
    }
}

/// Where each migration's part of the script starts, plus the end.
/// Extensions a replay may install first, at most (each is one install).
const MAX_ASSUMED: usize = 4;

/// The allowlisted extension a failure says is missing: an operator class
/// (`operator class "public.gin_trgm_ops" does not exist for access method
/// "gin"`) or a type of one, which the history uses but never creates (it
/// was there before its first migration, as pg_dump-made baselines with
/// their `CREATE EXTENSION` removed assume).
fn missing_extension(
    failure: &refine::Failure,
    catalog: &crate::catalog::Catalog,
) -> Option<String> {
    if failure.sqlstate != "42704" {
        return None;
    }
    let unqualified = |name: &str| name.rsplit('.').next().unwrap_or(name).to_owned();
    let message = failure.message.as_str();
    if let Some(rest) = message.strip_prefix("operator class \"") {
        let (name, rest) = rest.split_once('"')?;
        let method = rest
            .strip_prefix(" does not exist for access method \"")?
            .strip_suffix('"')?;
        return catalog
            .extension_with_opclass(method, &unqualified(name))
            .map(str::to_owned);
    }
    let name = message
        .strip_prefix("type \"")?
        .strip_suffix("\" does not exist")?;
    catalog
        .extension_with_type(&unqualified(name))
        .map(str::to_owned)
}

/// The wall-clock cap of a replay, in replay timeouts: the timeout counts
/// the sandbox's work only, and this stops a replay whose receiver stopped
/// taking its models.
const WALL_FACTOR: u32 = 3;

/// A replay's time: since it started, and the part spent waiting for the
/// receiver to take a model (`waiting`), which is not the sandbox's.
struct Stopwatch {
    started: Instant,
    /// Time waited so far, and since when it waits now.
    waited: std::sync::Mutex<(Duration, Option<Instant>)>,
}

impl Default for Stopwatch {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            waited: std::sync::Mutex::new((Duration::ZERO, None)),
        }
    }
}

impl Stopwatch {
    /// Runs `send`, counting its time as waited.
    async fn waiting<T>(&self, send: impl Future<Output = T>) -> T {
        self.lock().1 = Some(Instant::now());
        let out = send.await;
        let mut waited = self.lock();
        if let Some(since) = waited.1.take() {
            waited.0 += since.elapsed();
        }
        out
    }

    /// The time the replay worked: elapsed, minus waited.
    fn working(&self) -> Duration {
        let waited = self.lock();
        let now = waited.1.map_or(Duration::ZERO, |since| since.elapsed());
        self.started.elapsed().saturating_sub(waited.0 + now)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (Duration, Option<Instant>)> {
        self.waited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Statements run between two checks of the lock budget, at most: a
/// migration of thousands of statements is checked as it runs, not only
/// after it.
const LOCK_CHECK_EVERY: usize = 200;

/// Migration `k`'s part of the script cut before every
/// [`LOCK_CHECK_EVERY`]-th statement, with the last statement of each part.
fn parts(
    joined: &Joined,
    parsed: &ParsedSql,
    prepared: &Prepared,
    starts: &[usize],
    k: usize,
) -> Vec<(Range<usize>, Option<usize>)> {
    let (start, end) = (starts[k], starts[k + 1]);
    let statements: Vec<usize> = statements_in(joined, parsed, k).collect();
    let mut out = Vec::new();
    let mut from = start;
    for chunk in statements.chunks(LOCK_CHECK_EVERY) {
        let last = chunk.last().copied();
        let next = statements
            .get(out.len() * LOCK_CHECK_EVERY + chunk.len())
            .map_or(end, |&i| prepared.statement_starts[i].clamp(from, end));
        out.push((from..next, last));
        from = next;
    }
    match out.last_mut() {
        // The rest of the migration's script (its search path reset) with
        // the last part.
        Some((range, _)) => range.end = end,
        None => out.push((start..end, None)),
    }
    out
}

fn migration_starts(joined: &Joined, parsed: &ParsedSql, prepared: &Prepared) -> Vec<usize> {
    let mut starts = Vec::with_capacity(joined.ranges.len() + 1);
    for (k, range) in joined.ranges.iter().enumerate() {
        let at = parsed
            .statements
            .iter()
            .find(|s| s.span.start >= range.start)
            .map_or(prepared.script.len(), |s| {
                prepared.statement_starts[s.index]
            });
        starts.push(if k == 0 { 0 } else { at });
    }
    starts.push(prepared.script.len());
    // Never backwards (a migration without statements starts where the
    // next one does).
    for k in (0..starts.len() - 1).rev() {
        starts[k] = starts[k].min(starts[k + 1]);
    }
    starts
}

/// The statements of migration `k`.
fn statements_in<'a>(
    joined: &'a Joined,
    parsed: &'a ParsedSql,
    k: usize,
) -> impl Iterator<Item = usize> + 'a {
    let range = joined.ranges[k].clone();
    parsed
        .statements
        .iter()
        .filter(move |s| range.contains(&s.span.start))
        .map(|s| s.index)
}

/// "relation does not exist" after a skipped `DO` block (or other code)
/// that may have created it.
fn skipped_hint(d: &Diagnostic, skipped: &[Vec<Skipped>], k: usize) -> Option<String> {
    let missing = d
        .sqlstate
        .as_deref()
        .is_some_and(|s| matches!(s, "42P01" | "42704" | "42883" | "3F000" | "42703"));
    if !missing {
        return None;
    }
    let (j, statement) = skipped[..=k]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(j, list)| {
            list.iter()
                .rev()
                .find(|s| matches!(s.reason, SkipReason::Code | SkipReason::Temporary))
                .map(|s| (j, s))
        })?;
    let place = if j == k {
        format!("line {}", statement.line)
    } else {
        format!("an earlier migration (line {})", statement.line)
    };
    Some(if statement.reason == SkipReason::Temporary {
        format!(
            "The import skips temporary tables, views and sequences ({place}: `{}`) and what only uses them: a lasting object built from one cannot be replayed.",
            statement.text
        )
    } else {
        format!(
            "The import does not run {} statements ({place} was skipped): if it created this, move that DDL out of it.",
            statement.kind
        )
    })
}

struct Checked {
    migrations: Vec<String>,
    analyses: Vec<import::Analysis>,
    joined: Joined,
    parsed: ParsedSql,
    edits: Edits,
}

/// Steps 1 and 2 of the module documentation.
#[allow(clippy::too_many_lines)]
fn check(
    migrations: Vec<String>,
    catalog: &crate::catalog::Catalog,
    max_statements: usize,
) -> Result<Checked, Box<Replay>> {
    let analyses: Vec<import::Analysis> = migrations.iter().map(|m| import::analyze(m)).collect();
    let skipped: Vec<Vec<Skipped>> = analyses.iter().map(|a| a.skipped.clone()).collect();
    if let Some((k, analysis)) = analyses
        .iter()
        .enumerate()
        .find(|(_, a)| !a.errors.is_empty())
    {
        return Err(Box::new(Replay::failed(
            skipped,
            k,
            analysis.errors.clone(),
        )));
    }
    let texts: Vec<String> = analyses.iter().map(|a| a.sql.clone()).collect();
    let joined = Joined::new(&texts);
    let parsed = parse(&joined.text);
    if let Some(error) = parsed.errors.first() {
        let k = joined.migration_at(error.span.map_or(0, |s| s.start));
        let d = joined.localize(k, &migrations[k], &parsed, error.clone());
        return Err(Box::new(Replay::failed(skipped, k, vec![d])));
    }
    if parsed.statements.len() > max_statements {
        let d = Diagnostic::error(
            Code::TooManyStatements,
            format!(
                "the migrations hold {} statements; an import replays at most {max_statements}",
                parsed.statements.len()
            ),
        );
        return Err(Box::new(Replay::failed(
            skipped,
            migrations.len() - 1,
            vec![d],
        )));
    }

    let mut edits = Edits::default();
    // Statements unwrapped from guarded DO blocks, where they are, with
    // what their block ignored.
    let guarded: Vec<(Range<usize>, &Vec<String>)> = analyses
        .iter()
        .zip(&joined.ranges)
        .flat_map(|(a, range)| {
            a.guarded.iter().map(move |(g, conditions)| {
                (range.start + g.start..range.start + g.end, conditions)
            })
        })
        .collect();
    let mut rewritten: HashMap<usize, NodeEnum> = HashMap::new();
    let mut installed: HashSet<String> = HashSet::new();
    let mut installs: HashMap<String, usize> = HashMap::new();
    for statement in &parsed.statements {
        let k = joined.migration_at(statement.span.start);
        if let Some((_, conditions)) = guarded
            .iter()
            .find(|(g, _)| g.contains(&statement.span.start))
        {
            edits.guarded.insert(statement.index, (*conditions).clone());
        }
        match statements::rewrite(statement, &parsed, &joined.text) {
            Ok(Some(Rewrite {
                edits: ranges,
                node,
            })) => {
                edits.replace.insert(statement.index, ranges);
                if let Some(node) = node {
                    rewritten.insert(statement.index, node);
                }
            }
            Ok(None) => {}
            Err(message) => {
                let d = Diagnostic::error(Code::NotAllowed, message)
                    .with_span(statement.span)
                    .with_statement(statement.index);
                let d = joined.localize(k, &migrations[k], &parsed, d);
                return Err(Box::new(Replay::failed(skipped, k, vec![d])));
            }
        }
        match &statement.node {
            NodeEnum::CreateFunctionStmt(s) => {
                if let Some(name) = QualifiedName::from_list(&s.funcname) {
                    edits.defined.insert(name.resolve(&statement.search_path));
                }
            }
            NodeEnum::CreateExtensionStmt(s) => {
                if installed.contains(&s.extname) && s.if_not_exists {
                    edits.omit.insert(statement.index);
                    continue;
                }
                // CASCADE installs what it requires too: each install counts.
                let cascade = s.options.iter().any(|o| {
                    matches!(o.node.as_ref(), Some(NodeEnum::DefElem(d)) if d.defname == "cascade")
                });
                let names = if cascade {
                    catalog.with_requirements(&s.extname)
                } else {
                    vec![s.extname.clone()]
                };
                for name in names {
                    if !installed.insert(name.clone()) {
                        continue;
                    }
                    // Installing one locks thousands of objects until the
                    // replay ends: twice is a history that dropped it once.
                    let count = installs.entry(name.clone()).or_insert(0);
                    *count += 1;
                    if *count > MAX_INSTALLS {
                        let d = Diagnostic::error(
                            Code::NotAllowed,
                            format!(
                                "extension \"{name}\" is installed more than {MAX_INSTALLS} times in these migrations"
                            ),
                        )
                        .with_hint("Each install locks thousands of objects for the whole import.")
                        .with_span(statement.span)
                        .with_statement(statement.index);
                        let d = joined.localize(k, &migrations[k], &parsed, d);
                        return Err(Box::new(Replay::failed(skipped, k, vec![d])));
                    }
                }
            }
            NodeEnum::DropStmt(s)
                if s.remove_type() == pg_query::protobuf::ObjectType::ObjectExtension =>
            {
                for object in &s.objects {
                    if let Some(name) = crate::names::string(object) {
                        installed.remove(name);
                        // With CASCADE, what requires it goes too.
                        installed.retain(|other| {
                            !catalog.with_requirements(other).iter().any(|r| r == name)
                        });
                    }
                }
            }
            _ => {}
        }
    }
    let problems = allowlist::check_import(&parsed, &joined.text, catalog, &rewritten);
    if let Some(first) = problems.first() {
        let at = first.span.map_or(0, |s| s.start);
        let k = joined.migration_at(at);
        let diagnostics = problems
            .iter()
            .filter(|d| joined.migration_at(d.span.map_or(0, |s| s.start)) == k)
            .map(|d| joined.localize(k, &migrations[k], &parsed, d.clone()))
            .collect();
        return Err(Box::new(Replay::failed(skipped, k, diagnostics)));
    }
    Ok(Checked {
        migrations,
        analyses,
        joined,
        parsed,
        edits,
    })
}

#[cfg(test)]
mod tests {
    use super::{Joined, check};
    use crate::{catalog::Catalog, prepare::prepare_with, stubs::Overrides};

    fn script(migrations: &[&str]) -> String {
        let catalog = Catalog::new(["now", "lower", "repeat"].map(str::to_owned), []);
        let checked = check(
            migrations.iter().map(|m| (*m).to_owned()).collect(),
            &catalog,
            1000,
        )
        .unwrap_or_else(|failed| panic!("{:?}", failed.failure));
        prepare_with(
            &checked.joined.text,
            &checked.parsed,
            &catalog,
            &Overrides::default(),
            &checked.edits,
        )
        .script
    }

    /// The invariant the sandbox's safety rests on in import mode: no body
    /// a migration wrote reaches the script, however it is spelled.
    #[test]
    fn no_function_body_of_the_import_reaches_the_script() {
        let script = script(&[
            "CREATE FUNCTION a() RETURNS int LANGUAGE sql AS 'SELECT 1 /* USER-CODE-1 */';",
            "CREATE OR REPLACE FUNCTION b() RETURNS trigger LANGUAGE plpgsql AS $body$ BEGIN RAISE 'USER-CODE-2'; END $body$;\nCREATE PROCEDURE c() LANGUAGE sql BEGIN ATOMIC SELECT 'USER-CODE-3'; END;",
            "CREATE FUNCTION d(x int) RETURNS int LANGUAGE sql IMMUTABLE RETURN x + length('USER-CODE-4');\nCREATE FUNCTION e() RETURNS int LANGUAGE c AS 'USER-CODE-5', 'symbol';",
            "CREATE TABLE t (x int);\nCREATE TRIGGER g BEFORE INSERT ON t FOR EACH ROW WHEN (NEW.x::text = 'USER-CODE-6') EXECUTE FUNCTION b();\nCREATE RULE r AS ON INSERT TO t DO ALSO SELECT 'USER-CODE-7';\nCREATE TABLE u AS SELECT 'USER-CODE-8' AS v;",
        ]);
        for k in 1..=7 {
            assert!(
                !script.contains(&format!("USER-CODE-{k}")),
                "USER-CODE-{k} reached the script:\n{script}"
            );
        }
        // A CREATE TABLE … AS keeps its query (it is never run: WITH NO DATA).
        assert!(script.contains("USER-CODE-8' AS v WITH NO DATA"));
    }

    #[test]
    fn migrations_are_located_in_the_joined_text() {
        let joined = Joined::new(&[
            "CREATE TABLE a ();".to_owned(),
            String::new(),
            "x".to_owned(),
        ]);
        assert_eq!(joined.migration_at(0), 0);
        assert_eq!(joined.migration_at(joined.ranges[2].start), 2);
        // The separator after migration 0 belongs to it.
        assert_eq!(joined.migration_at(joined.ranges[0].end + 2), 0);
    }

    fn refused(migrations: &[&str]) -> Vec<String> {
        let catalog = Catalog::new(["now", "lower", "repeat"].map(str::to_owned), []);
        let failed = check(
            migrations.iter().map(|m| (*m).to_owned()).collect(),
            &catalog,
            1000,
        )
        .err()
        .unwrap_or_else(|| panic!("{migrations:?} passed"));
        failed
            .failure
            .expect("a failure")
            .diagnostics
            .into_iter()
            .map(|d| d.message)
            .collect()
    }

    /// The guard's dollar quote cannot be ended by the statement it wraps,
    /// whatever tags the statement spells.
    #[test]
    fn a_guarded_statement_cannot_end_its_guard() {
        let script = script(&[
            "DO $$ BEGIN CREATE TABLE t (a text DEFAULT '$romulus_guard$ $romulus_guard0$; SELECT 1; --'); EXCEPTION WHEN duplicate_table THEN NULL; END $$;",
        ]);
        let guard = script
            .lines()
            .find(|l| l.starts_with("DO "))
            .expect("the guarded statement");
        assert_eq!(
            guard,
            "DO $romulus_guard1$ BEGIN EXECUTE 'CREATE TABLE t (a text DEFAULT ''$romulus_guard$ $romulus_guard0$; SELECT 1; --'')'; EXCEPTION WHEN duplicate_table THEN NULL; END $romulus_guard1$;"
        );
    }

    /// What a migration may not do to the sandbox, stripped where a stub
    /// keeps the schema and skipped where nothing does (the security
    /// review's probes).
    #[test]
    fn what_could_escape_a_stub_is_stripped_or_skipped() {
        let script = script(&[
            "CREATE FUNCTION f(a int DEFAULT 1) RETURNS int LANGUAGE sql SECURITY DEFINER SET search_path = public AS 'SELECT 1';\n\
             CREATE FUNCTION w() RETURNS int LANGUAGE plpgsql WINDOW AS $$ BEGIN RETURN 1; END $$;\n\
             CREATE FUNCTION i() RETURNS int LANGUAGE internal AS 'pg_sleep';\n\
             CREATE FUNCTION c(int) RETURNS int AS 'MODULE', 'sym' LANGUAGE C STRICT;\n\
             CREATE PROCEDURE p() LANGUAGE plpgsql SET work_mem = '1GB' AS $$ BEGIN END $$;\n\
             CREATE OR REPLACE FUNCTION s() RETURNS SETOF record LANGUAGE sql AS 'select 1';",
            "ALTER FUNCTION f(int) SECURITY DEFINER;\nALTER FUNCTION f(int) RESET ALL;\nALTER ROUTINE f(int) SET search_path = x;",
            "CREATE EVENT TRIGGER e ON ddl_command_start EXECUTE FUNCTION f();\n\
             CREATE TABLE t (a int);\n\
             CREATE RULE r AS ON SELECT TO t DO INSTEAD SELECT 1;\n\
             CREATE TEXT SEARCH DICTIONARY d (TEMPLATE = simple);\n\
             CREATE TEXT SEARCH CONFIGURATION kept (PARSER = default);\n\
             CREATE CAST (text AS int) WITH FUNCTION f(text);",
            "CREATE TABLE u AS SELECT 1 AS a /* x */ WITH DATA;\nCREATE TABLE v AS SELECT 1 AS a -- c\n;",
            "DO $$ BEGIN /* CREATE TABLE a (x int) */ CREATE TABLE a (x int); EXCEPTION WHEN others THEN NULL; END $$;",
        ]);
        let functions: Vec<&str> = script
            .lines()
            .filter(|l| {
                l.starts_with("CREATE FUNCTION")
                    || l.starts_with("CREATE OR REPLACE")
                    || l.starts_with("CREATE PROCEDURE")
            })
            .collect();
        assert_eq!(
            functions,
            [
                "CREATE FUNCTION f(a int = 1) RETURNS int LANGUAGE plpgsql AS $$BEGIN RETURN NULL; END$$;",
                "CREATE FUNCTION w() RETURNS int LANGUAGE plpgsql AS $$BEGIN RETURN NULL; END$$;",
                "CREATE FUNCTION i() RETURNS int LANGUAGE plpgsql AS $$BEGIN RETURN NULL; END$$;",
                "CREATE FUNCTION c(int) RETURNS int RETURNS NULL ON NULL INPUT LANGUAGE plpgsql AS $$BEGIN RETURN NULL; END$$;",
                "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$BEGIN RETURN; END$$;",
                "CREATE OR REPLACE FUNCTION s() RETURNS SETOF record LANGUAGE plpgsql AS $$BEGIN RETURN; END$$;",
            ]
        );
        for gone in [
            "ALTER",
            "EVENT",
            "RULE",
            "DICTIONARY",
            "CAST",
            "SECURITY",
            "search_path = x",
        ] {
            assert!(!script.contains(gone), "{gone} in\n{script}");
        }
        assert!(script.contains("CREATE TEXT SEARCH CONFIGURATION kept"));
        assert!(script.contains("CREATE TABLE u AS SELECT 1 AS a /* x */ WITH NO DATA;"));
        assert!(script.contains("CREATE TABLE v AS SELECT 1 AS a WITH NO DATA"));
        // The commented-out statement in the DO block is not unwrapped.
        assert_eq!(script.matches("CREATE TABLE a").count(), 1, "{script}");
        assert!(script.contains("EXCEPTION WHEN others THEN NULL"));
    }

    #[test]
    fn statements_that_run_queries_from_elsewhere_are_refused() {
        assert_eq!(
            refused(&["CREATE TABLE t (a int); CREATE TABLE u AS EXECUTE p;"]),
            ["CREATE TABLE … AS EXECUTE is not allowed"]
        );
        // Temporary helpers (whatever builds them) are skipped, never run.
        let script = script(&[
            "CREATE TABLE t (a int); SELECT a INTO TEMP x FROM t; CREATE TEMP TABLE y AS EXECUTE p;",
        ]);
        assert!(!script.contains("TEMP"), "{script}");
        assert!(!script.contains("EXECUTE"), "{script}");
        // A function in pg_temp is not a helper the import can skip.
        assert_eq!(
            refused(&["CREATE FUNCTION pg_temp.f() RETURNS int LANGUAGE sql AS 'SELECT 1';"]),
            ["pg_temp is reserved"]
        );
    }

    /// A guarded enum type is created behind an existence check, not in a
    /// subtransaction: values added to it later stay usable in the
    /// replay's one transaction. Other guarded statements keep the
    /// handler.
    #[test]
    fn a_guarded_enum_is_checked_for_not_caught() {
        let script = script(&[
            "DO $$ BEGIN CREATE TYPE public.kind AS ENUM ('a'); EXCEPTION WHEN duplicate_object OR invalid_table_definition THEN NULL; END $$;\n\
             DO $$ BEGIN CREATE TABLE t (id int); EXCEPTION WHEN duplicate_table THEN NULL; END $$;",
        ]);
        assert!(
            script.contains(
                "IF to_regtype('public.kind') IS NULL THEN EXECUTE 'CREATE TYPE public.kind AS ENUM (''a'')'; END IF;"
            ),
            "{script}"
        );
        assert!(
            script.contains(
                "EXECUTE 'CREATE TABLE t (id int)'; EXCEPTION WHEN duplicate_table THEN NULL;"
            ),
            "{script}"
        );
    }

    #[test]
    fn a_missing_object_names_the_extension_that_has_it() {
        use crate::catalog::Extension;
        let catalog = Catalog::new(
            [],
            [
                (
                    "pg_trgm".to_owned(),
                    Extension {
                        opclasses: ["gin:gin_trgm_ops".to_owned()].into(),
                        ..Extension::default()
                    },
                ),
                (
                    "citext".to_owned(),
                    Extension {
                        types: ["citext".to_owned()].into(),
                        ..Extension::default()
                    },
                ),
            ],
        );
        let failure = |sqlstate: &str, message: &str| crate::refine::Failure {
            sqlstate: sqlstate.to_owned(),
            message: message.to_owned(),
            position: None,
        };
        let missing = |f| super::missing_extension(&f, &catalog);
        assert_eq!(
            missing(failure(
                "42704",
                "operator class \"public.gin_trgm_ops\" does not exist for access method \"gin\""
            ))
            .as_deref(),
            Some("pg_trgm")
        );
        assert_eq!(
            missing(failure("42704", "type \"citext\" does not exist")).as_deref(),
            Some("citext")
        );
        assert_eq!(
            missing(failure(
                "42704",
                "operator class \"gin_trgm_ops\" does not exist for access method \"btree\""
            )),
            None
        );
        assert_eq!(
            missing(failure("42704", "type \"mood\" does not exist")),
            None
        );
        assert_eq!(
            missing(failure("42P01", "relation \"x\" does not exist")),
            None
        );
    }
}
