//! Test migration: romulus-migration-check on a playground's rows. The
//! playground is a short-lived one the API built for the test (or a copy
//! of someone's playground); its rows are already there
//! (`Seed::Existing`), its policies run for `PUBLIC` (playgrounds have no
//! other roles). The test runs as the schema's owner, each statement within
//! the develop deadline, the whole test within the migration budget, and
//! leaves the playground as it ends: the API deletes it after the report.

use std::sync::Arc;

use remus_core::Schema;
use romulus_migration_check::{Check, Event, Migration, Options, Seed};
use romulus_play_runner::password::Role;
use tokio::sync::{OwnedMutexGuard, mpsc};

use super::{
    host::{Entry, Host, HostError, Notice, connect},
    session::PlaySession,
};
use crate::protocol::{Test, TestEvent};

/// Runs the test under the operation lock the caller took (`op`), telling
/// `events`; ends with `Done` or `Failed`.
pub async fn run(
    host: Arc<Host>,
    entry: Arc<Entry>,
    test: Test,
    events: mpsc::Sender<TestEvent>,
    op: OwnedMutexGuard<()>,
) {
    let busy = Host::busy(&entry, "testing");
    let outcome = check(&host, &entry, test, &events).await;
    drop(busy);
    drop(op);
    let _ = entry.notices.send(Notice::Resume);
    let _ = events
        .send(match outcome {
            Ok(report) => TestEvent::Done {
                report: Box::new(report),
            },
            Err(message) => TestEvent::Failed { message },
        })
        .await;
}

async fn check(
    host: &Arc<Host>,
    entry: &Arc<Entry>,
    test: Test,
    events: &mpsc::Sender<TestEvent>,
) -> Result<romulus_migration_check::Report, String> {
    let base: Schema = serde_json::from_value(test.base)
        .map_err(|e| format!("the base model does not read: {e}"))?;
    let target: Schema = serde_json::from_value(test.target)
        .map_err(|e| format!("the target model does not read: {e}"))?;
    let label = match &test.data {
        romulus_migration_check::DataSource::Existing { label } => format!("a copy of {label}"),
        romulus_migration_check::DataSource::Generated { size, seed, .. } => {
            format!("generated rows ({size}, seed {seed})")
        }
        romulus_migration_check::DataSource::Provided => "the playground's rows".to_owned(),
    };
    let check = Check {
        base,
        target,
        up: Migration {
            statements: test.up,
            reordered: test.up_reordered,
        },
        down: test.down.map(|statements| Migration {
            statements,
            reordered: test.down_reordered,
        }),
        seed: Seed::Existing { label },
        options: Options {
            sample: test.sample.clamp(1, 200),
            statement_deadline: host.config.develop_deadline,
            budget: host.config.migration_deadline,
            policies_public: true,
            base_label: test.base_label,
            target_label: test.target_label,
        },
    };
    let target = host.start_locked(entry).await.map_err(|e| match e {
        HostError::Pg(err) => err.to_string(),
        other => other.to_string(),
    })?;
    let mut conn = connect(&target, entry.id, Role::Owner, &host.config.secret)
        .await
        .map_err(|e| format!("The playground could not be reached: {e}"))?;
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let run = async {
        let tx = tx;
        let mut session = PlaySession {
            host,
            entry,
            conn: &mut conn,
        };
        let mut on = |event: Event| {
            let _ = tx.send(event);
        };
        romulus_migration_check::run(&mut session, &check, &mut on).await
    };
    let forward = async {
        while let Some(event) = rx.recv().await {
            let _ = events.send(TestEvent::Progress { event }).await;
        }
    };
    let (mut report, ()) = tokio::join!(run, forward);
    conn.close().await;
    report.data = test.data;
    Ok(report)
}
