//! What romulus keeps, and for how long (docs/MVP.md "Retention and
//! deletion"):
//!
//! - **Accounts and organizations, deleted on request** ([`deletion`]): an
//!   operator runs `romulus-api delete-user EMAIL` or `delete-org SLUG`
//!   (`--dry-run` first: what would go). Everything the account or the
//!   organization alone holds goes, in one transaction; what it wrote in an
//!   organization it leaves stays the organization's (authored by a deleted
//!   account; the audit log keeps the name it recorded). A single sign-on
//!   account is deleted at the identity provider too ([`idp`]).
//! - **Deleted databases** are purged 30 days after their deletion, with
//!   everything they held but their audit events ([`purge::databases`]).
//! - **Invitations** nobody accepted are deleted 30 days after they expired
//!   ([`purge::invitations`]); a revoked one is deleted at once, an accepted
//!   one stays with the membership.
//! - **Inactive accounts** ([`inactive`], `ROMULUS_DELETE_INACTIVE_ACCOUNTS`,
//!   off by default): unused for 3 years (`users.last_seen_at`), emailed 30
//!   days before, then deleted as on request.
//!
//! [`sweeper`] runs the purges and the inactive accounts now and every
//! [`SWEEP_EVERY`], one process at a time ([`SWEEP_LOCK`]). A deletion
//! changes what the running API holds in memory (open drafts, websockets)
//! wherever it ran, the CLI included: [`live`] tells the API. What counts
//! as using an account is [`activity`]'s.
//!
//! Kept elsewhere, by their own rules: invoices (in Stripe), the sign-in log
//! (`auth_events`, 90 days: it records the deletions too), Stripe's webhook
//! events (`billing_events`, 90 days: `billing::events`), backups (the
//! operator's), Zitadel's event store (until the operator redacts it:
//! docs/self-host/privacy.md).
//!
//! Self-service deletion (later): an endpoint calls
//! [`deletion::Deleter::delete_account`] with the signed-in person's id,
//! answering its [`deletion::DeletionError`]s (the last admin, an
//! organization that pays) as 409s.

pub mod activity;
pub mod deletion;
pub mod idp;
pub mod inactive;
pub mod live;
pub mod purge;

use std::time::Duration;

use crate::AppState;

/// How often [`sweeper`] runs.
pub const SWEEP_EVERY: Duration = Duration::from_hours(1);

/// What one sweep did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Swept {
    pub databases: u64,
    pub invitations: u64,
    pub notices: u64,
    pub accounts: u64,
}

/// The advisory lock one sweep holds (`hashtext` of it): a second process
/// (another API started by mistake, a deploy's overlap) skips its sweep
/// rather than send a notice twice.
pub const SWEEP_LOCK: &str = "romulus.retention";

/// Sweeps once, unless another process is sweeping: the purges, then (when
/// on) the inactive accounts' notices and deletions. Each step logs its own
/// failure and lets the others run.
pub async fn sweep(state: &AppState) -> Swept {
    let lock = match crate::db::try_advisory_lock(&state.db, SWEEP_LOCK).await {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            tracing::info!("retention: another process is sweeping, skipped");
            return Swept::default();
        }
        Err(err) => {
            tracing::warn!(error = %err, "retention: cannot take the sweep's lock");
            return Swept::default();
        }
    };
    let swept = sweep_locked(state).await;
    if let Err(err) = lock.release().await {
        tracing::debug!(error = %err, "retention: releasing the sweep's lock");
    }
    swept
}

async fn sweep_locked(state: &AppState) -> Swept {
    let mut swept = Swept::default();
    match purge::databases(&state.db).await {
        Ok(n) => swept.databases = n,
        Err(err) => tracing::warn!(error = %err, "purging deleted databases failed"),
    }
    match purge::invitations(&state.db).await {
        Ok(n) => swept.invitations = n,
        Err(err) => tracing::warn!(error = %err, "purging expired invitations failed"),
    }
    if state.config.delete_inactive_accounts {
        match inactive::send_notices(state).await {
            Ok(n) => swept.notices = n,
            Err(err) => tracing::warn!(error = %err, "inactive accounts: notices failed"),
        }
        match inactive::delete_due(state).await {
            Ok(n) => swept.accounts = n,
            Err(err) => tracing::warn!(error = %err, "inactive accounts: deletions failed"),
        }
    }
    swept
}

/// Sweeps now, then every `every`.
pub fn sweeper(state: AppState, every: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let swept = sweep(&state).await;
            if swept != Swept::default() {
                tracing::info!(?swept, "retention swept");
            }
            tokio::time::sleep(every).await;
        }
    })
}
