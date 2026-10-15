//! The collab server: one in-memory Y.Doc (yrs) per open draft, the y-sync
//! protocol over a websocket, persistence to `draft_updates` /
//! `draft_snapshots`, awareness stamped with the authenticated user, and
//! compile-on-type.
//!
//! Endpoint: `GET /orgs/{org}/drafts/{id}/ws` [member], the websocket
//! upgrade. It checks `Origin` (`csrf::SameOrigin`: the CSRF middleware only
//! guards unsafe methods, and browsers send cookies with cross-site
//! upgrades) and the membership, then the [`Hub`] takes it from there:
//!
//! - each draft someone has open is a room ([`room`]): its doc, the
//!   connections in it, its writes and compiles, run by one task;
//! - reviewers' document updates are dropped server-side, their awareness
//!   (cursor, pointer) goes through; the server stamps every awareness
//!   state with the user's id, name and color ([`awareness`]);
//! - a role change or removal closes that member's sockets (the client
//!   reconnects with the rights it has now, or is refused);
//! - compile results, status changes and renames are [`protocol::ServerEvent`]s,
//!   a message type only the server sends.
//!
//! **One API instance.** Docs live in this process's memory and each one
//! has a single writer (see [`store`]); two instances would each hold their
//! own copy of a draft. Scaling out means routing every connection of a
//! draft to one instance (docs/MVP.md "Roles").
//!
//! Shutting down ([`Hub::shutdown`]) closes every socket with 1012 (the
//! clients reconnect after the restart) and writes every room first.

pub mod awareness;
pub mod compile;
pub mod doc;
pub mod protocol;
pub mod room;
mod socket;
pub mod store;

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::{Notify, mpsc, oneshot};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use self::{
    awareness::Peer,
    compile::DraftCompiler,
    room::{Command, ConnId, Content, FreezeError, Outgoing, Settings, Shared},
};
use crate::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(socket::connect))
}

/// Someone who has a draft open right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Present {
    pub user_id: Uuid,
    pub name: String,
    /// They may edit (developer or admin); otherwise they watch (reviewer).
    pub editing: bool,
}

/// Every loaded draft. Cheap to clone; one per API process, in `AppState`.
#[derive(Clone)]
pub struct Hub(Arc<HubInner>);

pub struct HubInner {
    db: PgPool,
    compiler: DraftCompiler,
    settings: Settings,
    rooms: Mutex<HashMap<Uuid, Entry>>,
    next_conn: AtomicU64,
    next_room: AtomicU64,
    closing: AtomicBool,
    /// Notified each time a room leaves.
    left: Notify,
}

struct Entry {
    generation: u64,
    commands: mpsc::UnboundedSender<Command>,
    shared: Arc<Shared>,
}

/// Why the hub could not do something with a draft.
#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum HubError {
    /// The draft could not be loaded (gone, or the database failing).
    #[error("the draft could not be loaded")]
    Unavailable,
    #[error("the server is shutting down")]
    Closing,
}

/// A socket's place in a room.
pub struct Connection {
    pub id: ConnId,
    commands: mpsc::UnboundedSender<Command>,
}

impl Connection {
    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.send(Command::Leave { conn: self.id });
    }
}

impl Hub {
    pub fn new(db: PgPool, compiler: DraftCompiler) -> Self {
        Self::with_settings(db, compiler, Settings::default())
    }

    pub fn with_settings(db: PgPool, compiler: DraftCompiler, settings: Settings) -> Self {
        Self(Arc::new(HubInner {
            db,
            compiler,
            settings,
            rooms: Mutex::new(HashMap::new()),
            next_conn: AtomicU64::new(1),
            next_room: AtomicU64::new(1),
            closing: AtomicBool::new(false),
            left: Notify::new(),
        }))
    }

    pub fn compiler(&self) -> &DraftCompiler {
        &self.0.compiler
    }

    /// Joins a socket to its draft's room (loading it if needed). The room
    /// sends the first frames (welcome, sync step 1, awareness, the last
    /// compile) to `out` before this returns.
    pub async fn join(
        &self,
        draft: Uuid,
        peer: Peer,
        out: mpsc::Sender<Outgoing>,
    ) -> Result<Connection, HubError> {
        let id = self.0.next_conn.fetch_add(1, Ordering::Relaxed);
        let (joined, wait) = oneshot::channel();
        let commands = self.0.send(
            draft,
            Command::Join {
                conn: id,
                peer,
                out,
                joined,
            },
        )?;
        wait.await.map_err(|_| HubError::Unavailable)?;
        Ok(Connection { id, commands })
    }

    /// The draft's content now (loading it if needed).
    pub async fn content(&self, draft: Uuid) -> Result<Content, HubError> {
        let (reply, wait) = oneshot::channel();
        self.0.send(draft, Command::Content { reply })?;
        wait.await.map_err(|_| HubError::Unavailable)
    }

    /// Freezes the draft for publishing: everyone sees it, every update is
    /// dropped from now on. Answers the frozen content. Follow with
    /// [`Hub::published`] or [`Hub::unfreeze`].
    pub async fn freeze(
        &self,
        draft: Uuid,
        by: &str,
    ) -> Result<Result<Content, FreezeError>, HubError> {
        let (reply, wait) = oneshot::channel();
        self.0.send(
            draft,
            Command::Freeze {
                by: by.to_owned(),
                reply,
            },
        )?;
        wait.await.map_err(|_| HubError::Unavailable)
    }

    /// Publishing failed: the draft is editable again.
    pub fn unfreeze(&self, draft: Uuid) {
        self.0.tell(draft, Command::Unfreeze);
    }

    pub fn published(&self, draft: Uuid, version: i32, by: &str) {
        self.0.tell(
            draft,
            Command::Published {
                version,
                by: by.to_owned(),
            },
        );
    }

    pub fn renamed(&self, draft: Uuid, name: &str) {
        self.0.tell(
            draft,
            Command::Renamed {
                name: name.to_owned(),
            },
        );
    }

    pub fn deleted(&self, draft: Uuid, by: &str) {
        self.0.tell(draft, Command::Deleted { by: by.to_owned() });
    }

    /// A version was created: the drafts of that database open now hear it.
    pub fn version_created(&self, database_id: Uuid, number: i32) {
        let rooms = self.0.rooms();
        for entry in rooms.values() {
            if entry.shared.database_id.get() == Some(&database_id) {
                let _ = entry.commands.send(Command::VersionCreated { number });
            }
        }
    }

    /// Who has each of these drafts open.
    pub fn present(&self, drafts: &[Uuid]) -> HashMap<Uuid, Vec<Present>> {
        let rooms = self.0.rooms();
        drafts
            .iter()
            .filter_map(|id| {
                let entry = rooms.get(id)?;
                let present = entry
                    .shared
                    .present
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                Some((*id, present))
            })
            .collect()
    }

    /// Closes every socket and writes every room; refuses new joins.
    pub async fn shutdown(&self) {
        self.0.closing.store(true, Ordering::SeqCst);
        loop {
            let left = self.0.left.notified();
            {
                let rooms = self.0.rooms();
                if rooms.is_empty() {
                    return;
                }
                for entry in rooms.values() {
                    let _ = entry.commands.send(Command::Shutdown);
                }
            }
            if tokio::time::timeout(Duration::from_secs(10), left)
                .await
                .is_err()
            {
                tracing::error!("collab: rooms did not close in time");
                return;
            }
        }
    }
}

impl HubInner {
    fn rooms(&self) -> MutexGuard<'_, HashMap<Uuid, Entry>> {
        // Plain data, no await under the lock: poisoning cannot leave it torn.
        self.rooms.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sends a command to the draft's room, loading it first if needed.
    /// Under the rooms lock, so a room that decides to unload sees it (see
    /// [`HubInner::unload`]).
    fn send(
        self: &Arc<Self>,
        draft: Uuid,
        command: Command,
    ) -> Result<mpsc::UnboundedSender<Command>, HubError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(HubError::Closing);
        }
        let mut rooms = self.rooms();
        let entry = rooms.entry(draft).or_insert_with(|| {
            let generation = self.next_room.fetch_add(1, Ordering::Relaxed);
            let (commands, receiver) = mpsc::unbounded_channel();
            let shared = Arc::new(Shared::default());
            tokio::spawn(room::run(
                Arc::clone(self),
                draft,
                generation,
                Arc::clone(&shared),
                receiver,
            ));
            Entry {
                generation,
                commands,
                shared,
            }
        });
        entry
            .commands
            .send(command)
            .map_err(|_| HubError::Unavailable)?;
        Ok(entry.commands.clone())
    }

    /// Sends a command to the draft's room if it is loaded (a draft nobody
    /// has open needs no news).
    fn tell(&self, draft: Uuid, command: Command) {
        if let Some(entry) = self.rooms().get(&draft) {
            let _ = entry.commands.send(command);
        }
    }

    /// Called by a room that has written everything and has nobody in it:
    /// leaves the hub unless a command arrived meanwhile, which it returns
    /// (the room then stays). Commands are only sent under the lock taken
    /// here, so none can arrive once the room has left.
    fn unload(
        &self,
        draft: Uuid,
        generation: u64,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> Option<Command> {
        let mut rooms = self.rooms();
        if let Ok(command) = commands.try_recv() {
            return Some(command);
        }
        if rooms
            .get(&draft)
            .is_some_and(|e| e.generation == generation)
        {
            rooms.remove(&draft);
        }
        drop(rooms);
        self.left.notify_waiters();
        None
    }

    /// A room that stopped (failed to load, or shut down).
    fn remove(&self, draft: Uuid, generation: u64) {
        let mut rooms = self.rooms();
        if rooms
            .get(&draft)
            .is_some_and(|e| e.generation == generation)
        {
            rooms.remove(&draft);
        }
        drop(rooms);
        self.left.notify_waiters();
    }
}
