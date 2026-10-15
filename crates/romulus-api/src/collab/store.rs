//! Where a draft's Y.Doc lives between sessions: `draft_updates` (append
//! only), folded now and then into `draft_snapshots`, and the last compile
//! in `draft_compiles`.
//!
//! Each loaded draft has one [`Writer`]: a task that runs its writes one
//! after the other. Ordering is what makes compaction safe: a snapshot is
//! the doc's whole state when it was queued, so it covers every update
//! queued before it, which the writer has inserted by then; it records the
//! last of their ids, and the rows up to it are deleted. Two writers for one
//! draft would break that, hence one API instance (docs/MVP.md).

use std::{sync::Arc, time::Duration};

use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::domain::DraftStatus;

/// What loading a draft finds.
pub struct Loaded {
    pub database_id: Uuid,
    pub status: DraftStatus,
    /// The snapshot, then every update after it, in order.
    pub updates: Vec<Vec<u8>>,
    /// The id of the last update loaded (or that the snapshot covers).
    pub last_update_id: i64,
    /// Update rows not yet folded into the snapshot.
    pub rows_since_snapshot: usize,
    pub compile: Option<SavedCompile>,
}

/// A `draft_compiles` row.
#[derive(Debug, Clone)]
pub struct SavedCompile {
    pub source_hash: Vec<u8>,
    pub model: Option<Value>,
    pub errors: Option<Value>,
    pub good_model: Option<Value>,
    pub good_tables: Option<Value>,
}

/// `None` when the draft does not exist.
pub async fn load(db: &PgPool, draft_id: Uuid) -> Result<Option<Loaded>, sqlx::Error> {
    let mut tx = db.begin().await?;
    let Some(draft) = sqlx::query!(
        r#"SELECT database_id, status AS "status: DraftStatus" FROM drafts WHERE id = $1"#,
        draft_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(None);
    };
    let snapshot = sqlx::query!(
        "SELECT state, upto_update_id FROM draft_snapshots WHERE draft_id = $1",
        draft_id,
    )
    .fetch_optional(&mut *tx)
    .await?;
    let upto = snapshot.as_ref().map_or(0, |s| s.upto_update_id);
    let rows = sqlx::query!(
        r#"SELECT id, update AS "update!" FROM draft_updates
           WHERE draft_id = $1 AND id > $2 ORDER BY id"#,
        draft_id,
        upto,
    )
    .fetch_all(&mut *tx)
    .await?;
    let compile = sqlx::query_as!(
        SavedCompile,
        "SELECT source_hash, model, errors, good_model, good_tables
         FROM draft_compiles WHERE draft_id = $1",
        draft_id,
    )
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;

    let last_update_id = rows.last().map_or(upto, |row| row.id);
    let rows_since_snapshot = rows.len();
    let updates = snapshot
        .map(|s| s.state)
        .into_iter()
        .chain(rows.into_iter().map(|row| row.update))
        .collect();
    Ok(Some(Loaded {
        database_id: draft.database_id,
        status: draft.status,
        updates,
        last_update_id,
        rows_since_snapshot,
        compile,
    }))
}

/// Appends one update; also marks the draft as just edited (at most every
/// few seconds, so typing does not rewrite the row on every keystroke).
pub async fn append(db: &PgPool, draft_id: Uuid, update: &[u8]) -> Result<i64, sqlx::Error> {
    let id = sqlx::query_scalar!(
        "INSERT INTO draft_updates (draft_id, update) VALUES ($1, $2) RETURNING id",
        draft_id,
        update,
    )
    .fetch_one(db)
    .await?;
    sqlx::query!(
        "UPDATE drafts SET updated_at = now()
         WHERE id = $1 AND updated_at < now() - interval '5 seconds'",
        draft_id,
    )
    .execute(db)
    .await?;
    Ok(id)
}

/// Replaces the snapshot with `state`, which covers every update up to
/// `upto`, and deletes those.
pub async fn compact(
    db: &PgPool,
    draft_id: Uuid,
    state: &[u8],
    upto: i64,
) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;
    sqlx::query!(
        "INSERT INTO draft_snapshots (draft_id, state, upto_update_id) VALUES ($1, $2, $3)
         ON CONFLICT (draft_id) DO UPDATE
         SET state = EXCLUDED.state, upto_update_id = EXCLUDED.upto_update_id, created_at = now()",
        draft_id,
        state,
        upto,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM draft_updates WHERE draft_id = $1 AND id <= $2",
        draft_id,
        upto,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// What a compile leaves in `draft_compiles`.
#[derive(Debug)]
pub struct CompileRow {
    pub source_hash: Vec<u8>,
    /// When it compiled; it also becomes `good_model`.
    pub model: Option<Arc<Value>>,
    /// When it did not: the diagnostics. `good_model` stays as it was.
    pub errors: Option<Value>,
    /// With `model`: where each table is created.
    pub tables: Option<Value>,
}

pub async fn save_compile(
    db: &PgPool,
    draft_id: Uuid,
    row: &CompileRow,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO draft_compiles (draft_id, source_hash, model, errors, good_model, good_tables)
         VALUES ($1, $2, $3, $4, $3, $5)
         ON CONFLICT (draft_id) DO UPDATE
         SET compiled_at = now(), source_hash = EXCLUDED.source_hash, model = EXCLUDED.model,
             errors = EXCLUDED.errors,
             good_model = COALESCE(EXCLUDED.good_model, draft_compiles.good_model),
             good_tables = COALESCE(EXCLUDED.good_tables, draft_compiles.good_tables)",
        draft_id,
        row.source_hash,
        row.model.as_deref(),
        row.errors,
        row.tables,
    )
    .execute(db)
    .await?;
    Ok(())
}

pub enum Job {
    Append(Vec<u8>),
    /// The doc's whole state: becomes the snapshot.
    Compact(Vec<u8>),
    Compile(Box<CompileRow>),
    /// Answered once every job queued before it is done.
    Flush(oneshot::Sender<()>),
}

/// The queue of a draft's writes. Dropping every handle ends the task once
/// the queue is empty.
#[derive(Clone)]
pub struct Writer(mpsc::UnboundedSender<Job>);

impl Writer {
    pub fn spawn(db: PgPool, draft_id: Uuid, last_update_id: i64) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(db, draft_id, last_update_id, rx));
        Self(tx)
    }

    pub fn send(&self, job: Job) {
        // The task only ends once every sender is gone.
        let _ = self.0.send(job);
    }

    /// Waits for every write queued so far.
    pub async fn flush(&self) {
        let (done, wait) = oneshot::channel();
        self.send(Job::Flush(done));
        let _ = wait.await;
    }
}

const ATTEMPTS: u32 = 5;

async fn run(db: PgPool, draft_id: Uuid, mut last_id: i64, mut jobs: mpsc::UnboundedReceiver<Job>) {
    while let Some(job) = jobs.recv().await {
        match job {
            Job::Append(update) => {
                if let Some(id) = retry("append an update", draft_id, || {
                    append(&db, draft_id, &update)
                })
                .await
                {
                    last_id = id;
                }
            }
            Job::Compact(state) => {
                retry("compact", draft_id, || {
                    compact(&db, draft_id, &state, last_id)
                })
                .await;
            }
            Job::Compile(row) => {
                retry("save a compile", draft_id, || {
                    save_compile(&db, draft_id, &row)
                })
                .await;
            }
            Job::Flush(done) => {
                let _ = done.send(());
            }
        }
    }
}

/// Runs a write until it succeeds, a few times with a growing pause (the
/// database restarting). A write that never succeeds is logged as lost.
async fn retry<T, F, Fut>(what: &str, draft_id: Uuid, mut write: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, sqlx::Error>>,
{
    let mut pause = Duration::from_millis(100);
    for attempt in 1..=ATTEMPTS {
        match write().await {
            Ok(value) => return Some(value),
            Err(err) if attempt < ATTEMPTS => {
                tracing::warn!(draft = %draft_id, error = %err, attempt, "collab: cannot {what}, retrying");
                tokio::time::sleep(pause).await;
                pause *= 2;
            }
            Err(err) => {
                tracing::error!(draft = %draft_id, error = %err, "collab: cannot {what}, giving up");
            }
        }
    }
    None
}
