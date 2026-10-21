//! Telling the running API what a deletion took away, wherever it ran: the
//! CLI (`romulus-api delete-org`) is another process, and the API holds open
//! drafts and websockets in memory.
//!
//! A deletion writes a [`Notice`] per database and per membership it ends
//! with `pg_notify` in its own transaction (Postgres delivers them at
//! commit, never for a rollback); [`listen`] hears them and does what the
//! REST endpoints do in process: closes the databases' drafts
//! (`Hub::database_deleted`) and publishes membership changes (the sockets
//! named close). Missed while the API was down or reconnecting: nothing was
//! open then, or the sockets' own session checks close them.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, postgres::PgListener};
use uuid::Uuid;

use crate::{AppState, domain::Role, orgs::events::MembershipChange};

/// The channel deletions notify on.
pub const CHANNEL: &str = "romulus_deleted";

/// One thing a committed deletion ended. Small: a notification carries at
/// most 8000 bytes, so a deletion sends one per database and member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Notice {
    /// The database is gone (its organization deleted): its drafts close.
    DatabaseDeleted { database: Uuid },
    /// `user` is no longer a member of `org` (an account or an organization
    /// deleted).
    MemberRemoved { org: Uuid, user: Uuid },
    /// `user` now has `role` in `org` (made an admin when the only one's
    /// account went).
    RoleChanged { org: Uuid, user: Uuid, role: Role },
}

/// Queues `notice` in the caller's transaction.
pub async fn notify(tx: &mut PgConnection, notice: Notice) -> Result<(), sqlx::Error> {
    let payload = serde_json::to_string(&notice).expect("a notice serializes");
    sqlx::query!("SELECT pg_notify($1, $2)", CHANNEL, payload)
        .execute(tx)
        .await?;
    Ok(())
}

/// Does in this process what `notice` says happened.
pub fn apply(state: &AppState, notice: Notice) {
    match notice {
        Notice::DatabaseDeleted { database } => state.collab.database_deleted(database, "romulus"),
        Notice::MemberRemoved { org, user } => {
            state.membership.publish(MembershipChange::Removed {
                org_id: org,
                user_id: user,
            });
        }
        Notice::RoleChanged { org, user, role } => {
            state.membership.publish(MembershipChange::RoleChanged {
                org_id: org,
                user_id: user,
                role,
            });
        }
    }
}

/// Listens on [`CHANNEL`] until aborted, reconnecting after a failure.
pub fn listen(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(err) = listen_once(&state).await {
                tracing::warn!(error = %err, "deletion notices: listening failed, again in 5 s");
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
}

async fn listen_once(state: &AppState) -> Result<(), sqlx::Error> {
    let mut listener = PgListener::connect_with(&state.db).await?;
    listener.listen(CHANNEL).await?;
    loop {
        // Reconnects by itself after a dropped connection.
        let notification = listener.recv().await?;
        match serde_json::from_str::<Notice>(notification.payload()) {
            Ok(notice) => apply(state, notice),
            Err(err) => tracing::warn!(error = %err, "deletion notices: not a notice"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_fit_a_notification_and_read_back() {
        let notice = Notice::RoleChanged {
            org: Uuid::from_u128(1),
            user: Uuid::from_u128(2),
            role: Role::Admin,
        };
        let text = serde_json::to_string(&notice).unwrap();
        assert!(text.len() < 8000);
        assert_eq!(serde_json::from_str::<Notice>(&text).unwrap(), notice);
    }
}
