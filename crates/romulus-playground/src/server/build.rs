//! Building a playground (docs/playground.md, "Lifecycle", steps 1–4): its
//! cluster (the runner; init runs on the first start), the schema as its
//! owner, the generated rows, `ANALYZE`, a clean stop, the generated copy,
//! and a start for the people waiting.

use std::{sync::Arc, time::Instant};

use remus_core::Schema;
use romulus_play_runner::{
    password::Role,
    protocol::{Create, Extension as RunnerExtension, Size as RunnerSize},
};
use tokio::sync::{OwnedMutexGuard, mpsc};

use super::{
    host::{Entry, Host, HostError, connect},
    pg::{Conn, PgError, quote_ident},
    seed,
};
use crate::protocol::{Build, BuildEvent, Size, Stage, TableRows};

/// Runs the build under the operation lock the caller took (`op`),
/// telling `events` as it goes; ends with `Ready` or `Failed`. A failed
/// build leaves nothing behind; a deleted one is cancelled (the task
/// aborted) and destroy removes what it left.
pub async fn build(
    host: Arc<Host>,
    entry: Arc<Entry>,
    build: Build,
    events: mpsc::Sender<BuildEvent>,
    op: OwnedMutexGuard<()>,
) {
    let started = Instant::now();
    let _op = op;
    let _busy = Host::busy(&entry, "building");
    let mut stage = Stage::Starting;
    let outcome = run(&host, &entry, build, &events, &mut stage, started).await;
    match outcome {
        Ok(ready) => {
            let _ = events.send(ready).await;
        }
        Err(message) => {
            tracing::warn!(playground = %entry.id, ?stage, error = %message, "build failed");
            let _ = events
                .send(BuildEvent::Failed {
                    stage,
                    message: message.clone(),
                })
                .await;
            // Nothing half-built stays on disk.
            if let Err(err) = host.runner.destroy(entry.id).await {
                tracing::warn!(playground = %entry.id, error = %err, "cannot clean up a failed build");
            }
        }
    }
}

async fn tell(events: &mpsc::Sender<BuildEvent>, stage: &mut Stage, next: Stage) {
    *stage = next;
    let _ = events.send(BuildEvent::Stage { stage: next }).await;
}

fn describe(err: &HostError) -> String {
    match err {
        HostError::Pg(PgError::Db(db)) => db.message.clone(),
        other => other.to_string(),
    }
}

#[allow(clippy::too_many_lines, reason = "the build's steps, in order")]
async fn run(
    host: &Arc<Host>,
    entry: &Arc<Entry>,
    build: Build,
    events: &mpsc::Sender<BuildEvent>,
    stage: &mut Stage,
    started: Instant,
) -> Result<BuildEvent, String> {
    tell(events, stage, Stage::Starting).await;
    let model: Schema =
        serde_json::from_value(build.model).map_err(|e| format!("the model does not read: {e}"))?;
    // Extensions off the allowlist were refused when the version was
    // compiled; a schema that is not a plain identifier gets public.
    let extensions = build
        .extensions
        .iter()
        .map(|e| RunnerExtension {
            name: e.name.clone(),
            schema: if romulus_play_runner::protocol::is_plain_identifier(&e.schema) {
                e.schema.clone()
            } else {
                "public".to_owned()
            },
        })
        .collect();
    let create = Create {
        size: match build.size {
            Size::Small => RunnerSize::Small,
            Size::Medium => RunnerSize::Medium,
        },
        extensions,
    };
    // A leftover of an earlier attempt with this id.
    if host
        .runner
        .state(entry.id)
        .await
        .map_err(|e| e.to_string())?
        .exists
    {
        host.runner
            .destroy(entry.id)
            .await
            .map_err(|e| e.to_string())?;
    }
    host.runner
        .create(entry.id, &create)
        .await
        .map_err(|e| e.to_string())?;
    let target = host.start_locked(entry).await.map_err(|e| describe(&e))?;

    tell(events, stage, Stage::Schema).await;
    let mut owner = connect(&target, entry.id, Role::Owner, &host.config.secret)
        .await
        .map_err(|e| e.to_string())?;
    // One implicit transaction: all of it, or nothing. Each step of the
    // build within the develop deadline: a slow schema fails its build
    // promptly (and its container goes with what still ran).
    let limit = host.config.develop_deadline;
    let slow = |what: &str| format!("{what} took more than {} s", limit.as_secs());
    match tokio::time::timeout(limit, owner.query(&build.schema_sql)).await {
        Err(_) => return Err(slow("the schema")),
        Ok(Err(err)) => {
            return Err(match err.db() {
                Some(db) => format!("the schema does not apply: {}", db.message),
                None => err.to_string(),
            });
        }
        Ok(Ok(_)) => {}
    }

    tell(events, stage, Stage::Rows).await;
    let options = seed::Options {
        size: build.size,
        profile: build.profile,
        seed: build.seed,
        max_rows_per_table: None,
    };
    let total = model
        .entities
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                remus_core::model::EntityKind::Table
                    | remus_core::model::EntityKind::PartitionedTable
            )
        })
        .count();
    let mut done = 0;
    let (table_tx, mut table_rx) = mpsc::unbounded_channel();
    let loaded = seed::load(
        Arc::clone(&host.seeder),
        model,
        options,
        &mut owner,
        limit,
        move |schema, table, rows| {
            done += 1;
            let _ = table_tx.send(BuildEvent::Table {
                schema: schema.to_owned(),
                table: table.to_owned(),
                rows,
                done,
                total,
            });
        },
    );
    let forward = async {
        while let Some(event) = table_rx.recv().await {
            let _ = events.send(event).await;
        }
    };
    let (loaded, ()) = tokio::join!(loaded, forward);
    let loaded = loaded.map_err(|e| format!("the rows do not load: {e}"))?;

    tell(events, stage, Stage::Analyze).await;
    tokio::time::timeout(limit, owner.query("ANALYZE"))
        .await
        .map_err(|_| slow("gathering statistics"))?
        .map_err(|e| e.to_string())?;
    let tables = tokio::time::timeout(limit, count(&mut owner))
        .await
        .map_err(|_| slow("counting the rows"))?
        .map_err(|e| e.to_string())?;
    owner.close().await;

    tell(events, stage, Stage::Snapshot).await;
    host.reset_snapshot(entry).await.map_err(|e| describe(&e))?;
    Ok(BuildEvent::Ready {
        tables,
        report: loaded.report,
        // The stand-in generates nothing: no version to show.
        generator: match host.seeder.version() {
            0 => "none".to_owned(),
            version => version.to_string(),
        },
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

/// Rows per table the schema's owner owns (partitions counted in their
/// parent), exactly: small and medium playgrounds count fast. The schema
/// may define operators: the service's own SQL resolves in `pg_catalog`. A
/// table forcing row-level security on its owner is counted with that off,
/// in a transaction rolled back: its policies (which may read settings
/// only the application sets) are for the people querying it.
async fn count(conn: &mut Conn) -> Result<Vec<TableRows>, PgError> {
    let tables = conn
        .query(
            "BEGIN; SET LOCAL search_path = pg_catalog; \
             SELECT n.nspname, c.relname, c.relforcerowsecurity::text FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN ('r', 'p') AND NOT c.relispartition \
               AND pg_catalog.pg_get_userbyid(c.relowner) = 'play_owner' \
             ORDER BY 1, 2; COMMIT",
        )
        .await?;
    let mut counted = Vec::with_capacity(tables.len());
    for row in tables {
        let (Some(schema), Some(table)) = (
            row.first().cloned().flatten(),
            row.get(1).cloned().flatten(),
        ) else {
            continue;
        };
        let relation = format!("{}.{}", quote_ident(&schema), quote_ident(&table));
        let forced = row.get(2).cloned().flatten().as_deref() == Some("true");
        let unforce = if forced {
            format!("ALTER TABLE {relation} NO FORCE ROW LEVEL SECURITY; ")
        } else {
            String::new()
        };
        let rows = conn
            .query(&format!(
                "BEGIN; SET LOCAL search_path = pg_catalog; {unforce}\
                 SELECT pg_catalog.count(*) FROM {relation}; ROLLBACK"
            ))
            .await?
            .first()
            .and_then(|r| r.first().cloned().flatten())
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        counted.push(TableRows {
            schema,
            table,
            rows,
        });
    }
    Ok(counted)
}

impl Host {
    /// A clean stop, the generated copy, a start: what Reset restores.
    pub async fn reset_snapshot(&self, entry: &Arc<Entry>) -> Result<(), HostError> {
        let _ = entry.notices.send(super::host::Notice::Pause(
            "the playground is being prepared".to_owned(),
        ));
        self.forget(entry).await;
        self.runner.stop(entry.id).await?;
        self.runner.snapshot(entry.id).await?;
        self.start_locked(entry).await?;
        Ok(())
    }
}
