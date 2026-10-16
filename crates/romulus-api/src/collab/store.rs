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

use super::{protocol::ReviewRequest, review};
use crate::domain::DraftStatus;

/// What loading a draft finds.
pub struct Loaded {
    pub database_id: Uuid,
    pub org_id: Uuid,
    pub status: DraftStatus,
    /// Its database is deleted (its drafts are then deleted too).
    pub database_deleted: bool,
    /// The snapshot, then every update after it, in order.
    pub updates: Vec<Vec<u8>>,
    /// The id of the last update loaded (or that the snapshot covers).
    pub last_update_id: i64,
    /// Update rows not yet folded into the snapshot.
    pub rows_since_snapshot: usize,
    pub compile: Option<SavedCompile>,
    /// Editors, approvals (of members only), the review request, and the
    /// organization's setting.
    pub review: review::Review,
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

/// `None` when the draft does not exist. One snapshot of the database
/// (REPEATABLE READ): the snapshot row and the updates after it agree.
pub async fn load(db: &PgPool, draft_id: Uuid) -> Result<Option<Loaded>, sqlx::Error> {
    let mut tx = db
        .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .await?;
    let Some(draft) = sqlx::query!(
        r#"SELECT d.database_id, d.status AS "status: DraftStatus",
                  b.deleted_at IS NOT NULL AS "database_deleted!", b.org_id, o.approvals_required
           FROM drafts d JOIN databases b ON b.id = d.database_id
           JOIN organizations o ON o.id = b.org_id
           WHERE d.id = $1"#,
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
    let editors = sqlx::query_scalar!(
        "SELECT user_id FROM draft_editors WHERE draft_id = $1",
        draft_id
    )
    .fetch_all(&mut *tx)
    .await?;
    let approvals = sqlx::query!(
        "SELECT a.user_id, u.name, a.sql_sha256, a.approved_at
         FROM draft_approvals a
         JOIN users u ON u.id = a.user_id
         JOIN memberships m ON m.user_id = a.user_id AND m.org_id = $2
         WHERE a.draft_id = $1 ORDER BY a.approved_at",
        draft_id,
        draft.org_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    let requested = sqlx::query!(
        r#"SELECT r.requested_at, u.id AS "user_id?", u.name AS "name?"
           FROM draft_review_requests r LEFT JOIN users u ON u.id = r.requested_by
           WHERE r.draft_id = $1"#,
        draft_id,
    )
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;

    let review = review::Review {
        required: i32::from(draft.approvals_required),
        editors: editors.into_iter().collect(),
        approvals: approvals
            .into_iter()
            .map(|a| review::Approval {
                user_id: a.user_id,
                name: a.name,
                sha256: hex(&a.sql_sha256),
                at: a.approved_at,
            })
            .collect(),
        requested: requested.map(|r| ReviewRequest {
            user_id: r.user_id,
            name: r.name,
            at: r.requested_at,
        }),
    };
    let last_update_id = rows.last().map_or(upto, |row| row.id);
    let rows_since_snapshot = rows.len();
    let updates = snapshot
        .map(|s| s.state)
        .into_iter()
        .chain(rows.into_iter().map(|row| row.update))
        .collect();
    Ok(Some(Loaded {
        database_id: draft.database_id,
        org_id: draft.org_id,
        status: draft.status,
        database_deleted: draft.database_deleted,
        updates,
        last_update_id,
        rows_since_snapshot,
        compile,
        review,
    }))
}

/// Lowercase hex, as `compile::sha256_hex` writes digests.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Records that `user_id` edited the draft's SQL (once is enough).
pub async fn mark_editor(db: &PgPool, draft_id: Uuid, user_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO draft_editors (draft_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        draft_id,
        user_id,
    )
    .execute(db)
    .await?;
    Ok(())
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
/// `upto`, and deletes those. Never with an older snapshot than the stored
/// one (a stray writer of a previous room, say).
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
         SET state = EXCLUDED.state, upto_update_id = EXCLUDED.upto_update_id, created_at = now()
         WHERE draft_snapshots.upto_update_id <= EXCLUDED.upto_update_id",
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
    /// Someone's first edit of the SQL (`draft_editors`).
    Editor(Uuid),
    /// Answered once every job queued before it is done.
    Flush(oneshot::Sender<()>),
}

/// The queue of a draft's writes. Dropping every handle ends the task once
/// the queue is empty.
#[derive(Clone)]
pub struct Writer(mpsc::UnboundedSender<Job>);

impl Writer {
    /// `failed` hears of every append or compaction that failed for good.
    pub fn spawn(
        db: PgPool,
        draft_id: Uuid,
        last_update_id: i64,
        failed: mpsc::UnboundedSender<()>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(db, draft_id, last_update_id, rx, failed));
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

async fn run(
    db: PgPool,
    draft_id: Uuid,
    mut last_id: i64,
    mut jobs: mpsc::UnboundedReceiver<Job>,
    failed: mpsc::UnboundedSender<()>,
) {
    while let Some(job) = jobs.recv().await {
        match job {
            Job::Append(update) => {
                match retry("append an update", draft_id, || {
                    append(&db, draft_id, &update)
                })
                .await
                {
                    Some(id) => last_id = id,
                    None => {
                        let _ = failed.send(());
                    }
                }
            }
            Job::Compact(state) => {
                if retry("compact", draft_id, || {
                    compact(&db, draft_id, &state, last_id)
                })
                .await
                .is_none()
                {
                    let _ = failed.send(());
                }
            }
            Job::Compile(row) => {
                retry("save a compile", draft_id, || {
                    save_compile(&db, draft_id, &row)
                })
                .await;
            }
            Job::Editor(user_id) => {
                retry("record an editor", draft_id, || {
                    mark_editor(&db, draft_id, user_id)
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
