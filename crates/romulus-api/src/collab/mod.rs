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
//!   a message type only the server sends;
//! - who edited the SQL, and the approvals that count, are the room's to
//!   say ([`review`]).
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
pub mod patch;
pub mod protocol;
pub mod review;
pub mod room;
mod socket;
pub mod store;

use std::{
    collections::{HashMap, HashSet},
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
    compile::SqlCompiler,
    protocol::{ReviewRequest, ServerEvent},
    review::{Approval, ApprovalError},
    room::{
        Command, ConnId, Content, DATA_QUEUE, Data, FreezeError, FreezeFor, Incoming, Outbox,
        ReviewChange, RewriteError, Settings, Shared, UpdateTicket,
    },
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
    compiler: SqlCompiler,
    settings: Settings,
    rooms: Mutex<Rooms>,
    next_conn: AtomicU64,
    next_room: AtomicU64,
    next_ticket: AtomicU64,
    closing: AtomicBool,
    /// Notified each time a room leaves.
    left: Notify,
}

#[derive(Default)]
struct Rooms {
    entries: HashMap<Uuid, Entry>,
    /// Databases deleted while this process ran: a room of theirs still
    /// loading when the deletion was announced learns it when it attaches
    /// ([`HubInner::attach`]), under the same lock.
    deleted_databases: HashSet<Uuid>,
}

struct Entry {
    generation: u64,
    /// Everything but document updates: never bounded, so a socket leaving
    /// is never lost.
    commands: mpsc::UnboundedSender<Command>,
    /// Document updates and awareness from sockets: bounded (backpressure).
    data: mpsc::Sender<Data>,
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

/// Why a draft cannot be deleted now: it is being published or updated.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("the draft is being published or updated")]
pub struct Publishing;

/// A socket's place in a room.
pub struct Connection {
    pub id: ConnId,
    commands: mpsc::UnboundedSender<Command>,
    data: mpsc::Sender<Data>,
}

impl Connection {
    /// Hands decoded messages to the room, waiting while it is busy (the
    /// socket then stops reading: backpressure). False once the room is gone.
    pub async fn incoming(&self, messages: Vec<Incoming>) -> bool {
        self.data
            .send(Data {
                conn: self.id,
                messages,
            })
            .await
            .is_ok()
    }

    /// The socket sent something that is not y-sync.
    pub fn malformed(&self) {
        let _ = self.commands.send(Command::Malformed { conn: self.id });
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Leave { conn: self.id });
    }
}

impl Hub {
    pub fn new(db: PgPool, compiler: SqlCompiler) -> Self {
        Self::with_settings(db, compiler, Settings::default())
    }

    pub fn with_settings(db: PgPool, compiler: SqlCompiler, settings: Settings) -> Self {
        Self(Arc::new(HubInner {
            db,
            compiler,
            settings,
            rooms: Mutex::new(Rooms::default()),
            next_conn: AtomicU64::new(1),
            next_room: AtomicU64::new(1),
            next_ticket: AtomicU64::new(1),
            closing: AtomicBool::new(false),
            left: Notify::new(),
        }))
    }

    pub fn compiler(&self) -> &SqlCompiler {
        &self.0.compiler
    }

    pub fn settings(&self) -> &Settings {
        &self.0.settings
    }

    /// Joins a socket to its draft's room (loading it if needed). The room
    /// sends the first frames (welcome, sync step 1, awareness, the last
    /// compile) to `out` before this returns.
    pub async fn join(&self, draft: Uuid, peer: Peer, out: Outbox) -> Result<Connection, HubError> {
        let id = self.0.next_conn.fetch_add(1, Ordering::Relaxed);
        let (joined, wait) = oneshot::channel();
        let (commands, data) = self.0.send(
            draft,
            Command::Join {
                conn: id,
                peer,
                out,
                joined,
            },
        )?;
        wait.await.map_err(|_| HubError::Unavailable)?;
        Ok(Connection { id, commands, data })
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
        self.freeze_for(draft, by, FreezeFor::Publish).await
    }

    /// [`Hub::freeze`] for "Update draft": everyone sees "updating". With the
    /// content, the ticket that lets this update, and nothing else,
    /// rewrite the SQL meanwhile ([`Hub::rewrite_update`]). Then
    /// [`Hub::merge_layout`] and [`Hub::unfreeze`].
    pub async fn freeze_to_update(
        &self,
        draft: Uuid,
        by: &str,
    ) -> Result<Result<(Content, UpdateTicket), FreezeError>, HubError> {
        let ticket = UpdateTicket(self.0.next_ticket.fetch_add(1, Ordering::Relaxed));
        Ok(self
            .freeze_for(draft, by, FreezeFor::Update(ticket))
            .await?
            .map(|content| (content, ticket)))
    }

    async fn freeze_for(
        &self,
        draft: Uuid,
        by: &str,
        purpose: FreezeFor,
    ) -> Result<Result<Content, FreezeError>, HubError> {
        let (reply, wait) = oneshot::channel();
        self.0.send(
            draft,
            Command::Freeze {
                by: by.to_owned(),
                purpose,
                reply,
            },
        )?;
        wait.await.map_err(|_| HubError::Unavailable)
    }

    /// Replaces the draft's SQL by `changes` ([`patch::changes`] from the
    /// text whose SHA-256 is `from` to the new one), in one transaction that
    /// also notes the rewrite for `reason` (`doc::Roots::note_rewrite`:
    /// clients drop their undo history), if nobody edited it since; answers
    /// whether anything changed. On [`RewriteError::Changed`], read the
    /// content again and recompute.
    pub async fn rewrite(
        &self,
        draft: Uuid,
        from: String,
        changes: Vec<patch::Change>,
        reason: &'static str,
    ) -> Result<Result<bool, RewriteError>, HubError> {
        let (reply, wait) = oneshot::channel();
        self.0.send(
            draft,
            Command::Rewrite {
                from,
                changes,
                reason,
                ticket: None,
                merged: None,
                reply,
            },
        )?;
        wait.await.map_err(|_| HubError::Unavailable)
    }

    /// [`Hub::rewrite`] by the update holding `ticket`, while the draft is
    /// frozen for it ([`Hub::freeze_to_update`]); the doc notes, in the same
    /// transaction, that it merged `version`.
    pub async fn rewrite_update(
        &self,
        draft: Uuid,
        ticket: UpdateTicket,
        version: i32,
        from: String,
        changes: Vec<patch::Change>,
    ) -> Result<Result<bool, RewriteError>, HubError> {
        let (reply, wait) = oneshot::channel();
        self.0.send(
            draft,
            Command::Rewrite {
                from,
                changes,
                reason: "update",
                ticket: Some(ticket),
                merged: Some(version),
                reply,
            },
        )?;
        wait.await.map_err(|_| HubError::Unavailable)
    }

    /// Applies "Update draft"'s layout and group changes (after its
    /// [`Hub::rewrite_update`] of the SQL) for the update holding `ticket`,
    /// and answers once everything the room changed is written: the draft's
    /// base may move then.
    pub async fn merge_layout(
        &self,
        draft: Uuid,
        ticket: UpdateTicket,
        layout: Vec<(String, Option<crate::versions::layout::TablePosition>)>,
        groups: Vec<(String, Option<crate::versions::layout::TableGroup>)>,
    ) -> Result<Result<(), RewriteError>, HubError> {
        let (reply, wait) = oneshot::channel();
        self.0.send(
            draft,
            Command::MergeLayout {
                ticket,
                layout,
                groups,
                reply,
            },
        )?;
        wait.await.map_err(|_| HubError::Unavailable)
    }

    /// Publishing failed: the draft is editable again.
    pub fn unfreeze(&self, draft: Uuid) {
        self.0.tell(draft, Command::Unfreeze);
    }

    /// "Update draft" moved the draft's base to `version`: everyone hears it
    /// (before the unfreeze).
    pub fn updated(&self, draft: Uuid, version: i32, by: &str, conflicts: u32) {
        self.0.tell(
            draft,
            Command::Updated {
                version,
                by: by.to_owned(),
                conflicts,
            },
        );
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

    /// About to delete the draft: refused while it is being published;
    /// otherwise its room (if loaded) stops taking edits and lets everyone
    /// go. The database row is the caller's to update.
    pub async fn delete(&self, draft: Uuid, by: &str) -> Result<(), Publishing> {
        let (reply, wait) = oneshot::channel();
        let sent = self.0.tell(
            draft,
            Command::Delete {
                by: by.to_owned(),
                reply,
            },
        );
        if !sent {
            return Ok(());
        }
        // A room that went away meanwhile is nothing to wait for.
        wait.await.unwrap_or(Ok(()))
    }

    /// A version was created: the drafts of that database open now hear it.
    pub fn version_created(&self, database_id: Uuid, number: i32) {
        let rooms = self.0.rooms();
        for entry in rooms.entries.values() {
            if entry.shared.database_id.get() == Some(&database_id) {
                let _ = entry.commands.send(Command::VersionCreated { number });
            }
        }
    }

    /// A version was removed: the drafts of that database open now hear it
    /// (those based on it now need "Update draft").
    pub fn version_removed(&self, database_id: Uuid, number: i32, by: &str) {
        self.to_database(
            database_id,
            &ServerEvent::VersionRemoved {
                number,
                by: by.to_owned(),
            },
        );
    }

    /// A removed version was restored.
    pub fn version_restored(&self, database_id: Uuid, number: i32, by: &str) {
        self.to_database(
            database_id,
            &ServerEvent::VersionRestored {
                number,
                by: by.to_owned(),
            },
        );
    }

    /// `event` to every draft of the database open now.
    fn to_database(&self, database_id: Uuid, event: &ServerEvent) {
        let rooms = self.0.rooms();
        for entry in rooms.entries.values() {
            if entry.shared.database_id.get() == Some(&database_id) {
                let _ = entry.commands.send(Command::Event(event.clone()));
            }
        }
    }

    /// Whether `user_id` may approve the draft's SQL as it is now, the text
    /// hashing to `sha256` (loading the draft if needed). The approval is
    /// then the caller's to write, and to announce ([`Hub::approved`]).
    pub async fn check_approval(
        &self,
        draft: Uuid,
        user_id: Uuid,
        sha256: String,
    ) -> Result<Result<(), ApprovalError>, HubError> {
        let (reply, wait) = oneshot::channel();
        self.0.send(
            draft,
            Command::CheckApproval {
                user_id,
                sha256,
                reply,
            },
        )?;
        wait.await.map_err(|_| HubError::Unavailable)
    }

    /// An approval was written: everyone in the draft hears it.
    pub fn approved(&self, draft: Uuid, approval: Approval) {
        self.0
            .tell(draft, Command::Review(ReviewChange::Approved(approval)));
    }

    pub fn withdrawn(&self, draft: Uuid, user_id: Uuid) {
        self.0
            .tell(draft, Command::Review(ReviewChange::Withdrawn(user_id)));
    }

    pub fn review_requested(&self, draft: Uuid, requested: Option<ReviewRequest>) {
        self.0
            .tell(draft, Command::Review(ReviewChange::Requested(requested)));
    }

    /// The organization changed how many approvals publishing needs: its
    /// drafts open now hear it.
    pub fn approvals_required(&self, org_id: Uuid, required: i32) {
        let rooms = self.0.rooms();
        for entry in rooms.entries.values() {
            if entry.shared.org_id.get() == Some(&org_id) {
                let _ = entry
                    .commands
                    .send(Command::Review(ReviewChange::Required(required)));
            }
        }
    }

    /// How many approvals count now, for each of these drafts whose room is
    /// loaded (the others' are the database's to say).
    pub fn approvals(&self, drafts: &[Uuid]) -> HashMap<Uuid, usize> {
        let rooms = self.0.rooms();
        drafts
            .iter()
            .filter_map(|id| {
                let entry = rooms.entries.get(id)?;
                let count = *entry
                    .shared
                    .approvals
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                Some((*id, count?))
            })
            .collect()
    }

    /// The database was deleted: its drafts' rooms close as deleted drafts
    /// do, those loading now included.
    pub fn database_deleted(&self, database_id: Uuid, by: &str) {
        let mut rooms = self.0.rooms();
        rooms.deleted_databases.insert(database_id);
        for entry in rooms.entries.values() {
            if entry.shared.database_id.get() == Some(&database_id) {
                let _ = entry.commands.send(Command::Deleted { by: by.to_owned() });
            }
        }
    }

    /// Who has each of these drafts open.
    pub fn present(&self, drafts: &[Uuid]) -> HashMap<Uuid, Vec<Present>> {
        let rooms = self.0.rooms();
        drafts
            .iter()
            .filter_map(|id| {
                let entry = rooms.entries.get(id)?;
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
                if rooms.entries.is_empty() {
                    return;
                }
                for entry in rooms.entries.values() {
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
    fn rooms(&self) -> MutexGuard<'_, Rooms> {
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
    ) -> Result<(mpsc::UnboundedSender<Command>, mpsc::Sender<Data>), HubError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(HubError::Closing);
        }
        let mut rooms = self.rooms();
        let entry = rooms.entries.entry(draft).or_insert_with(|| {
            let generation = self.next_room.fetch_add(1, Ordering::Relaxed);
            let (commands, receiver) = mpsc::unbounded_channel();
            let (data, data_receiver) = mpsc::channel(DATA_QUEUE);
            let shared = Arc::new(Shared::default());
            tokio::spawn(room::run(
                Arc::clone(self),
                draft,
                generation,
                Arc::clone(&shared),
                receiver,
                data_receiver,
            ));
            Entry {
                generation,
                commands,
                data,
                shared,
            }
        });
        entry
            .commands
            .send(command)
            .map_err(|_| HubError::Unavailable)?;
        Ok((entry.commands.clone(), entry.data.clone()))
    }

    /// Sends a command to the draft's room if it is loaded (a draft nobody
    /// has open needs no news). Whether it was.
    fn tell(&self, draft: Uuid, command: Command) -> bool {
        self.rooms()
            .entries
            .get(&draft)
            .is_some_and(|entry| entry.commands.send(command).is_ok())
    }

    /// A room that loaded tells which database it belongs to; whether that
    /// database was deleted meanwhile (see [`Rooms::deleted_databases`]).
    pub(crate) fn attach(&self, database_id: Uuid, shared: &Shared) -> bool {
        let rooms = self.rooms();
        let _ = shared.database_id.set(database_id);
        rooms.deleted_databases.contains(&database_id)
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
            .entries
            .get(&draft)
            .is_some_and(|e| e.generation == generation)
        {
            rooms.entries.remove(&draft);
        }
        drop(rooms);
        self.left.notify_waiters();
        None
    }

    /// A room that stopped (failed to load, or shut down).
    fn remove(&self, draft: Uuid, generation: u64) {
        let mut rooms = self.rooms();
        if rooms
            .entries
            .get(&draft)
            .is_some_and(|e| e.generation == generation)
        {
            rooms.entries.remove(&draft);
        }
        drop(rooms);
        self.left.notify_waiters();
    }
}
