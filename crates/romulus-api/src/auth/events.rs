//! Sessions that end while long-lived connections still use them.
//!
//! A websocket authenticated by a session cookie outlives the request that
//! opened it. Signing out publishes the session here (by the sha256 its row
//! is keyed by), and the collab server closes the sockets opened with it;
//! sessions that expire are caught by the sockets checking theirs now and
//! then (`session::is_live`).

use tokio::sync::broadcast;

/// A broadcast channel of ended sessions' `id_hash`es, in `AppState` (one
/// API instance in the MVP, so in-process is enough). Cheap to clone.
#[derive(Debug, Clone)]
pub struct SessionEvents(broadcast::Sender<Vec<u8>>);

impl SessionEvents {
    /// Ended sessions a slow subscriber may fall behind by before it sees
    /// `Lagged` (and checks its own session instead).
    const CAPACITY: usize = 256;

    pub fn new() -> Self {
        Self(broadcast::channel(Self::CAPACITY).0)
    }

    /// Call once the session row is gone.
    pub fn ended(&self, id_hash: Vec<u8>) {
        // An error only means nobody is listening.
        let _ = self.0.send(id_hash);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.0.subscribe()
    }
}

impl Default for SessionEvents {
    fn default() -> Self {
        Self::new()
    }
}
