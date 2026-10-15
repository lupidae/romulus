//! One loaded draft: its Y.Doc, the connections in it, and the work around
//! them, run by a single task (an actor) so nothing needs a lock.
//!
//! - **Sync**: y-sync with each connection. Document updates from people
//!   who may not edit (reviewers), or while the draft is not open, are
//!   dropped; their awareness still goes through.
//! - **Awareness**: each state is stamped with its connection's identity
//!   (see `awareness`), and a connection only speaks for the Yjs client ids
//!   it announced first.
//! - **Persistence**: applied updates are batched, merged and appended to
//!   `draft_updates`; after [`Settings::compact_after`] rows, or when the
//!   room unloads, the doc's state replaces the snapshot.
//! - **Compile on type**: a change to `sql` schedules a compile
//!   ([`Settings::debounce`] after the last change, at most
//!   [`Settings::max_wait`] after the first); its result goes to everyone
//!   and to `draft_compiles`. A rename typed in the text moves the table's
//!   layout and group entries (docs/MVP.md "Renames").
//! - **Rewrites**: the server replacing the SQL (Tidy, Update draft's
//!   merge) sends the changes it computed against one text
//!   ([`super::patch`]); the room applies them in one transaction if the
//!   text is still that one (and the room open, or frozen for an update),
//!   and refuses otherwise (the caller computes again). The same
//!   transaction notes the rewrite in `meta` (`Roots::note_rewrite`): every
//!   client drops its undo history, which could otherwise delete characters
//!   the rewrite reused.
//! - **Graph edits markers**: two editors making their first graph edit at
//!   once each append `-- edits from the graph`; before compiling, the room
//!   removes all but the first (a server edit).
//! - **Publishing** and **updating** freeze the room (every update dropped)
//!   until they are done or fail; then every connection is asked to sync
//!   again, which brings back what was dropped meanwhile. A frozen room
//!   never unloads.
//! - **Unloading**: [`Settings::idle_unload`] after the last connection
//!   leaves, the room writes everything and leaves the hub.
//! - **Bounds**: each socket's queue is bounded in bytes
//!   ([`Settings::max_queued_bytes`]: a socket that falls behind is let go
//!   and syncs again), document updates reach the room through a bounded
//!   channel (a socket flooding it waits, and stops reading), and the doc
//!   has a size cap ([`Settings::max_doc_bytes`]: an update past it closes
//!   its socket with 4413).

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::body::Bytes;
use serde_json::Value;
use tokio::{
    sync::{mpsc, oneshot},
    time::{Instant, sleep_until},
};
use uuid::Uuid;
use yrs::{
    Doc, ReadTxn, StateVector, Transact, TransactionMut, Update,
    sync::{
        Message, SyncMessage,
        awareness::{AwarenessUpdate, AwarenessUpdateEntry},
    },
    updates::decoder::Decode,
};

use super::{
    HubInner, Present,
    awareness::{Peer, sanitize},
    compile::{self, CompileState, Compiled, Rejected, SqlCompiler, sha256_hex},
    doc::{self, Roots},
    patch,
    protocol::{self, LiveStatus, ServerEvent},
    store::{self, CompileRow, Job, Loaded, Writer},
};
use crate::{
    domain::DraftStatus,
    versions::layout::{TableGroup, TablePosition, VersionLayout},
};

/// Timings and thresholds; [`Settings::default`] in production, shorter in
/// tests.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Compile this long after the last change to the SQL…
    pub debounce: Duration,
    /// …but no later than this after the first one, however fast people type.
    pub max_wait: Duration,
    /// Applied updates are written at most this long after they arrive.
    pub flush_every: Duration,
    /// Rows appended since the last snapshot before the doc is compacted.
    pub compact_after: usize,
    /// How long an empty room stays loaded (a page reload comes back).
    pub idle_unload: Duration,
    /// When the compiler is unavailable, try again after this long.
    pub retry_unavailable: Duration,
    /// A doc grows no larger (its encoded state, roughly).
    pub max_doc_bytes: usize,
    /// Frames queued for one socket, in bytes, before it counts as too slow.
    pub max_queued_bytes: usize,
    /// Sockets are pinged this often; one that answers nothing for two and a
    /// half pings is closed (4002: the client reconnects if it is there).
    pub ping_every: Duration,
    /// Sockets check their session this often (sign-outs are immediate).
    pub session_check: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(300),
            max_wait: Duration::from_secs(1),
            flush_every: Duration::from_millis(100),
            compact_after: 200,
            idle_unload: Duration::from_secs(10),
            retry_unavailable: Duration::from_secs(10),
            max_doc_bytes: 16 * 1024 * 1024,
            max_queued_bytes: 16 * 1024 * 1024,
            ping_every: Duration::from_secs(25),
            session_check: Duration::from_secs(60),
        }
    }
}

/// Document updates queued for one room, from all its sockets: past this,
/// sockets wait (and stop reading theirs).
pub const DATA_QUEUE: usize = 256;

/// A connection's number within the hub.
pub type ConnId = u64;

/// Most Yjs client ids one connection may speak for (one doc, and the odd
/// new id Yjs picks after a clash).
const CLIENTS_PER_CONNECTION: usize = 4;

/// What the socket task hands the room, already decoded (decoding runs
/// there, so a malformed frame only costs its own connection).
pub enum Incoming {
    Step1(StateVector),
    /// A document update, raw (to relay and store) and decoded (to apply).
    Update(Bytes, Update),
    Awareness(AwarenessUpdate),
    AwarenessQuery,
}

impl Incoming {
    /// The room's view of a y-sync message; `None` for messages it ignores
    /// (auth, custom messages: only the server sends those) or an update
    /// that does not decode.
    pub fn from_message(message: Message) -> Option<Result<Self, ()>> {
        Some(match message {
            Message::Sync(SyncMessage::SyncStep1(sv)) => Ok(Self::Step1(sv)),
            Message::Sync(SyncMessage::SyncStep2(bytes) | SyncMessage::Update(bytes)) => {
                Update::decode_v1(&bytes)
                    .map(|update| Self::Update(bytes.into(), update))
                    .map_err(|_| ())
            }
            Message::Awareness(update) => Ok(Self::Awareness(update)),
            Message::AwarenessQuery => Ok(Self::AwarenessQuery),
            Message::Auth(_) | Message::Custom(..) => return None,
        })
    }
}

/// What the room sends a socket task.
#[derive(Debug, Clone)]
pub enum Outgoing {
    Frame(Bytes),
    /// Close the websocket with this code and reason.
    Close(u16, &'static str),
}

/// The room's end of a socket's queue: bounded by the bytes waiting in it.
#[derive(Clone)]
pub struct Outbox {
    tx: mpsc::UnboundedSender<Outgoing>,
    queued: Arc<AtomicUsize>,
    limit: usize,
}

/// The socket task's end.
pub struct Inbox {
    rx: mpsc::UnboundedReceiver<Outgoing>,
    queued: Arc<AtomicUsize>,
}

/// A socket's queue, holding at most `limit` bytes of frames (one larger
/// frame always fits in an empty queue: a big doc's first sync).
pub fn queue(limit: usize) -> (Outbox, Inbox) {
    let (tx, rx) = mpsc::unbounded_channel();
    let queued = Arc::new(AtomicUsize::new(0));
    (
        Outbox {
            tx,
            queued: Arc::clone(&queued),
            limit,
        },
        Inbox { rx, queued },
    )
}

impl Outbox {
    /// Queues a frame; false when the socket is too far behind, or gone.
    pub fn frame(&self, frame: Bytes) -> bool {
        let len = frame.len();
        let before = self.queued.fetch_add(len, Ordering::SeqCst);
        if before > 0 && before + len > self.limit {
            self.queued.fetch_sub(len, Ordering::SeqCst);
            return false;
        }
        self.tx.send(Outgoing::Frame(frame)).is_ok()
    }

    /// Asks the socket to close (whatever is queued goes first).
    pub fn close(&self, code: u16, reason: &'static str) {
        let _ = self.tx.send(Outgoing::Close(code, reason));
    }
}

impl Inbox {
    pub async fn recv(&mut self) -> Option<Outgoing> {
        let out = self.rx.recv().await?;
        if let Outgoing::Frame(frame) = &out {
            self.queued.fetch_sub(frame.len(), Ordering::SeqCst);
        }
        Some(out)
    }
}

/// Websocket close codes the web app understands.
pub mod close {
    /// The server is restarting: reconnect.
    pub const RESTART: u16 = 1012;
    /// The connection sent something that is not y-sync.
    pub const PROTOCOL: u16 = 1002;
    /// The connection could not keep up: reconnect (and sync again).
    pub const LAGGING: u16 = 4000;
    /// The member's role changed or they were removed: reconnect, to get
    /// the rights they have now (or be refused).
    pub const MEMBERSHIP: u16 = 4001;
    /// The client answered no ping for a while: reconnect if still there.
    pub const SILENT: u16 = 4002;
    /// The session ended (signed out or expired): sign in again. Final
    /// (4400–4499: y-websocket does not reconnect).
    pub const SESSION: u16 = 4401;
    /// The draft was deleted: do not reconnect.
    pub const DELETED: u16 = 4404;
    /// An update would make the draft larger than it may be: the update was
    /// not applied; reload.
    pub const TOO_LARGE: u16 = 4413;
    /// The draft could not be loaded.
    pub const UNAVAILABLE: u16 = 1011;
}

/// The draft's content at one moment.
#[derive(Debug, Clone)]
pub struct Content {
    pub sql: String,
    pub layout: VersionLayout,
    /// The model of exactly this SQL, when the last compile was of it and
    /// succeeded (publishing then needs no compile of its own).
    pub model: Option<Arc<Value>>,
}

#[derive(Debug, thiserror::Error)]
pub enum FreezeError {
    #[error("the draft is not open ({0:?})")]
    NotOpen(LiveStatus),
}

/// Why the server could not rewrite the SQL.
#[derive(Debug, thiserror::Error)]
pub enum RewriteError {
    #[error("the draft is not open ({0:?})")]
    NotOpen(LiveStatus),
    /// Someone edited it since the changes were computed: compute again.
    #[error("the SQL changed meanwhile")]
    Changed,
}

/// Why a draft is frozen: what everyone sees meanwhile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeFor {
    Publish,
    Update,
}

pub enum Command {
    Join {
        conn: ConnId,
        peer: Peer,
        out: Outbox,
        joined: oneshot::Sender<()>,
    },
    /// The connection sent something malformed.
    Malformed {
        conn: ConnId,
    },
    Leave {
        conn: ConnId,
    },
    Content {
        reply: oneshot::Sender<Content>,
    },
    Freeze {
        by: String,
        purpose: FreezeFor,
        reply: oneshot::Sender<Result<Content, FreezeError>>,
    },
    /// "Update draft"'s layout and group changes; answered once everything
    /// the room changed is written (handled by the loop).
    MergeLayout {
        layout: Vec<(String, Option<TablePosition>)>,
        groups: Vec<(String, Option<TableGroup>)>,
        reply: oneshot::Sender<()>,
    },
    Unfreeze,
    Published {
        version: i32,
        by: String,
    },
    Renamed {
        name: String,
    },
    Deleted {
        by: String,
    },
    /// About to be deleted: refused while publishing, otherwise as `Deleted`.
    Delete {
        by: String,
        reply: oneshot::Sender<Result<(), super::Publishing>>,
    },
    VersionCreated {
        number: i32,
    },
    /// Replace the SQL with `changes`, computed against the text whose
    /// SHA-256 is `from`, for `reason` (`tidy`…): whether anything changed.
    Rewrite {
        from: String,
        changes: Vec<patch::Change>,
        reason: &'static str,
        reply: oneshot::Sender<Result<bool, RewriteError>>,
    },
    /// The draft was updated to a newer version (its base moved).
    Updated {
        version: i32,
        by: String,
        conflicts: u32,
    },
    Shutdown,
}

/// Messages from one socket, already decoded: the data channel's items.
pub struct Data {
    pub conn: ConnId,
    pub messages: Vec<Incoming>,
}

/// What the hub knows of a room without asking it.
#[derive(Default)]
pub struct Shared {
    pub database_id: std::sync::OnceLock<Uuid>,
    pub present: std::sync::Mutex<Vec<Present>>,
}

struct Conn {
    peer: Peer,
    stamp: Value,
    out: Outbox,
    /// Yjs client ids this connection speaks for.
    clients: HashSet<u64>,
}

struct ClientState {
    clock: u32,
    /// Sanitized JSON, or `null` once the client said it left.
    json: Arc<str>,
    conn: ConnId,
}

/// A compile, finished.
struct CompileDone {
    /// Of the SQL compiled.
    sha256: String,
    /// Where it creates each relation (`compile::created_at`).
    tables: HashMap<String, usize>,
    result: Result<Compiled, Rejected>,
}

/// The last compile, as it is sent to someone joining.
struct LastCompile {
    state: CompileState,
    sha256: Option<String>,
    diagnostics: Value,
}

struct Room {
    id: Uuid,
    hub: Arc<HubInner>,
    settings: Settings,
    compiler: SqlCompiler,
    shared: Arc<Shared>,

    doc: Doc,
    roots: Roots,
    status: LiveStatus,
    conns: HashMap<ConnId, Conn>,
    clients: HashMap<u64, ClientState>,

    writer: Writer,
    /// Writes that failed for good, reported by the writer.
    failures: mpsc::UnboundedReceiver<()>,
    /// Told to everyone who joins once a write failed.
    save_failed: bool,
    /// The doc's size, roughly: its state at load or compaction, plus the
    /// updates applied since.
    doc_bytes: usize,
    /// Renames typed in the text that arrived while frozen: moved if
    /// publishing fails.
    deferred_renames: Vec<(String, String)>,
    /// Applied updates not written yet, and when they will be.
    pending: Vec<Bytes>,
    flush_at: Option<Instant>,
    rows_since_snapshot: usize,

    /// The SQL as of the last update applied.
    sql: String,
    /// Changed since the last compile started: (first change, last change).
    dirty: Option<(Instant, Instant)>,
    retry_at: Option<Instant>,
    results: (
        mpsc::UnboundedSender<CompileDone>,
        mpsc::UnboundedReceiver<CompileDone>,
    ),
    last_compile: Option<LastCompile>,
    good_model: Option<Arc<Value>>,
    good_tables: HashMap<String, usize>,

    idle_since: Option<Instant>,
}

/// Loads the draft, then serves it until it unloads. Commands that arrive
/// while it loads wait in the queue.
pub async fn run(
    hub: Arc<HubInner>,
    id: Uuid,
    generation: u64,
    shared: Arc<Shared>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    mut data: mpsc::Receiver<Data>,
) {
    let loaded = match store::load(&hub.db, id).await {
        Ok(Some(loaded)) => loaded,
        Ok(None) => {
            tracing::warn!(draft = %id, "collab: no such draft");
            hub.remove(id, generation);
            return;
        }
        Err(err) => {
            tracing::error!(draft = %id, error = %err, "collab: cannot load the draft");
            hub.remove(id, generation);
            return;
        }
    };
    let mut room = match Room::new(hub, id, shared, loaded) {
        Ok(room) => room,
        Err((hub, err)) => {
            tracing::error!(draft = %id, error = %err, "collab: the stored doc does not decode");
            hub.remove(id, generation);
            return;
        }
    };
    tracing::debug!(draft = %id, "collab: room loaded");

    loop {
        let flush_at = room.flush_at;
        let compile_at = room.compile_due();
        // A frozen room stays: an unloaded one would come back open and take
        // edits the publish then silently drops.
        let idle_at = room
            .idle_since
            .filter(|_| !room.status.is_frozen())
            .map(|since| since + room.settings.idle_unload);
        tokio::select! {
            biased;
            command = commands.recv() => match command {
                Some(Command::Shutdown) | None => {
                    room.close_all(close::RESTART, "the server is restarting");
                    room.persist().await;
                    room.hub.remove(id, generation);
                    return;
                }
                Some(Command::MergeLayout { layout, groups, reply }) => {
                    if room.status.is_frozen() || room.status == LiveStatus::Open {
                        room.server_edit(|roots, txn| roots.apply_layout(txn, &layout, &groups));
                    }
                    // Written before the draft's base moves.
                    room.flush_pending();
                    room.writer.flush().await;
                    let _ = reply.send(());
                }
                Some(command) => room.handle(command),
            },
            Some(Data { conn, messages }) = data.recv() => {
                for message in messages {
                    room.incoming(conn, message);
                }
            }
            Some(()) = room.failures.recv() => room.save_failed(),
            Some(done) = room.results.1.recv() => room.compiled(done),
            () = at(compile_at) => room.start_compile(),
            () = at(flush_at) => room.flush_pending(),
            () = at(idle_at) => {
                room.persist().await;
                // Someone came back while we were writing: stay.
                if let Some(command) = room.hub.unload(id, generation, &mut commands) {
                    room.handle(command);
                } else {
                    tracing::debug!(draft = %id, "collab: room unloaded");
                    return;
                }
            }
        }
    }
}

async fn at(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

impl Room {
    fn new(
        hub: Arc<HubInner>,
        id: Uuid,
        shared: Arc<Shared>,
        loaded: Loaded,
    ) -> Result<Self, (Arc<HubInner>, yrs::encoding::read::Error)> {
        let doc = doc::new_doc();
        let roots = Roots::of(&doc);
        {
            let mut txn = doc.transact_mut();
            for bytes in &loaded.updates {
                match Update::decode_v1(bytes) {
                    Ok(update) => {
                        if let Err(err) = txn.apply_update(update) {
                            tracing::error!(draft = %id, error = %err, "collab: a stored update does not apply");
                        }
                    }
                    Err(err) => return Err((hub, err)),
                }
            }
        }
        let sql = roots.sql(&doc.transact());
        // A database deleted while this loaded: its drafts are deleted too.
        let database_deleted = hub.attach(loaded.database_id, &shared);
        let status = match loaded.status {
            _ if database_deleted || loaded.database_deleted => LiveStatus::Deleted,
            DraftStatus::Open => LiveStatus::Open,
            DraftStatus::Published => LiveStatus::Published,
            DraftStatus::Deleted => LiveStatus::Deleted,
        };
        let doc_bytes = loaded.updates.iter().map(Vec::len).sum();
        let (failed, failures) = mpsc::unbounded_channel();

        let sha256 = sha256_hex(&sql);
        let mut room = Self {
            id,
            settings: hub.settings.clone(),
            compiler: hub.compiler.clone(),
            writer: Writer::spawn(hub.db.clone(), id, loaded.last_update_id, failed),
            failures,
            save_failed: false,
            doc_bytes,
            deferred_renames: Vec::new(),
            hub,
            shared,
            doc,
            roots,
            status,
            conns: HashMap::new(),
            clients: HashMap::new(),
            pending: Vec::new(),
            flush_at: None,
            rows_since_snapshot: loaded.rows_since_snapshot,
            sql,
            dirty: None,
            retry_at: None,
            results: mpsc::unbounded_channel(),
            last_compile: None,
            good_model: None,
            good_tables: HashMap::new(),
            idle_since: Some(Instant::now()),
        };
        if let Some(saved) = loaded.compile {
            room.good_model = saved.good_model.map(Arc::new);
            room.good_tables = saved
                .good_tables
                .and_then(|tables| serde_json::from_value(tables).ok())
                .unwrap_or_default();
            if hex(&saved.source_hash) == sha256 {
                room.last_compile = Some(LastCompile {
                    state: if saved.model.is_some() {
                        CompileState::Ok
                    } else {
                        CompileState::Failed
                    },
                    sha256: Some(sha256),
                    diagnostics: saved.errors.unwrap_or_else(|| Value::Array(Vec::new())),
                });
            }
        }
        if room.last_compile.is_none() && room.status == LiveStatus::Open {
            let now = Instant::now();
            room.dirty = Some((now, now));
        }
        if room.rows_since_snapshot >= room.settings.compact_after {
            room.compact();
        }
        Ok(room)
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Join {
                conn,
                peer,
                out,
                joined,
            } => {
                self.join(conn, peer, out);
                let _ = joined.send(());
            }
            Command::Malformed { conn } => {
                if let Some(c) = self.conns.get(&conn) {
                    c.out.close(close::PROTOCOL, "malformed message");
                }
                self.leave(conn);
            }
            Command::Leave { conn } => self.leave(conn),
            Command::Content { reply } => {
                self.flush_pending();
                let _ = reply.send(self.content());
            }
            Command::Freeze { by, purpose, reply } => self.freeze(by, purpose, reply),
            Command::Unfreeze => self.unfreeze(),
            Command::Published { version, by } => {
                self.status = LiveStatus::Published;
                self.dirty = None;
                self.deferred_renames.clear();
                self.broadcast_event(
                    &ServerEvent::Status {
                        status: self.status,
                        version: Some(version),
                        by: Some(by),
                    },
                    None,
                );
            }
            Command::Renamed { name } => {
                self.broadcast_event(&ServerEvent::Renamed { name }, None);
            }
            Command::Delete { by, reply } => {
                if self.status.is_frozen() {
                    let _ = reply.send(Err(super::Publishing));
                    return;
                }
                let _ = reply.send(Ok(()));
                self.handle(Command::Deleted { by });
            }
            Command::Deleted { by } => {
                self.status = LiveStatus::Deleted;
                self.dirty = None;
                self.broadcast_event(
                    &ServerEvent::Status {
                        status: self.status,
                        version: None,
                        by: Some(by),
                    },
                    None,
                );
                self.close_all(close::DELETED, "the draft was deleted");
            }
            Command::Updated {
                version,
                by,
                conflicts,
            } => {
                self.broadcast_event(
                    &ServerEvent::Updated {
                        version,
                        by,
                        conflicts,
                    },
                    None,
                );
            }
            Command::VersionCreated { number } => {
                self.broadcast_event(&ServerEvent::VersionCreated { number }, None);
            }
            Command::Rewrite {
                from,
                changes,
                reason,
                reply,
            } => {
                let _ = reply.send(self.rewrite(&from, &changes, reason));
            }
            // Handled by the loop.
            Command::Shutdown | Command::MergeLayout { .. } => {}
        }
    }

    // ---- Connections ------------------------------------------------------

    fn join(&mut self, conn: ConnId, peer: Peer, out: Outbox) {
        let read_only = !peer.can_edit() || self.status != LiveStatus::Open;
        let welcome = protocol::event(&ServerEvent::Welcome {
            role: peer.role,
            read_only,
            status: self.status,
        });
        if self.status == LiveStatus::Deleted {
            // Joined as it was being deleted: let go at once.
            out.frame(welcome);
            out.close(close::DELETED, "the draft was deleted");
            return;
        }
        let mut first = vec![welcome];
        // The client answers with what it has that the doc lacks (edits
        // made offline); its own step 1 gets the doc's content back.
        first.push(protocol::sync_step1(&self.doc.transact().state_vector()));
        if let Some(everyone) = self.awareness_of(self.clients.keys().copied()) {
            first.push(everyone);
        }
        if let Some(compile) = self.compile_for_joiner() {
            first.push(compile);
        }
        if self.save_failed {
            first.push(protocol::event(&ServerEvent::SaveFailed));
        }
        for frame in first {
            if !out.frame(frame) {
                return;
            }
        }
        self.conns.insert(
            conn,
            Conn {
                stamp: peer.stamp(),
                peer,
                out,
                clients: HashSet::new(),
            },
        );
        self.idle_since = None;
        self.share_presence();
    }

    fn leave(&mut self, conn: ConnId) {
        let Some(gone) = self.conns.remove(&conn) else {
            return;
        };
        // Their cursors disappear for everyone else at once.
        let mut removed = HashMap::new();
        for client in gone.clients {
            if let Some(state) = self.clients.remove(&client) {
                removed.insert(
                    yrs::block::ClientID::new(client),
                    AwarenessUpdateEntry {
                        clock: state.clock.wrapping_add(1),
                        json: "null".into(),
                    },
                );
            }
        }
        if !removed.is_empty() {
            self.broadcast(
                &protocol::awareness(AwarenessUpdate { clients: removed }),
                None,
            );
        }
        if self.conns.is_empty() {
            self.idle_since = Some(Instant::now());
        }
        self.share_presence();
    }

    fn close_all(&mut self, code: u16, reason: &'static str) {
        for conn in self.conns.values() {
            conn.out.close(code, reason);
        }
    }

    fn share_presence(&self) {
        let mut present: Vec<Present> = Vec::new();
        for conn in self.conns.values() {
            match present.iter_mut().find(|p| p.user_id == conn.peer.user_id) {
                Some(p) => p.editing |= conn.peer.can_edit(),
                None => present.push(Present {
                    user_id: conn.peer.user_id,
                    name: conn.peer.name.clone(),
                    editing: conn.peer.can_edit(),
                }),
            }
        }
        present.sort_by(|a, b| a.name.cmp(&b.name));
        *self
            .shared
            .present
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = present;
    }

    fn send(&mut self, conn: ConnId, frame: Bytes) {
        let lagging = self.conns.get(&conn).is_some_and(|c| !c.out.frame(frame));
        if lagging {
            self.drop_lagging(conn);
        }
    }

    fn broadcast(&mut self, frame: &Bytes, except: Option<ConnId>) {
        let lagging: Vec<ConnId> = self
            .conns
            .iter()
            .filter(|(id, _)| Some(**id) != except)
            .filter(|(_, c)| !c.out.frame(frame.clone()))
            .map(|(id, _)| *id)
            .collect();
        for conn in lagging {
            self.drop_lagging(conn);
        }
    }

    fn broadcast_event(&mut self, event: &ServerEvent, except: Option<ConnId>) {
        self.broadcast(&protocol::event(event), except);
    }

    /// A connection whose queue is full is let go: it closes (4000), and the
    /// client reconnects and syncs again.
    fn drop_lagging(&mut self, conn: ConnId) {
        tracing::info!(draft = %self.id, conn, "collab: dropping a connection that cannot keep up");
        if let Some(c) = self.conns.get(&conn) {
            c.out.close(close::LAGGING, "too slow");
        }
        self.leave(conn);
    }

    // ---- y-sync -------------------------------------------------------------

    fn incoming(&mut self, conn: ConnId, message: Incoming) {
        if !self.conns.contains_key(&conn) {
            return;
        }
        match message {
            Incoming::Step1(sv) => {
                let update = self.doc.transact().encode_state_as_update_v1(&sv);
                self.send(conn, protocol::sync_step2(update));
            }
            Incoming::Update(bytes, update) => self.apply(conn, bytes, update),
            Incoming::Awareness(update) => self.awareness(conn, update),
            Incoming::AwarenessQuery => {
                if let Some(everyone) = self.awareness_of(self.clients.keys().copied()) {
                    self.send(conn, everyone);
                }
            }
        }
    }

    fn apply(&mut self, conn: ConnId, bytes: Bytes, update: Update) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        if !c.peer.can_edit() {
            tracing::debug!(draft = %self.id, user = %c.peer.user_id, "collab: dropped an update from a reviewer");
            return;
        }
        if self.status != LiveStatus::Open {
            // Brought back by the sync that follows an unfreeze.
            return;
        }
        if update.is_empty() {
            return;
        }
        if self.doc_bytes + bytes.len() > self.settings.max_doc_bytes {
            // Deleted content may have made room: measure before refusing.
            self.doc_bytes = self.state().len();
            if self.doc_bytes + bytes.len() > self.settings.max_doc_bytes {
                tracing::warn!(draft = %self.id, size = self.doc_bytes, "collab: refused an update past the size cap");
                if let Some(c) = self.conns.get(&conn) {
                    c.out
                        .close(close::TOO_LARGE, "the draft would be too large");
                }
                self.leave(conn);
                return;
            }
        }
        {
            let mut txn = self.doc.transact_mut();
            if let Err(err) = txn.apply_update(update) {
                tracing::warn!(draft = %self.id, error = %err, "collab: an update does not apply");
                return;
            }
        }
        self.doc_bytes += bytes.len();
        self.broadcast(&protocol::update(bytes.to_vec()), Some(conn));
        self.queue_write(bytes);
        self.sql_may_have_changed();
    }

    /// A change the server makes itself (moving a renamed table's entries,
    /// a rewrite), sent to everyone and stored like any other.
    fn server_edit(&mut self, edit: impl FnOnce(&Roots, &mut TransactionMut) -> bool) {
        let before = self.doc.transact().state_vector();
        let changed = {
            let mut txn = self.doc.transact_mut();
            edit(&self.roots, &mut txn)
        };
        if !changed {
            return;
        }
        let update = self.doc.transact().encode_state_as_update_v1(&before);
        self.doc_bytes += update.len();
        self.broadcast(&protocol::update(update.clone()), None);
        self.queue_write(update.into());
    }

    fn freeze(
        &mut self,
        by: String,
        purpose: FreezeFor,
        reply: oneshot::Sender<Result<Content, FreezeError>>,
    ) {
        if self.status != LiveStatus::Open {
            let _ = reply.send(Err(FreezeError::NotOpen(self.status)));
            return;
        }
        self.status = match purpose {
            FreezeFor::Publish => LiveStatus::Publishing,
            FreezeFor::Update => LiveStatus::Updating,
        };
        self.flush_pending();
        self.broadcast_event(
            &ServerEvent::Status {
                status: self.status,
                version: None,
                by: Some(by),
            },
            None,
        );
        let _ = reply.send(Ok(self.content()));
    }

    fn unfreeze(&mut self) {
        if !self.status.is_frozen() {
            return;
        }
        self.status = LiveStatus::Open;
        self.broadcast_event(
            &ServerEvent::Status {
                status: self.status,
                version: None,
                by: None,
            },
            None,
        );
        // Edits made while frozen were dropped: ask everyone for what they
        // have that the doc lacks.
        let step1 = protocol::sync_step1(&self.doc.transact().state_vector());
        self.broadcast(&step1, None);
        let renames = std::mem::take(&mut self.deferred_renames);
        if !renames.is_empty() {
            self.server_edit(|roots, txn| roots.rename(txn, &renames));
        }
    }

    /// Applies a rewrite of the SQL if the text is still the one it was
    /// computed against (see [`Command::Rewrite`]).
    fn rewrite(
        &mut self,
        from: &str,
        changes: &[patch::Change],
        reason: &str,
    ) -> Result<bool, RewriteError> {
        // Open, or frozen for an update (this rewrite is its merge).
        if !matches!(self.status, LiveStatus::Open | LiveStatus::Updating) {
            return Err(RewriteError::NotOpen(self.status));
        }
        let current = self.roots.sql(&self.doc.transact());
        if sha256_hex(&current) != from {
            return Err(RewriteError::Changed);
        }
        let mut edited = false;
        self.server_edit(|roots, txn| {
            edited = patch::apply(&roots.sql, txn, &current, changes);
            if edited {
                roots.note_rewrite(txn, reason);
            }
            edited
        });
        self.sql_may_have_changed();
        Ok(edited)
    }

    fn content(&self) -> Content {
        let txn = self.doc.transact();
        let sql = self.roots.sql(&txn);
        let compiled_now = self.last_compile.as_ref().is_some_and(|last| {
            last.state == CompileState::Ok && last.sha256.as_deref() == Some(&sha256_hex(&sql))
        });
        Content {
            layout: self.roots.layout(&txn),
            model: compiled_now.then(|| self.good_model.clone()).flatten(),
            sql,
        }
    }

    // ---- Awareness ----------------------------------------------------------

    fn awareness(&mut self, conn: ConnId, update: AwarenessUpdate) {
        let Some(c) = self.conns.get_mut(&conn) else {
            return;
        };
        let mut relay = HashMap::new();
        for (client_id, entry) in update.clients {
            let client = client_id.get();
            let known = self.clients.get(&client);
            if known.is_some_and(|state| state.conn != conn) {
                // Someone else's client id: never theirs to change.
                continue;
            }
            if !c.clients.contains(&client) {
                if c.clients.len() >= CLIENTS_PER_CONNECTION {
                    continue;
                }
                c.clients.insert(client);
            }
            let gone = &*entry.json == "null";
            let newer = known.is_none_or(|state| {
                state.clock < entry.clock || (state.clock == entry.clock && gone)
            });
            if !newer {
                continue;
            }
            let json: Arc<str> = if gone {
                "null".into()
            } else {
                match sanitize(&entry.json, &c.stamp) {
                    Some(json) => json.into(),
                    None => continue,
                }
            };
            self.clients.insert(
                client,
                ClientState {
                    clock: entry.clock,
                    json: Arc::clone(&json),
                    conn,
                },
            );
            relay.insert(
                client_id,
                AwarenessUpdateEntry {
                    clock: entry.clock,
                    json,
                },
            );
        }
        if !relay.is_empty() {
            // To the sender too: y-websocket counts any message as a sign
            // the connection is alive, and a lone editor gets no others.
            self.broadcast(
                &protocol::awareness(AwarenessUpdate { clients: relay }),
                None,
            );
        }
    }

    /// The current states of these clients, as one awareness message.
    fn awareness_of(&self, clients: impl Iterator<Item = u64>) -> Option<Bytes> {
        let entries: HashMap<_, _> = clients
            .filter_map(|client| {
                let state = self.clients.get(&client)?;
                (&*state.json != "null").then(|| {
                    (
                        yrs::block::ClientID::new(client),
                        AwarenessUpdateEntry {
                            clock: state.clock,
                            json: Arc::clone(&state.json),
                        },
                    )
                })
            })
            .collect();
        (!entries.is_empty()).then(|| protocol::awareness(AwarenessUpdate { clients: entries }))
    }

    // ---- Persistence ----------------------------------------------------------

    fn queue_write(&mut self, update: Bytes) {
        if self.pending.is_empty() {
            self.flush_at = Some(Instant::now() + self.settings.flush_every);
        }
        self.pending.push(update);
    }

    fn flush_pending(&mut self) {
        self.flush_at = None;
        if self.pending.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending);
        let merged = if pending.len() == 1 {
            pending[0].to_vec()
        } else {
            match yrs::merge_updates_v1(pending.iter().map(AsRef::as_ref)) {
                Ok(merged) => merged,
                Err(err) => {
                    // Store them one by one rather than lose them.
                    tracing::warn!(draft = %self.id, error = %err, "collab: cannot merge updates");
                    for update in pending {
                        self.writer.send(Job::Append(update.to_vec()));
                        self.rows_since_snapshot += 1;
                    }
                    return;
                }
            }
        };
        self.writer.send(Job::Append(merged));
        self.rows_since_snapshot += 1;
        if self.rows_since_snapshot >= self.settings.compact_after {
            self.compact();
        }
    }

    /// The doc's whole state, encoded.
    fn state(&self) -> Vec<u8> {
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    fn compact(&mut self) {
        let state = self.state();
        self.doc_bytes = state.len();
        self.writer.send(Job::Compact(state));
        self.rows_since_snapshot = 0;
    }

    /// A write failed for good: everyone hears it (their edits are in their
    /// tabs, not on the server).
    fn save_failed(&mut self) {
        if !self.save_failed {
            self.save_failed = true;
            self.broadcast_event(&ServerEvent::SaveFailed, None);
        }
    }

    /// Everything written: what unloading and shutting down wait for.
    async fn persist(&mut self) {
        self.flush_pending();
        if self.rows_since_snapshot > 0 {
            self.compact();
        }
        self.writer.flush().await;
    }

    // ---- Compiling ------------------------------------------------------------

    fn sql_may_have_changed(&mut self) {
        let sql = self.roots.sql(&self.doc.transact());
        if sql == self.sql {
            return;
        }
        self.sql = sql;
        let now = Instant::now();
        self.dirty = Some(self.dirty.map_or((now, now), |(first, _)| (first, now)));
    }

    fn compile_due(&self) -> Option<Instant> {
        let dirty = self.dirty.map(|(first, last)| {
            (last + self.settings.debounce).min(first + self.settings.max_wait)
        });
        match (dirty, self.retry_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn start_compile(&mut self) {
        self.dirty = None;
        self.retry_at = None;
        if self.status == LiveStatus::Published || self.status == LiveStatus::Deleted {
            return;
        }
        if self.status == LiveStatus::Open {
            self.collapse_markers();
        }
        let (compiler, id, sql) = (self.compiler.clone(), self.id, self.sql.clone());
        let results = self.results.0.clone();
        tokio::spawn(async move {
            let sha256 = sha256_hex(&sql);
            let tables = compile::created_at(&sql);
            let result = compiler.compile_for(&id.to_string(), sql).await;
            let _ = results.send(CompileDone {
                sha256,
                tables,
                result,
            });
        });
    }

    /// Removes the graph edits markers after the first (see the module
    /// documentation). Not a rewrite: the characters removed are markers.
    fn collapse_markers(&mut self) {
        let extra = doc::extra_markers(&self.sql);
        if extra.is_empty() {
            return;
        }
        let changes: Vec<patch::Change> = extra
            .into_iter()
            .map(|delete| patch::Change {
                delete,
                insert: String::new(),
            })
            .collect();
        let current = self.sql.clone();
        self.server_edit(|roots, txn| patch::apply(&roots.sql, txn, &current, &changes));
        self.sql = self.roots.sql(&self.doc.transact());
    }

    fn compiled(&mut self, done: CompileDone) {
        let CompileDone {
            sha256,
            tables,
            result,
        } = done;
        let compiled = match result {
            Ok(compiled) => compiled,
            Err(Rejected::Superseded) => return,
            Err(Rejected::Api(err)) => {
                tracing::warn!(draft = %self.id, reason = %err, "collab: cannot compile");
                self.retry_at = Some(Instant::now() + self.settings.retry_unavailable);
                self.last_compile = Some(LastCompile {
                    state: CompileState::Unavailable,
                    sha256: None,
                    diagnostics: Value::Array(Vec::new()),
                });
                self.broadcast_event(
                    &ServerEvent::Compile {
                        state: CompileState::Unavailable,
                        sha256: None,
                        diagnostics: Vec::new(),
                        model: None,
                    },
                    None,
                );
                return;
            }
        };
        let diagnostics = serde_json::to_value(&compiled.diagnostics).unwrap_or_default();
        let source_hash = unhex(&sha256);
        if let Some(model) = compiled.model {
            let renames = compile::renames(&self.good_tables, &tables);
            match self.status {
                _ if renames.is_empty() => {}
                LiveStatus::Open => {
                    tracing::debug!(draft = %self.id, ?renames, "collab: carrying layout over renames");
                    self.server_edit(|roots, txn| roots.rename(txn, &renames));
                }
                // Moved if publishing fails (and moot if it succeeds).
                LiveStatus::Publishing | LiveStatus::Updating => {
                    self.deferred_renames.extend(renames);
                }
                LiveStatus::Published | LiveStatus::Deleted => {}
            }
            let model = Arc::new(serde_json::to_value(&model).unwrap_or_default());
            self.good_model = Some(Arc::clone(&model));
            self.good_tables = tables;
            self.writer.send(Job::Compile(Box::new(CompileRow {
                source_hash,
                model: Some(Arc::clone(&model)),
                errors: None,
                tables: serde_json::to_value(&self.good_tables).ok(),
            })));
            self.broadcast_event(
                &ServerEvent::Compile {
                    state: CompileState::Ok,
                    sha256: Some(sha256.clone()),
                    diagnostics: compiled.diagnostics,
                    model: Some(model),
                },
                None,
            );
            self.last_compile = Some(LastCompile {
                state: CompileState::Ok,
                sha256: Some(sha256),
                diagnostics,
            });
        } else {
            self.writer.send(Job::Compile(Box::new(CompileRow {
                source_hash,
                model: None,
                errors: Some(diagnostics.clone()),
                tables: None,
            })));
            self.broadcast_event(
                &ServerEvent::Compile {
                    state: CompileState::Failed,
                    sha256: Some(sha256.clone()),
                    diagnostics: compiled.diagnostics,
                    model: None,
                },
                None,
            );
            self.last_compile = Some(LastCompile {
                state: CompileState::Failed,
                sha256: Some(sha256),
                diagnostics,
            });
        }
    }

    /// The compile event for someone joining: the last result, with the
    /// last model that compiled whatever the result was.
    fn compile_for_joiner(&self) -> Option<Bytes> {
        let last = self.last_compile.as_ref();
        if last.is_none() && self.good_model.is_none() {
            return None;
        }
        let event = serde_json::json!({
            "type": "compile",
            "state": last.map_or(CompileState::Pending, |l| l.state.clone()),
            "sha256": last.and_then(|l| l.sha256.clone()),
            "diagnostics": last.map_or(Value::Array(Vec::new()), |l| l.diagnostics.clone()),
            "model": self.good_model.as_deref(),
        });
        let json = serde_json::to_vec(&event).unwrap_or_default();
        Some(
            yrs::updates::encoder::Encode::encode_v1(&Message::Custom(
                protocol::MESSAGE_EVENT,
                json,
            ))
            .into(),
        )
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_socket_queue_holds_so_many_bytes() {
        let (out, mut inbox) = queue(10);
        assert!(out.frame(Bytes::from_static(b"123456")));
        assert!(!out.frame(Bytes::from_static(b"123456")), "past the limit");
        assert!(out.frame(Bytes::from_static(b"1234")));
        inbox.recv().await.unwrap();
        inbox.recv().await.unwrap();
        // Empty again: one frame larger than the limit fits (a first sync).
        assert!(out.frame(Bytes::from(vec![0; 64])));
        assert!(!out.frame(Bytes::from_static(b"1")));
    }
}
