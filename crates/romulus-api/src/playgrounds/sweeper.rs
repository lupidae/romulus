//! Playgrounds' lifetimes (docs/playground.md, "Lifetime"), swept now and
//! every [`SWEEP_EVERY`]:
//!
//! - **expired**: unused for 7 days, or their draft published or deleted, or
//!   their database deleted: their cluster is destroyed, the row kept as
//!   `expired` (its page says so) for 30 days with its history;
//! - **builds that will never finish**: started before this process (its
//!   build task died with the last one) or running for over 30 minutes:
//!   `failed`, and whatever the host holds of them destroyed;
//! - **orphans**: clusters the host holds that no live row names (a delete
//!   whose host call failed), a migration test running included;
//! - **migration tests left behind** by an earlier process (their task died
//!   with it): `error`, and their playground destroyed;
//! - **purged**: expired and failed rows 30 days old.
//!
//! The host stops idle playgrounds itself (15 minutes without anyone).

use std::time::Duration;

use chrono::{DateTime, Utc};
use romulus_playground::protocol::Access;
use uuid::Uuid;

use super::host::{Caller, HostError};
use crate::AppState;

pub const SWEEP_EVERY: Duration = Duration::from_mins(5);

fn romulus(org: Uuid) -> Caller {
    Caller {
        org,
        user: None,
        access: Access::Develop,
    }
}

/// What one sweep did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Swept {
    pub expired: usize,
    pub failed: usize,
    pub orphans: usize,
    pub purged: u64,
    /// Migration tests an earlier process left running.
    pub tests: usize,
}

/// Sweeps every [`SWEEP_EVERY`] until aborted; the first sweep now.
pub fn sweeper(state: AppState, every: Duration) -> tokio::task::JoinHandle<()> {
    let started = Utc::now();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        loop {
            tick.tick().await;
            match sweep(&state, started).await {
                Ok(swept) if swept != Swept::default() => {
                    tracing::info!(?swept, "playgrounds swept");
                }
                Ok(_) => {}
                Err(err) => tracing::warn!(error = %err, "playground sweep failed"),
            }
        }
    })
}

/// Tests a restart left queued or running end as errors: the playground
/// each held (read before the update clears it) and its organization.
async fn end_left_tests(
    state: &AppState,
    started: DateTime<Utc>,
) -> Result<Vec<LeftTest>, sqlx::Error> {
    sqlx::query_as!(
        LeftTest,
        r#"WITH left_over AS (
               SELECT id, playground_id FROM migration_tests
               WHERE status IN ('queued', 'running') AND created_at < $1
               FOR UPDATE
           )
           UPDATE migration_tests t
           SET status = 'error', finished_at = now(), playground_id = NULL,
               failure = 'The server restarted during the test. Run it again.'
           FROM left_over l
           WHERE t.id = l.id
           RETURNING l.playground_id, t.org_id"#,
        started,
    )
    .fetch_all(&state.db)
    .await
}

struct LeftTest {
    playground_id: Option<Uuid>,
    org_id: Uuid,
}

/// One sweep. `started`: when this process started (builds older than it
/// have no task left to finish them).
pub async fn sweep(state: &AppState, started: DateTime<Utc>) -> Result<Swept, sqlx::Error> {
    let mut swept = Swept::default();
    let Ok(backend) = state.playgrounds.host.backend() else {
        return Ok(swept);
    };
    let destroy = |id: Uuid, org: Uuid| async move {
        match backend.destroy(id, romulus(org)).await {
            Ok(()) => true,
            Err(err) if err.code() == Some("not_found") => true,
            Err(HostError::Refused { message, .. } | HostError::Unavailable(message)) => {
                tracing::warn!(playground = %id, %message, "cannot destroy an expired playground yet");
                false
            }
            Err(HostError::Off) => false,
        }
    };

    let due = sqlx::query!(
        r#"SELECT p.id, p.org_id
           FROM playgrounds p
           JOIN databases b ON b.id = p.database_id
           LEFT JOIN drafts d ON d.id = p.draft_id
           WHERE p.status = 'ready'
             AND (p.expires_at < now()
                  OR b.deleted_at IS NOT NULL
                  OR (p.draft_id IS NOT NULL AND d.status <> 'open'))"#
    )
    .fetch_all(&state.db)
    .await?;
    for playground in due {
        if destroy(playground.id, playground.org_id).await {
            sqlx::query!(
                "UPDATE playgrounds SET status = 'expired' WHERE id = $1 AND status = 'ready'",
                playground.id
            )
            .execute(&state.db)
            .await?;
            swept.expired += 1;
        }
    }

    let stuck = sqlx::query!(
        "UPDATE playgrounds SET status = 'failed',
                failure = 'The build did not finish (the server restarted, or it took too long). Delete it and try again.'
         WHERE status = 'building' AND (created_at < $1 OR created_at < now() - interval '30 minutes')
         RETURNING id, org_id",
        started,
    )
    .fetch_all(&state.db)
    .await?;
    for playground in stuck {
        state.playgrounds.set_progress(playground.id, None);
        destroy(playground.id, playground.org_id).await;
        swept.failed += 1;
    }

    let left = end_left_tests(state, started).await?;
    for test in left {
        if let Some(playground) = test.playground_id {
            destroy(playground, test.org_id).await;
        }
        swept.tests += 1;
    }

    if let Ok(listing) = backend.list().await {
        let live: std::collections::HashSet<Uuid> = sqlx::query_scalar!(
            r#"SELECT id AS "id!" FROM playgrounds WHERE status IN ('building', 'ready')
               UNION ALL
               SELECT playground_id FROM migration_tests
               WHERE status IN ('queued', 'running') AND playground_id IS NOT NULL"#
        )
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .collect();
        for orphan in listing.playgrounds.iter().filter(|p| !live.contains(&p.id)) {
            if destroy(orphan.id, Uuid::nil()).await {
                swept.orphans += 1;
            }
        }
    }

    swept.purged = sqlx::query!(
        "DELETE FROM playgrounds
         WHERE status IN ('expired', 'failed') AND last_used_at < now() - interval '30 days'"
    )
    .execute(&state.db)
    .await?
    .rows_affected();
    Ok(swept)
}
