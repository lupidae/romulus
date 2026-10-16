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

use super::{
    Inner, Lease, MODEL_DATABASE, SandboxError, bump, millis, readable,
    session::{self, AttemptError, Settings, Slot, Step},
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
        on_event: &mut (dyn FnMut(ReplayEvent) + Send),
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
        let mut work = Box::pin(self.replay_on(
            slot,
            &migrations,
            &skipped,
            &joined,
            &parsed,
            &edits,
            from,
            on_event,
        ));
        let outcome = tokio::select! {
            outcome = &mut work => Some(outcome),
            () = tokio::time::sleep(self.config.replay_timeout) => None,
        };
        let outcome = if let Some(outcome) = outcome {
            drop(work);
            outcome
        } else {
            let backend = pid.load(Ordering::Relaxed);
            self.terminate((backend != 0).then_some(backend)).await;
            let _ = tokio::time::timeout(Duration::from_secs(2), &mut work).await;
            drop(work);
            if let Some(still) = slot.discard().filter(|p| *p != backend) {
                self.terminate(Some(still)).await;
            }
            bump(&self.counters.timeouts);
            let diagnostic = Diagnostic::error(
                Code::Timeout,
                format!(
                    "the import took longer than {} and was stopped",
                    readable(self.config.replay_timeout)
                ),
            )
            .with_hint(
                "Import fewer migrations at once, or look for a migration that takes long to run.",
            );
            Ok(Replay::failed(
                skipped,
                migrations.len().saturating_sub(1),
                vec![diagnostic],
            ))
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
        on_event: &mut (dyn FnMut(ReplayEvent) + Send),
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

        // Phase 1: the whole script, correcting stubs.
        let mut overrides = Overrides::default();
        let mut round = 0;
        let mut connection_retries = 0;
        let prepared = loop {
            let prepared = prepare_with(&joined.text, parsed, &self.catalog, &overrides, edits);
            let whole = [Step {
                range: 0..prepared.script.len(),
                introspect: None,
            }];
            match slot
                .replay(
                    &self.options,
                    &settings,
                    &prepared.script,
                    &whole,
                    false,
                    &mut |_, _, _| {},
                )
                .await
            {
                Ok(()) => break prepared,
                Err(AttemptError::Sql(failure)) => {
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
        on_event(ReplayEvent::Checked);

        // Phase 2: a migration at a time, models from `from` on.
        let starts = migration_starts(joined, parsed, &prepared);
        let mut steps = Vec::new();
        let first = from.min(migrations.len());
        if first > 0 {
            steps.push(Step {
                range: 0..starts[first],
                introspect: None,
            });
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
        }
        let mut models: Vec<Option<Schema>> = vec![None; migrations.len()];
        let mut unreadable = None;
        let mut emit = |k: usize, json: String, roles: Vec<session::PolicyRoles>| {
            match session::postprocess(&json, &roles, MODEL_DATABASE) {
                Ok(model) => {
                    // Migrations with nothing to run in between share it.
                    for (j, shared) in models.iter_mut().enumerate().skip(k + 1) {
                        if !empty.contains(&j) {
                            break;
                        }
                        *shared = Some(model.clone());
                    }
                    models[k] = Some(model);
                }
                Err(e) => unreadable = Some(e.to_string()),
            }
        };
        let ran = slot
            .replay(
                &self.options,
                &settings,
                &prepared.script,
                &steps,
                prepared.records_policy_roles,
                &mut emit,
            )
            .await;
        match ran {
            Ok(()) => {}
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
        for (k, model) in models.into_iter().enumerate().skip(first) {
            let Some(model) = model else {
                return Err(SandboxError::Unavailable(format!(
                    "no model was read after migration {k}"
                )));
            };
            on_event(ReplayEvent::Replayed {
                index: k,
                model: Box::new(model),
            });
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
                .find(|s| s.reason == SkipReason::Code)
                .map(|s| (j, s))
        })?;
    let place = if j == k {
        format!("line {}", statement.line)
    } else {
        format!("an earlier migration (line {})", statement.line)
    };
    Some(format!(
        "The import does not run {} statements ({place} was skipped): if it created this, move that DDL out of it.",
        statement.kind
    ))
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
    let mut rewritten: HashMap<usize, NodeEnum> = HashMap::new();
    let mut installed: HashSet<String> = HashSet::new();
    for statement in &parsed.statements {
        let k = joined.migration_at(statement.span.start);
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
                } else {
                    installed.insert(s.extname.clone());
                }
            }
            NodeEnum::DropStmt(s)
                if s.remove_type() == pg_query::protobuf::ObjectType::ObjectExtension =>
            {
                for object in &s.objects {
                    if let Some(name) = crate::names::string(object) {
                        installed.remove(name);
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
}
