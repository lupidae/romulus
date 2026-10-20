//! Running a migration's statements, measured (docs/playground.md,
//! "Migration runs"): each in its own `BEGIN … COMMIT` unless it cannot run
//! in a transaction (`CONCURRENTLY`, `VACUUM`), stopping at the first that
//! fails. For each:
//!
//! - duration;
//! - locks, from `pg_locks` for its own backend before `COMMIT`: exact (a
//!   statement outside a transaction gets its documented lock, marked so);
//! - rewrites: a table's `relfilenode` changed, exact;
//! - rows changed, from `pg_stat_xact_user_tables`;
//! - on failure: SQLSTATE, message, detail, the position in the statement,
//!   and the rows that break it when the error says which (a NOT NULL
//!   column, a unique index on plain columns, looked up in the catalog),
//!   counted and sampled after the rollback.
//!
//! Transaction control in a written migration (`BEGIN`, `COMMIT`) is
//! skipped and said so. The engine's own queries resolve in `pg_catalog`
//! only (`SET LOCAL search_path`), whatever the migration defined or set.

use std::{collections::HashMap, time::Duration};

use romulus_sandbox::pg_query::NodeEnum;
use tokio::time::Instant;

use crate::{
    report::{
        Lock, MigrationReport, Offending, SqlError, StatementResult, StatementStatus, TextSpan,
    },
    session::{Rows, Session, SessionError, quote_ident, quote_literal},
};

/// One statement to run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Statement {
    /// One statement, with or without its `;`.
    pub sql: String,
    /// `deletes_data`, `can_fail`, `widens_access`, as romulus-migration
    /// flagged it.
    pub hazard: Option<String>,
    /// romulus-migration planned it (its hazard is a flag); otherwise it is
    /// written by hand and has none.
    #[serde(default)]
    pub planned: bool,
    /// Runs outside a transaction (`CREATE INDEX CONCURRENTLY`), whatever
    /// its text says.
    #[serde(default)]
    pub outside_transaction: bool,
}

impl Statement {
    /// A statement romulus-migration planned.
    pub fn planned(statement: &romulus_migration::Statement) -> Self {
        Self {
            sql: statement.sql.clone(),
            hazard: statement.hazard.and_then(|h| {
                serde_json::to_value(h)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
            }),
            planned: true,
            outside_transaction: statement.stage == romulus_migration::Stage::Concurrent,
        }
    }

    /// A statement written by hand.
    pub fn written(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            hazard: None,
            planned: false,
            outside_transaction: false,
        }
    }

    /// The statements of a migration written by hand, or why it does not
    /// split (a syntax error, located).
    pub fn split(text: &str) -> Result<Vec<Self>, String> {
        let parsed = romulus_sandbox::parse(text);
        if let Some(error) = parsed.errors.first() {
            return Err(match error.span {
                Some(span) => format!("line {}: {}", span.from.line, error.message),
                None => error.message.clone(),
            });
        }
        Ok(parsed
            .statements
            .iter()
            .map(|s| Self::written(format!("{};", s.text(text))))
            .collect())
    }

    /// The statements romulus-migration planned.
    pub fn all_planned(migration: &romulus_migration::Migration) -> Vec<Self> {
        migration.statements.iter().map(Self::planned).collect()
    }
}

/// How long a run and each statement may take.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Each statement, at most.
    pub statement: Duration,
    /// When the whole run must be over.
    pub until: Instant,
    /// The whole run's budget, as given (for the message saying it ran out).
    pub budget: Duration,
}

impl Limits {
    /// `budget` from now, `statement` each.
    pub fn new(statement: Duration, budget: Duration) -> Self {
        Self {
            statement,
            until: Instant::now() + budget,
            budget,
        }
    }

    fn left(&self) -> Duration {
        self.until.saturating_duration_since(Instant::now())
    }

    /// What one of the engine's own queries may take: a statement's limit
    /// or what is left, at least a second (to say what it found).
    pub(crate) fn read(&self) -> Duration {
        self.left().min(self.statement).max(Duration::from_secs(1))
    }
}

/// What a run tells as it goes.
#[derive(Debug, Clone)]
pub enum Progress {
    /// Statement `index` (of `total`) starts.
    Running {
        index: usize,
        total: usize,
    },
    Done(Box<StatementResult>),
}

/// How a statement must run: transaction control (skipped), or outside a
/// transaction.
pub(crate) fn shape(sql: &str) -> (bool, bool) {
    let parsed = romulus_sandbox::parse(sql);
    let Some(statement) = parsed.statements.first() else {
        return (false, false);
    };
    let transaction_control = matches!(statement.node(), NodeEnum::TransactionStmt(_));
    let outside = match statement.node() {
        NodeEnum::IndexStmt(s) => s.concurrent,
        NodeEnum::DropStmt(s) => s.concurrent,
        NodeEnum::ReindexStmt(s) => s
            .params
            .iter()
            .any(|p| matches!(p.node.as_ref(), Some(NodeEnum::DefElem(d)) if d.defname == "concurrently")),
        NodeEnum::VacuumStmt(_) => true,
        // `ALTER TYPE … ADD VALUE` runs in a transaction from 12 on.
        _ => false,
    };
    (transaction_control, outside)
}

/// Runs `statements` in order on `session`, measured; stops at the first
/// that fails (the rest are `not_run`).
pub async fn run<S: Session>(
    session: &mut S,
    statements: &[Statement],
    limits: Limits,
    on: &mut (dyn FnMut(Progress) + Send),
) -> MigrationReport {
    let started = Instant::now();
    let total = statements.len();
    let mut results = Vec::with_capacity(total);
    let mut failed_at = None;
    // The tables' files as the last statement left them; `None`: to read.
    let mut files: Option<Files> = None;
    for (index, statement) in statements.iter().enumerate() {
        let base = StatementResult {
            index,
            sql: statement.sql.clone(),
            status: StatementStatus::NotRun,
            hazard: statement.hazard.clone(),
            duration_ms: 0,
            locks: Vec::new(),
            locks_documented: false,
            rewrote: Vec::new(),
            rows_changed: None,
            tag: None,
            error: None,
            offending: None,
        };
        if failed_at.is_some() {
            results.push(base);
            continue;
        }
        let (control, outside) = shape(&statement.sql);
        if control {
            let result = StatementResult {
                status: StatementStatus::Skipped,
                ..base
            };
            on(Progress::Done(Box::new(result.clone())));
            results.push(result);
            continue;
        }
        on(Progress::Running { index, total });
        let left = limits.left();
        if left.is_zero() {
            let result = StatementResult {
                status: StatementStatus::Failed,
                error: Some(SqlError {
                    sqlstate: "57014".to_owned(),
                    message: format!(
                        "The run reached its {} limit before this statement.",
                        minutes(limits)
                    ),
                    ..SqlError::default()
                }),
                ..base
            };
            failed_at = Some(index);
            on(Progress::Done(Box::new(result.clone())));
            results.push(result);
            continue;
        }
        let result = measure(
            session,
            base,
            outside || statement.outside_transaction,
            left.min(limits.statement),
            limits,
            &mut files,
        )
        .await;
        if result.status == StatementStatus::Failed {
            failed_at = Some(index);
        }
        on(Progress::Done(Box::new(result.clone())));
        results.push(result);
    }
    MigrationReport {
        passed: failed_at.is_none(),
        statements: results,
        failed_at,
        duration_ms: millis(started),
    }
}

fn minutes(limits: Limits) -> String {
    let minutes = limits.budget.as_secs().div_ceil(60);
    if minutes < 2 {
        format!("{} s", limits.budget.as_secs())
    } else {
        format!("{minutes} min")
    }
}

pub(crate) fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Ahead of the engine's own queries in a transaction: names resolve in
/// `pg_catalog` only, until its end, whatever the migration created or set.
pub(crate) const CATALOG_ONLY: &str = "SET LOCAL search_path = pg_catalog; ";

const FILES: &str = "SELECT c.oid::text, c.relfilenode::text, n.nspname || '.' || c.relname \
    FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
    WHERE c.relkind IN ('r', 'm') AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
      AND n.nspname NOT LIKE 'pg_toast%'";

const LOCKS: &str = "SELECT n.nspname || '.' || c.relname, l.mode, c.relkind::text \
    FROM pg_catalog.pg_locks l \
    JOIN pg_catalog.pg_class c ON c.oid = l.relation \
    JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
    WHERE l.pid = pg_catalog.pg_backend_pid() AND l.locktype = 'relation' AND l.granted \
      AND n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg_toast%'";

const CHANGED: &str = "SELECT coalesce(sum(n_tup_ins + n_tup_upd + n_tup_del), 0)::text \
    FROM pg_catalog.pg_stat_xact_user_tables";

/// Postgres's lock modes, weakest first.
const MODES: &[&str] = &[
    "AccessShareLock",
    "RowShareLock",
    "RowExclusiveLock",
    "ShareUpdateExclusiveLock",
    "ShareLock",
    "ShareRowExclusiveLock",
    "ExclusiveLock",
    "AccessExclusiveLock",
];

fn strength(mode: &str) -> usize {
    MODES.iter().position(|m| *m == mode).unwrap_or(0)
}

/// Each table's file, by oid: (relfilenode, `schema.name`).
type Files = HashMap<String, (String, String)>;

fn file_map(rows: Vec<Vec<Option<String>>>) -> Files {
    rows.into_iter()
        .filter_map(|r| {
            let mut r = r.into_iter();
            Some((r.next()??, (r.next()??, r.next()??)))
        })
        .collect()
}

/// One of the engine's reads: in a transaction rolled back, names resolving
/// in `pg_catalog`, within `limit`. A failed read leaves no transaction
/// open.
pub(crate) async fn read<S: Session>(
    session: &mut S,
    sql: &str,
    limit: Duration,
) -> Result<Vec<Vec<Option<String>>>, SessionError> {
    let answer = session
        .run(
            &format!("BEGIN; {CATALOG_ONLY}{sql}; ROLLBACK"),
            limit,
            Rows::READ,
        )
        .await;
    match answer {
        Ok(answer) => Ok(answer.rows),
        Err(err) => {
            let _ = session.run("ROLLBACK", limit, Rows::Skip).await;
            Err(err)
        }
    }
}

/// The one text value of a read.
pub(crate) fn scalar(rows: &[Vec<Option<String>>]) -> Option<&str> {
    rows.first()?.first()?.as_deref()
}

async fn measure<S: Session>(
    session: &mut S,
    mut result: StatementResult,
    outside: bool,
    limit: Duration,
    limits: Limits,
    files: &mut Option<Files>,
) -> StatementResult {
    let sql = result.sql.clone();
    if !outside && files.is_none() {
        *files = read(session, FILES, limits.read()).await.ok().map(file_map);
    }
    let started = Instant::now();
    if !outside && let Err(err) = session.run("BEGIN", limit, Rows::Skip).await {
        return failed(result, &err, &sql, started, limit);
    }
    let ran = session.run(&sql, limit, Rows::Skip).await;
    result.duration_ms = millis(started);
    match ran {
        Ok(answer) => {
            result.tag = answer.tag;
            if outside {
                result.locks_documented = true;
                result.locks = documented(&sql);
                // A VACUUM FULL rewrites: read them again before the next.
                *files = None;
            } else {
                let measured = async {
                    let locks = session
                        .run(&format!("{CATALOG_ONLY}{LOCKS}"), limits.read(), Rows::READ)
                        .await?
                        .rows;
                    let after = session
                        .run(&format!("{CATALOG_ONLY}{FILES}"), limits.read(), Rows::READ)
                        .await?
                        .rows;
                    let changed = session
                        .run(
                            &format!("{CATALOG_ONLY}{CHANGED}"),
                            limits.read(),
                            Rows::READ,
                        )
                        .await?
                        .rows;
                    Ok::<_, SessionError>((locks, file_map(after), changed))
                }
                .await;
                // A statement that left its transaction failed (or a read
                // that did) is never reported passed: a COMMIT then rolls
                // back without an error.
                let (locks, after, changed) = match measured {
                    Ok(measured) => measured,
                    Err(err) => {
                        let _ = session.run("ROLLBACK", limit, Rows::Skip).await;
                        *files = None;
                        let err = SessionError::Lost(format!(
                            "the run could not measure the statement: {err}"
                        ));
                        return failed(result, &err, &sql, started, limit);
                    }
                };
                result.locks = strongest(locks);
                if let Some(before) = files.as_ref() {
                    result.rewrote = before
                        .iter()
                        .filter(|(oid, (node, _))| after.get(*oid).is_some_and(|(n, _)| n != node))
                        .map(|(_, (_, name))| name.clone())
                        .collect();
                    result.rewrote.sort();
                }
                result.rows_changed = scalar(&changed).and_then(|n| n.parse().ok());
                match session.run("COMMIT", limit, Rows::Skip).await {
                    Ok(answer) if answer.tag.as_deref() == Some("ROLLBACK") => {
                        *files = None;
                        let err = SessionError::Lost(
                            "the statement's transaction failed: it was rolled back".to_owned(),
                        );
                        return failed(result, &err, &sql, started, limit);
                    }
                    Ok(_) => *files = Some(after),
                    Err(err) => {
                        *files = None;
                        return failed(result, &err, &sql, started, limit);
                    }
                }
            }
            result.status = StatementStatus::Passed;
            result
        }
        Err(err) => {
            if !outside {
                let _ = session.run("ROLLBACK", limit, Rows::Skip).await;
            }
            let offending = match err.db() {
                Some(db) => offending(session, db, limits).await,
                None => None,
            };
            let mut result = failed(result, &err, &sql, started, limit);
            result.offending = offending;
            result
        }
    }
}

/// The strongest lock per relation; indexes follow their table's lock.
fn strongest(rows: Vec<Vec<Option<String>>>) -> Vec<Lock> {
    let mut strongest: Vec<Lock> = Vec::new();
    for row in rows {
        let (Some(relation), Some(mode)) = (
            row.first().cloned().flatten(),
            row.get(1).cloned().flatten(),
        ) else {
            continue;
        };
        if row.get(2).cloned().flatten().as_deref() == Some("i") {
            continue;
        }
        match strongest.iter_mut().find(|l| l.relation == relation) {
            Some(lock) if strength(&mode) > strength(&lock.mode) => lock.mode = mode,
            Some(_) => {}
            None => strongest.push(Lock { relation, mode }),
        }
    }
    strongest.sort_by(|a, b| a.relation.cmp(&b.relation));
    strongest
}

/// A [`SqlError`] for a statement's failure.
pub(crate) fn sql_error(err: &SessionError, sql: &str, limit: Duration) -> SqlError {
    match err {
        SessionError::Db(db) => SqlError {
            sqlstate: db.code.clone(),
            message: db.message.clone(),
            detail: db.detail.clone(),
            hint: db.hint.clone(),
            span: db.byte_position(sql).map(|at| TextSpan::of(sql, at..at)),
        },
        SessionError::Timeout => SqlError {
            sqlstate: "57014".to_owned(),
            message: format!(
                "The statement ran past {} s and was cancelled.",
                limit.as_secs()
            ),
            ..SqlError::default()
        },
        SessionError::Lost(message) => SqlError {
            sqlstate: "08000".to_owned(),
            message: message.clone(),
            ..SqlError::default()
        },
    }
}

fn failed(
    mut result: StatementResult,
    err: &SessionError,
    sql: &str,
    started: Instant,
    limit: Duration,
) -> StatementResult {
    result.status = StatementStatus::Failed;
    result.duration_ms = millis(started);
    result.error = Some(sql_error(err, sql, limit));
    result
}

/// The lock a statement outside a transaction takes, from the
/// documentation (`CREATE INDEX CONCURRENTLY` and its kin).
fn documented(sql: &str) -> Vec<Lock> {
    let upper = sql.to_ascii_uppercase();
    if !upper.contains("CONCURRENTLY") {
        return Vec::new();
    }
    vec![Lock {
        relation: "the table".to_owned(),
        mode: "ShareUpdateExclusiveLock".to_owned(),
    }]
}

/// The rows that break a statement: the NULLs of the column a NOT NULL
/// names, or the duplicates of the unique index a unique violation names,
/// its columns read from the catalog (an index on expressions gets none:
/// the error's text says enough). Never SQL built from the error's text;
/// names quoted, the queries resolving in `pg_catalog`.
async fn offending<S: Session>(
    session: &mut S,
    error: &crate::session::DbError,
    limits: Limits,
) -> Option<Offending> {
    let (schema, table) = (error.schema.as_ref()?, error.table.as_ref()?);
    let relation = format!("{}.{}", quote_ident(schema), quote_ident(table));
    let key = match error.code.as_str() {
        // not_null_violation: the column has NULLs.
        "23502" => None,
        // unique_violation: the index's key columns.
        "23505" => {
            let index = error.constraint.as_ref()?;
            let columns = read(
                session,
                &format!(
                    "SELECT a.attname::text FROM pg_catalog.pg_index i \
                     JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid \
                     JOIN pg_catalog.pg_namespace n ON n.oid = ic.relnamespace \
                     CROSS JOIN LATERAL pg_catalog.unnest(i.indkey) WITH ORDINALITY AS k(attnum, ord) \
                     JOIN pg_catalog.pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum \
                     WHERE n.nspname = {} AND ic.relname = {} AND i.indexprs IS NULL \
                       AND k.ord <= i.indnkeyatts \
                     ORDER BY k.ord",
                    quote_literal(schema),
                    quote_literal(index)
                ),
                limits.read(),
            )
            .await
            .ok()?;
            let names: Vec<String> = columns
                .into_iter()
                .filter_map(|r| r.into_iter().next().flatten())
                .map(|c| quote_ident(&c))
                .collect();
            if names.is_empty() {
                return None;
            }
            Some(names.join(", "))
        }
        _ => return None,
    };
    let (count, query, sample) = if let Some(columns) = key {
        let query = format!(
            "SELECT {columns}, count(*) FROM {relation} GROUP BY {columns} HAVING count(*) > 1 ORDER BY count(*) DESC"
        );
        (
            format!("SELECT pg_catalog.count(*)::text FROM ({query}) d"),
            query,
            format!(
                "SELECT ({columns})::text FROM {relation} GROUP BY {columns} HAVING pg_catalog.count(*) > 1 LIMIT 5"
            ),
        )
    } else {
        let column = quote_ident(error.column.as_ref()?);
        let filter = format!("{column} IS NULL");
        (
            format!("SELECT pg_catalog.count(*)::text FROM {relation} WHERE {filter}"),
            format!("SELECT * FROM {relation} WHERE {filter}"),
            format!("SELECT t::text FROM {relation} t WHERE {filter} LIMIT 5"),
        )
    };
    let count = read(session, &count, limits.read()).await.ok()?;
    let count = scalar(&count)?.parse().ok()?;
    let sample = read(session, &sample, limits.read())
        .await
        .ok()?
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .collect();
    Some(Offending {
        count,
        query: format!("{query} LIMIT 100;"),
        sample,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrently_and_transaction_control_are_recognised() {
        assert_eq!(
            shape("CREATE INDEX CONCURRENTLY i ON t (a);"),
            (false, true)
        );
        assert_eq!(shape("CREATE INDEX i ON t (a);"), (false, false));
        assert_eq!(shape("BEGIN;"), (true, false));
        assert_eq!(shape("COMMIT"), (true, false));
        assert_eq!(shape("DROP INDEX CONCURRENTLY i"), (false, true));
        assert_eq!(shape("ALTER TABLE t ADD COLUMN b int;"), (false, false));
    }

    #[test]
    fn the_strongest_lock_wins_and_indexes_follow_their_table() {
        let row = |r: &str, m: &str, k: &str| vec![Some(r.into()), Some(m.into()), Some(k.into())];
        let locks = strongest(vec![
            row("public.t", "AccessShareLock", "r"),
            row("public.t", "AccessExclusiveLock", "r"),
            row("public.t_pkey", "AccessExclusiveLock", "i"),
            row("public.a", "RowExclusiveLock", "r"),
        ]);
        assert_eq!(
            locks,
            [
                Lock {
                    relation: "public.a".into(),
                    mode: "RowExclusiveLock".into()
                },
                Lock {
                    relation: "public.t".into(),
                    mode: "AccessExclusiveLock".into()
                },
            ]
        );
    }

    #[test]
    fn written_migrations_split_into_statements_or_say_where_they_do_not() {
        let split =
            Statement::split("ALTER TABLE t ADD c int;\n-- note\nUPDATE t SET c = 1").unwrap();
        assert_eq!(
            split.iter().map(|s| s.sql.as_str()).collect::<Vec<_>>(),
            ["ALTER TABLE t ADD c int;", "UPDATE t SET c = 1;"]
        );
        assert!(!split[0].planned);
        assert!(
            Statement::split("ALTER TABLE t ADD;")
                .unwrap_err()
                .starts_with("line 1")
        );
    }
}
