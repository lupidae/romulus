//! Migration runs (docs/playground.md, "Migration runs"): reset to the
//! generated rows, then each statement as the schema's owner, in its own
//! `BEGIN … COMMIT` unless it cannot run in a transaction
//! (`CONCURRENTLY`, `VACUUM`), measured:
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
//! The run has a budget ([`Config::migration_deadline`]): each statement
//! gets the develop deadline or what is left of it. The service's own
//! queries in the owner's session resolve in `pg_catalog` only
//! (`SET LOCAL search_path`), whatever the migration defined or set.
//!
//! Its backfills run for real here, which a compile never does. Afterwards
//! the playground holds the migrated rows, until Reset.
//!
//! [`Config::migration_deadline`]: super::host::Config::migration_deadline

use std::{collections::HashMap, sync::Arc, time::Duration};

use romulus_play_runner::password::Role;
use romulus_sandbox::pg_query::NodeEnum;
use tokio::{
    sync::{OwnedMutexGuard, mpsc},
    time::Instant,
};

use super::{
    host::{Entry, Host, Notice, connect},
    pg::{self, Conn, DbError, Event, PgError, quote_ident, quote_literal},
};
use crate::protocol::{
    Lock, Migrate, MigrationEvent, MigrationReport, Offending, SqlError, StatementResult,
    StatementStatus, TextSpan,
};

/// After a cancel request, how long before the backend is terminated.
const GRACE: Duration = Duration::from_secs(5);

/// Runs the migration under the operation lock the caller took (`op`),
/// telling `events`; ends with `Done` or `Failed`.
pub async fn run(
    host: Arc<Host>,
    entry: Arc<Entry>,
    migrate: Migrate,
    by: String,
    events: mpsc::Sender<MigrationEvent>,
    op: OwnedMutexGuard<()>,
) {
    let busy = Host::busy(&entry, "migrating");
    let _ = events.send(MigrationEvent::Resetting).await;
    let target = match host
        .reset_locked(&entry, &format!("{by} is running the migration"))
        .await
    {
        Ok(target) => target,
        Err(err) => {
            // Not busy any more before the consoles hear it is back.
            drop(busy);
            drop(op);
            let _ = entry.notices.send(Notice::Resume);
            let _ = events
                .send(MigrationEvent::Failed {
                    message: format!("The playground could not be reset: {err}"),
                })
                .await;
            return;
        }
    };
    let outcome = async {
        let mut conn = connect(&target, entry.id, Role::Owner, &host.config.secret).await?;
        let report = statements(&host, &entry, &mut conn, &migrate, &events).await;
        conn.close().await;
        Ok::<_, PgError>(report)
    }
    .await;
    drop(busy);
    drop(op);
    let _ = entry.notices.send(Notice::Resume);
    let _ = events
        .send(match outcome {
            Ok(report) => MigrationEvent::Done { report },
            Err(err) => MigrationEvent::Failed {
                message: format!("The playground could not be reached: {err}"),
            },
        })
        .await;
}

/// How a statement must run.
fn shape(sql: &str) -> (bool, bool) {
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
        _ => false,
    };
    (transaction_control, outside)
}

async fn statements(
    host: &Arc<Host>,
    entry: &Arc<Entry>,
    conn: &mut Conn,
    migrate: &Migrate,
    events: &mpsc::Sender<MigrationEvent>,
) -> MigrationReport {
    let started = Instant::now();
    let budget = started + host.config.migration_deadline;
    let total = migrate.statements.len();
    let mut results = Vec::with_capacity(total);
    let mut failed_at = None;
    for (index, statement) in migrate.statements.iter().enumerate() {
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
            let _ = events
                .send(MigrationEvent::Statement {
                    result: Box::new(result.clone()),
                })
                .await;
            results.push(result);
            continue;
        }
        let _ = events.send(MigrationEvent::Running { index, total }).await;
        let left = budget.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let result = StatementResult {
                status: StatementStatus::Failed,
                error: Some(SqlError {
                    sqlstate: "57014".to_owned(),
                    message: format!(
                        "The run reached its {} min limit before this statement.",
                        host.config.migration_deadline.as_secs().div_ceil(60)
                    ),
                    ..SqlError::default()
                }),
                ..base
            };
            failed_at = Some(index);
            let _ = events
                .send(MigrationEvent::Statement {
                    result: Box::new(result.clone()),
                })
                .await;
            results.push(result);
            continue;
        }
        let permit = Arc::clone(&host.statements).acquire_owned().await.ok();
        let counted = entry.statement();
        let result = measure(
            host,
            entry,
            conn,
            base,
            outside || statement.outside_transaction,
            left.min(host.config.develop_deadline),
            budget,
        )
        .await;
        drop(counted);
        drop(permit);
        if result.status == StatementStatus::Failed {
            failed_at = Some(index);
        }
        let _ = events
            .send(MigrationEvent::Statement {
                result: Box::new(result.clone()),
            })
            .await;
        results.push(result);
    }
    MigrationReport {
        passed: failed_at.is_none(),
        statements: results,
        failed_at,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

/// Ahead of the service's own queries in the owner's session (inside the
/// statement's transaction): names resolve in `pg_catalog` only, until its
/// `COMMIT`, whatever the migration created or set.
const CATALOG_ONLY: &str = "SET LOCAL search_path = pg_catalog; ";

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

fn file_map(rows: Vec<Vec<Option<String>>>) -> HashMap<String, (String, String)> {
    rows.into_iter()
        .filter_map(|r| {
            let mut r = r.into_iter();
            Some((r.next()??, (r.next()??, r.next()??)))
        })
        .collect()
}

/// The tables' files before a statement: committed, so read on the control
/// session (`play_service`, whose `search_path` is `pg_catalog`), never in
/// the owner's session the migration may have steered.
async fn files_before(host: &Host, entry: &Entry) -> HashMap<String, (String, String)> {
    let Ok(mut control) = entry.control(&host.config.secret).await else {
        return HashMap::new();
    };
    let Some(conn) = control.as_mut() else {
        return HashMap::new();
    };
    if let Ok(rows) = conn.query(FILES).await {
        file_map(rows)
    } else {
        *control = None;
        HashMap::new()
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "a statement, where it runs, and its two time limits"
)]
async fn measure(
    host: &Arc<Host>,
    entry: &Arc<Entry>,
    conn: &mut Conn,
    mut result: StatementResult,
    outside: bool,
    limit: Duration,
    budget: Instant,
) -> StatementResult {
    let started = Instant::now();
    let sql = result.sql.clone();
    let mut before = HashMap::new();
    if !outside {
        before = files_before(host, entry).await;
        if let Err(err) = conn.query("BEGIN").await {
            return failed(result, &err, &sql, started);
        }
    }
    // Its rows (a `SELECT` in a migration) are read past, never kept.
    let ran = execute(host, entry, conn, &sql, limit, None)
        .await
        .map(|(tag, _)| tag);
    result.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match ran {
        Ok(tag) => {
            result.tag = Some(tag);
            if outside {
                result.locks_documented = true;
                result.locks = documented(&sql);
            } else {
                let locks = conn
                    .query(&format!("{CATALOG_ONLY}{LOCKS}"))
                    .await
                    .unwrap_or_default();
                let mut strongest: Vec<Lock> = Vec::new();
                for row in locks {
                    let (Some(relation), Some(mode)) = (
                        row.first().cloned().flatten(),
                        row.get(1).cloned().flatten(),
                    ) else {
                        continue;
                    };
                    // Indexes follow their table's lock: the tables tell it.
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
                result.locks = strongest;
                let after = conn
                    .query(&format!("{CATALOG_ONLY}{FILES}"))
                    .await
                    .map(file_map)
                    .unwrap_or_default();
                result.rewrote = before
                    .iter()
                    .filter(|(oid, (node, _))| after.get(*oid).is_some_and(|(n, _)| n != node))
                    .map(|(_, (_, name))| name.clone())
                    .collect();
                result.rewrote.sort();
                result.rows_changed = conn
                    .query(&format!("{CATALOG_ONLY}{CHANGED}"))
                    .await
                    .ok()
                    .and_then(|r| r.first().and_then(|r| r.first().cloned().flatten()))
                    .and_then(|n| n.parse().ok());
                // A COMMIT of a failed transaction rolls back without an
                // error: never report a statement passed that way.
                if conn.txn() == crate::protocol::Txn::Failed {
                    let _ = conn.query("ROLLBACK").await;
                    let err = PgError::Protocol("the run could not measure the statement".into());
                    return failed(result, &err, &sql, started);
                }
                if let Err(err) = conn.query("COMMIT").await {
                    return failed(result, &err, &sql, started);
                }
            }
            result.status = StatementStatus::Passed;
            result
        }
        Err(err) => {
            if !outside {
                let _ = conn.query("ROLLBACK").await;
            }
            // Within what is left of the run's budget, as statements are.
            let offending = match err.db() {
                Some(db) => {
                    let mut reads = Reads {
                        host,
                        entry,
                        conn,
                        budget,
                    };
                    offending(&mut reads, db).await
                }
                None => None,
            };
            let mut result = failed(result, &err, &sql, started);
            result.offending = offending;
            result
        }
    }
}

fn failed(
    mut result: StatementResult,
    err: &PgError,
    sql: &str,
    started: Instant,
) -> StatementResult {
    result.status = StatementStatus::Failed;
    result.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    result.error = Some(match err.db() {
        Some(db) => SqlError {
            sqlstate: db.code.clone(),
            message: db.message.clone(),
            detail: db.detail.clone(),
            hint: db.hint.clone(),
            span: db.byte_position(sql).map(|at| TextSpan::of(sql, at..at)),
        },
        None => SqlError {
            sqlstate: "08000".to_owned(),
            message: err.to_string(),
            ..SqlError::default()
        },
    });
    result
}

/// How many rows (and bytes of them) one of the service's reads keeps: its
/// counts and samples need a handful.
#[derive(Clone, Copy)]
struct Keep {
    rows: usize,
    bytes: usize,
}

const READ: Keep = Keep {
    rows: 100,
    bytes: 64 * 1024,
};

/// Runs `sql` to its end within `limit` (the develop deadline, or what is
/// left of the run's budget): its tag, and with `keep` the first rows of its
/// last result (within the cap; the rest read past). Without `keep` no row
/// is decoded or kept: a migration statement returning millions of rows
/// streams through in constant memory.
async fn execute(
    host: &Arc<Host>,
    entry: &Arc<Entry>,
    conn: &mut Conn,
    sql: &str,
    limit: Duration,
    keep: Option<Keep>,
) -> Result<(String, Vec<Vec<Option<String>>>), PgError> {
    conn.send_query(sql).await?;
    let key = conn.key();
    let target = conn.target().clone();
    let deadline = Instant::now() + limit;
    let mut cancelled_at: Option<Instant> = None;
    let mut tag = String::new();
    let mut rows = Vec::new();
    let mut bytes = 0usize;
    let mut error: Option<DbError> = None;
    loop {
        let next = async {
            if keep.is_some() {
                conn.next().await
            } else {
                conn.next_skipping_rows().await
            }
        };
        let event = tokio::select! {
            biased;
            event = next => event?,
            () = tokio::time::sleep_until(deadline), if cancelled_at.is_none() => {
                cancelled_at = Some(Instant::now());
                let _ = pg::cancel(&target, key).await;
                continue;
            }
            () = async {
                match cancelled_at {
                    Some(at) => tokio::time::sleep_until(at + GRACE).await,
                    None => std::future::pending().await,
                }
            } => {
                host.terminate(entry, key.pid).await;
                return Err(PgError::Protocol("the statement ran past its time limit".into()));
            }
        };
        match event {
            Event::Done(done) => tag = done,
            Event::Columns(_) => {
                rows.clear();
                bytes = 0;
            }
            Event::Row(row) => {
                let size: usize = row.iter().flatten().map(String::len).sum();
                if let Some(keep) = keep
                    && rows.len() < keep.rows
                    && bytes + size <= keep.bytes
                {
                    bytes += size;
                    rows.push(row);
                }
            }
            Event::Error(e) => error = error.or(Some(e)),
            Event::CopyIn => conn.copy_fail("COPY FROM STDIN has no data here").await?,
            Event::Ready(_) => break,
            _ => {}
        }
    }
    match error {
        Some(mut e) => {
            if cancelled_at.is_some() && e.code == "57014" {
                e.message = format!(
                    "The statement ran past {} s and was cancelled.",
                    limit.as_secs()
                );
            }
            Err(PgError::Db(Box::new(e)))
        }
        None => Ok((tag, rows)),
    }
}

/// The lock a statement outside a transaction takes, from the
/// documentation (`CREATE INDEX CONCURRENTLY` and its kin).
fn documented(sql: &str) -> Vec<Lock> {
    let upper = sql.to_ascii_uppercase();
    let mode = if upper.contains("CONCURRENTLY") {
        "ShareUpdateExclusiveLock"
    } else {
        return Vec::new();
    };
    vec![Lock {
        relation: "the table".to_owned(),
        mode: mode.to_owned(),
    }]
}

/// The rows that break a statement: the NULLs of the column a NOT NULL
/// names, or the duplicates of the unique index a unique violation names,
/// its columns read from the catalog (an index on expressions gets none: the
/// error's text says enough). Never SQL built from the error's text; names
/// quoted, the service's queries resolving in `pg_catalog`.
async fn offending(reads: &mut Reads<'_>, error: &DbError) -> Option<Offending> {
    let (schema, table) = (error.schema.as_ref()?, error.table.as_ref()?);
    let relation = format!("{}.{}", quote_ident(schema), quote_ident(table));
    let key = match error.code.as_str() {
        // not_null_violation: the column has NULLs.
        "23502" => None,
        // unique_violation: the index's key columns.
        "23505" => {
            let index = error.constraint.as_ref()?;
            let columns = reads
                .run(&format!(
                    "SELECT a.attname FROM pg_catalog.pg_index i \
                     JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid \
                     JOIN pg_catalog.pg_namespace n ON n.oid = ic.relnamespace \
                     CROSS JOIN LATERAL pg_catalog.unnest(i.indkey) WITH ORDINALITY AS k(attnum, ord) \
                     JOIN pg_catalog.pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum \
                     WHERE n.nspname = {} AND ic.relname = {} AND i.indexprs IS NULL \
                       AND k.ord <= i.indnkeyatts \
                     ORDER BY k.ord",
                    quote_literal(schema),
                    quote_literal(index)
                ))
                .await?;
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
    let count = reads
        .run(&count)
        .await?
        .first()?
        .first()
        .cloned()
        .flatten()?
        .parse()
        .ok()?;
    let sample = reads
        .run(&sample)
        .await?
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .collect();
    Some(Offending {
        count,
        query: format!("{query} LIMIT 100;"),
        sample,
    })
}

/// The service's reads in the owner's session after a failed statement:
/// within what is left of the run's budget (each at most the develop
/// deadline), names resolving in `pg_catalog`, rolled back.
struct Reads<'a> {
    host: &'a Arc<Host>,
    entry: &'a Arc<Entry>,
    conn: &'a mut Conn,
    budget: Instant,
}

impl Reads<'_> {
    async fn run(&mut self, sql: &str) -> Option<Vec<Vec<Option<String>>>> {
        let left = self
            .budget
            .saturating_duration_since(Instant::now())
            .min(self.host.config.develop_deadline);
        if left.is_zero() {
            return None;
        }
        let read = execute(
            self.host,
            self.entry,
            self.conn,
            &format!("BEGIN; {CATALOG_ONLY}{sql}"),
            left,
            Some(READ),
        )
        .await;
        let _ = self.conn.query("ROLLBACK").await;
        read.ok().map(|(_, rows)| rows)
    }
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
    fn the_strongest_lock_wins() {
        assert!(strength("AccessExclusiveLock") > strength("ShareLock"));
        assert!(strength("RowExclusiveLock") > strength("AccessShareLock"));
    }
}
