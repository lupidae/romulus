//! Membership changes that long-lived connections must react to.
//!
//! REST requests re-check membership on every call (`auth::Member`), but a
//! websocket authenticated an hour ago does not. docs/MVP.md: "A role change
//! or removal closes that member's open websockets." The orgs module
//! publishes every change here after its transaction commits; the collab
//! server subscribes once per socket and closes the ones it names:
//!
//! ```ignore
//! let mut changes = state.membership.subscribe();
//! loop {
//!     tokio::select! {
//!         change = changes.recv() => match change {
//!             Ok(change) if change.concerns(org_id, user_id) => break, // close; the client reconnects with its new rights
//!             Ok(_) => {}
//!             // Missed messages may have named this socket: close to be safe.
//!             Err(RecvError::Lagged(_)) => break,
//!             Err(RecvError::Closed) => break,
//!         },
//!         message = socket.recv() => { … }
//!     }
//! }
//! ```

use tokio::sync::broadcast;
use uuid::Uuid;

use crate::domain::Role;

/// What happened to one member of one organization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembershipChange {
    /// The member now has `role`. Their sockets must close: a reviewer may
    /// not keep a socket that was allowed to write.
    RoleChanged {
        org_id: Uuid,
        user_id: Uuid,
        role: Role,
    },
    /// The member left or was removed: no access at all any more.
    Removed { org_id: Uuid, user_id: Uuid },
}

impl MembershipChange {
    pub fn org_id(self) -> Uuid {
        match self {
            Self::RoleChanged { org_id, .. } | Self::Removed { org_id, .. } => org_id,
        }
    }

    pub fn user_id(self) -> Uuid {
        match self {
            Self::RoleChanged { user_id, .. } | Self::Removed { user_id, .. } => user_id,
        }
    }

    /// Whether a connection of `user_id` inside `org_id` must close.
    pub fn concerns(self, org_id: Uuid, user_id: Uuid) -> bool {
        self.org_id() == org_id && self.user_id() == user_id
    }
}

/// A broadcast channel of [`MembershipChange`]s, in `AppState` (one API
/// instance in the MVP, so in-process is enough). Cheap to clone.
#[derive(Debug, Clone)]
pub struct MembershipEvents(broadcast::Sender<MembershipChange>);

impl MembershipEvents {
    /// Messages a slow subscriber may fall behind by before it sees
    /// `Lagged` (and must assume the worst).
    const CAPACITY: usize = 1024;

    pub fn new() -> Self {
        Self(broadcast::channel(Self::CAPACITY).0)
    }

    /// Call after the change is committed, never before: a socket closed for
    /// a change that then rolls back would just reconnect, but one left open
    /// for a committed change keeps rights it lost.
    pub fn publish(&self, change: MembershipChange) {
        tracing::debug!(?change, "membership changed");
        // An error only means nobody is listening: no socket is open.
        let _ = self.0.send(change);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MembershipChange> {
        self.0.subscribe()
    }
}

impl Default for MembershipEvents {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribers_hear_committed_changes() {
        let events = MembershipEvents::new();
        let mut socket = events.subscribe();
        let (org, user, other) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        events.publish(MembershipChange::Removed {
            org_id: org,
            user_id: user,
        });
        let change = socket.recv().await.unwrap();
        assert!(change.concerns(org, user));
        assert!(!change.concerns(org, other));
        assert!(!change.concerns(other, user));
    }

    #[test]
    fn publishing_without_listeners_is_fine() {
        MembershipEvents::new().publish(MembershipChange::RoleChanged {
            org_id: Uuid::from_u128(1),
            user_id: Uuid::from_u128(2),
            role: Role::Reviewer,
        });
    }
}
