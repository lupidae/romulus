//! Migration runs (docs/playground.md, "Migration runs"): reset to the
//! generated rows, then each statement as the schema's owner, measured by
//! romulus-migration-check's runner (`statements::run`, the one a Test
//! migration and the migration oracle use): its own `BEGIN … COMMIT`
//! unless it cannot run in a transaction (`CONCURRENTLY`, `VACUUM`);
//! duration, locks from `pg_locks`, rewrites, rows changed, and on failure
//! Postgres's error located with the rows that break it.
//!
//! The run has a budget ([`Config::migration_deadline`]): each statement
//! gets the develop deadline or what is left of it, past which the service
//! cancels it, then terminates its backend ([`super::session`]).
//!
//! Its backfills run for real here, which a compile never does. Afterwards
//! the playground holds the migrated rows, until Reset.
//!
//! [`Config::migration_deadline`]: super::host::Config::migration_deadline

use std::sync::Arc;

use romulus_migration_check::{Limits, Progress, Statement, statements};
use romulus_play_runner::password::Role;
use tokio::sync::{OwnedMutexGuard, mpsc};

use super::{
    host::{Entry, Host, Notice, connect},
    pg::PgError,
    session::PlaySession,
};
use crate::protocol::{Migrate, MigrationEvent, MigrationReport};

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
        let report = statements_of(&host, &entry, &mut conn, &migrate, &events).await;
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

async fn statements_of(
    host: &Arc<Host>,
    entry: &Arc<Entry>,
    conn: &mut super::pg::Conn,
    migrate: &Migrate,
    events: &mpsc::Sender<MigrationEvent>,
) -> MigrationReport {
    let list: Vec<Statement> = migrate
        .statements
        .iter()
        .map(|s| Statement {
            sql: s.sql.clone(),
            hazard: s.hazard.clone(),
            planned: s.hazard.is_some(),
            outside_transaction: s.outside_transaction,
        })
        .collect();
    let limits = Limits::new(host.config.develop_deadline, host.config.migration_deadline);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let run = async {
        let tx = tx;
        let mut session = PlaySession { host, entry, conn };
        let mut on = |progress: Progress| {
            let _ = tx.send(progress);
        };
        statements::run(&mut session, &list, limits, &mut on).await
    };
    let forward = async {
        while let Some(progress) = rx.recv().await {
            let event = match progress {
                Progress::Running { index, total } => MigrationEvent::Running { index, total },
                Progress::Done(result) => MigrationEvent::Statement { result },
            };
            let _ = events.send(event).await;
        }
    };
    let (report, ()) = tokio::join!(run, forward);
    report
}
