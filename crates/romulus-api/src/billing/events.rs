//! Stripe's webhook events as kept (`billing_events`): id, type,
//! organization and arrival, so that a delivery seen again is not applied
//! twice ([`super::webhook`]). The privacy policy keeps them 90 days:
//! [`sweeper`] deletes older ones now and every [`SWEEP_EVERY`], one
//! process at a time ([`SWEEP_LOCK`]), billing on or off (those of a past
//! configuration go too).
//!
//! Forgetting an event never applies it twice: Stripe retries a delivery
//! for three days, and an event being applied now arrived now.

use std::time::Duration;

use sqlx::PgPool;

/// How often [`sweeper`] runs.
pub const SWEEP_EVERY: Duration = Duration::from_hours(1);

/// The advisory lock one sweep holds (`hashtext` of it): a second process
/// skips its sweep.
pub const SWEEP_LOCK: &str = "romulus.billing_events";

/// Deletes the events received more than 90 days ago, unless another
/// process is sweeping them: how many, `None` when it skipped or failed
/// (logged).
pub async fn sweep(db: &PgPool) -> Option<u64> {
    let lock = match crate::db::try_advisory_lock(db, SWEEP_LOCK).await {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            tracing::info!("billing: another process is sweeping webhook events, skipped");
            return None;
        }
        Err(err) => {
            tracing::warn!(error = %err, "billing: cannot take the webhook events sweep's lock");
            return None;
        }
    };
    let deleted =
        sqlx::query!("DELETE FROM billing_events WHERE received_at < now() - interval '90 days'")
            .execute(db)
            .await;
    // Closing the connection releases the lock too.
    if let Err(err) = sqlx::Connection::close(lock).await {
        tracing::debug!(error = %err, "billing: closing the webhook events sweep's lock");
    }
    match deleted {
        Ok(done) => Some(done.rows_affected()),
        Err(err) => {
            tracing::warn!(error = %err, "billing: deleting old webhook events failed");
            None
        }
    }
}

/// Sweeps now, then every `every`.
pub fn sweeper(db: PgPool, every: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match sweep(&db).await {
                None | Some(0) => {}
                Some(deleted) => {
                    tracing::info!(deleted, "billing: webhook events past 90 days deleted");
                }
            }
            tokio::time::sleep(every).await;
        }
    })
}
