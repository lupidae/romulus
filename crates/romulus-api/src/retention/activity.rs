//! What counts as using an account, for the inactive accounts
//! (`super::inactive`): `users.last_seen_at` moves, and an inactivity notice
//! no longer holds. Every way in calls [`account_used`]:
//!
//! - a sign-in (`auth::session::create`) and a session's use, when the
//!   session itself is touched (`auth::session`);
//! - a personal access token's use by the person's agent
//!   (`auth::access_tokens::Agent::touch`);
//! - an API token's use by CI, for the account that made it
//!   (`databases::tokens::Bearer`): a database someone keeps pushing to is
//!   theirs in use;
//! - a connected app's OAuth access token, by the person's agent, the same
//!   way (`Agent::touch`, through `oauth::apps::touch`). Getting tokens is
//!   not using the account: a client refreshing without calling counts
//!   nothing (and its grant lasts 90 days at most).

use std::time::Duration;

use sqlx::PgExecutor;
use uuid::Uuid;

/// `users.last_seen_at` moves at most this often (a notice to clear is
/// always cleared): a busy agent or CI does not write the account each
/// request.
pub const ACCOUNT_TOUCH_EVERY: Duration = Duration::from_hours(1);

/// The account `user` was used now: see the module documentation.
pub async fn account_used(db: impl PgExecutor<'_>, user: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE users SET last_seen_at = now(), inactivity_notice_at = NULL,
                          inactivity_failures = 0, inactivity_retry_at = NULL
         WHERE id = $1
           AND (last_seen_at < now() - make_interval(secs => $2) OR inactivity_notice_at IS NOT NULL)",
        user,
        ACCOUNT_TOUCH_EVERY.as_secs_f64(),
    )
    .execute(db)
    .await
    .map(drop)
}
