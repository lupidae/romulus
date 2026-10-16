//! An import's models while it replays (`import_staging`): written as the
//! replay reads them (`run`), moved into versions when the import is
//! stored (`versions::store::store_import`), deleted when it ends. Rows an
//! import left behind (a process stopped mid-import) are swept at startup
//! and every [`SWEEP_EVERY`] ([`sweeper`]), once older than any import
//! runs, and never those of an import in flight ([`Turns`]).

use std::time::Duration;

use sqlx::PgPool;
use uuid::Uuid;

use super::Turns;

/// How often [`sweeper`] looks for rows left behind.
pub const SWEEP_EVERY: Duration = Duration::from_mins(10);

/// A new import's id in the staging table.
pub fn new_id() -> Uuid {
    let mut bytes = [0u8; 16];
    // The OS random source failing leaves no safe way to continue.
    getrandom::fill(&mut bytes).expect("the OS random number generator is available");
    uuid::Builder::from_random_bytes(bytes).into_uuid()
}

/// Removes an import's staged models.
pub async fn discard(db: &PgPool, staging: Uuid) {
    let deleted = sqlx::query!("DELETE FROM import_staging WHERE import_id = $1", staging)
        .execute(db)
        .await;
    if let Err(err) = deleted {
        tracing::warn!(error = %err, %staging, "staged models left behind");
    }
}

/// Removes the rows imports left behind: older than an hour (an import
/// waits 10 minutes at most for its turn, and replays for 15 at most, wall
/// clock), and not an import's in flight. Returns how many.
pub async fn sweep(db: &PgPool, turns: &Turns) -> Result<u64, sqlx::Error> {
    let in_flight = turns.in_flight();
    let deleted = sqlx::query!(
        "DELETE FROM import_staging
         WHERE created_at < now() - interval '1 hour' AND import_id <> ALL($1)",
        &in_flight,
    )
    .execute(db)
    .await?;
    Ok(deleted.rows_affected())
}

/// Sweeps now (rows a stopped process left), then every `every`.
pub fn sweeper(db: PgPool, turns: Turns, every: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match sweep(&db, &turns).await {
                Ok(0) => {}
                Ok(rows) => tracing::info!(rows, "swept models imports left behind"),
                Err(err) => tracing::warn!(error = %err, "sweeping staged models failed"),
            }
            tokio::time::sleep(every).await;
        }
    })
}
