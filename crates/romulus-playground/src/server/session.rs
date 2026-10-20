//! A playground's session as romulus-migration-check takes it
//! ([`romulus_migration_check::Session`]): its statements run under the
//! host's caps (a slot of [`Host::statements`], counted as the playground
//! in use), each within its limit: past it the service sends Postgres's
//! cancel request, then terminates the backend after a grace period
//! (`statement_timeout` is the playground's people's to set, so it is never
//! relied on).

use std::{sync::Arc, time::Duration};

use romulus_migration_check::{Answer, DbError, Rows, Session, SessionError};
use tokio::time::Instant;

use super::{
    host::{Entry, Host},
    pg::{self, Conn, Event, PgError},
};

/// After a cancel request, how long before the backend is terminated.
const GRACE: Duration = Duration::from_secs(5);

/// The owner's session on a playground, for the engine.
pub struct PlaySession<'a> {
    pub host: &'a Arc<Host>,
    pub entry: &'a Arc<Entry>,
    pub conn: &'a mut Conn,
}

fn session_error(err: PgError) -> SessionError {
    match err {
        PgError::Db(db) => SessionError::Db(Box::new(DbError {
            code: db.code,
            message: db.message,
            detail: db.detail,
            hint: db.hint,
            position: db.position,
            schema: db.schema,
            table: db.table,
            column: db.column,
            constraint: db.constraint,
        })),
        PgError::Timeout => SessionError::Timeout,
        other => SessionError::Lost(other.to_string()),
    }
}

impl Session for PlaySession<'_> {
    async fn run(
        &mut self,
        sql: &str,
        limit: Duration,
        rows: Rows,
    ) -> Result<Answer, SessionError> {
        let permit = Arc::clone(&self.host.statements).acquire_owned().await.ok();
        let counted = self.entry.statement();
        let keep = match rows {
            Rows::Skip => None,
            Rows::Keep { rows, bytes } => Some(Keep { rows, bytes }),
        };
        let ran = execute(self.host, self.entry, self.conn, sql, limit, keep).await;
        drop(counted);
        drop(permit);
        ran.map(|(tag, rows)| Answer {
            tag: Some(tag),
            rows,
        })
        .map_err(session_error)
    }

    async fn copy_in(
        &mut self,
        statement: &str,
        data: &mut (dyn Iterator<Item = Vec<u8>> + Send),
        limit: Duration,
    ) -> Result<u64, SessionError> {
        let until = Instant::now() + limit;
        let copied = async {
            self.conn.send_query(statement).await?;
            match self.conn.next().await? {
                Event::CopyIn => {}
                Event::Error(error) => {
                    while !matches!(self.conn.next().await?, Event::Ready(_)) {}
                    return Err(PgError::Db(Box::new(error)));
                }
                _ => return Err(PgError::Protocol("COPY did not start".into())),
            }
            for chunk in data {
                self.conn.copy_data(&chunk).await?;
            }
            self.conn.copy_done().await?;
            let mut rows = 0;
            let mut failed = None;
            loop {
                match self.conn.next().await? {
                    Event::Done(tag) => {
                        rows = tag
                            .strip_prefix("COPY ")
                            .and_then(|n| n.parse().ok())
                            .unwrap_or(0);
                    }
                    Event::Error(error) => failed = Some(error),
                    Event::Ready(_) => break,
                    _ => {}
                }
            }
            match failed {
                Some(error) => Err(PgError::Db(Box::new(error))),
                None => Ok(rows),
            }
        };
        tokio::time::timeout_at(until, copied)
            .await
            .map_err(|_| SessionError::Timeout)?
            .map_err(session_error)
    }
}

/// How many rows (and bytes of them) a query keeps.
#[derive(Clone, Copy)]
pub struct Keep {
    pub rows: usize,
    pub bytes: usize,
}

/// Runs `sql` to its end within `limit`: its last tag, and with `keep` the
/// first rows of its last result (within the cap; the rest read past).
/// Without `keep` no row is decoded or kept: a migration statement
/// returning millions of rows streams through in constant memory. Past
/// `limit` it is cancelled, then its backend terminated after [`GRACE`]
/// ([`PgError::Timeout`]).
pub async fn execute(
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
    let mut error: Option<pg::DbError> = None;
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
                return Err(PgError::Timeout);
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
