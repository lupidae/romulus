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
//! - **Publishing** freezes the room (every update dropped) until it is
//!   published or the attempt fails; then every connection is asked to sync
//!   again, which brings back what was dropped meanwhile.
//! - **Unloading**: [`Settings::idle_unload`] after the last connection
//!   leaves, the room writes everything and leaves the hub.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
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
    compile::{self, CompileError, CompileState, Compiled, DraftCompiler, sha256_hex},
    doc::{self, Presentation, Roots},
    protocol::{self, LiveStatus, ServerEvent},
    store::{self, CompileRow, Job, Loaded, Writer},
};
use crate::domain::DraftStatus;

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
        }
    }
}

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
    /// The draft was deleted: do not reconnect (4400–4499: final, as y-websocket reads them).
    pub const DELETED: u16 = 4404;
    /// The draft could not be loaded.
    pub const UNAVAILABLE: u16 = 1011;
}

/// The draft's content at one moment.
#[derive(Debug, Clone)]
pub struct Content {
    pub sql: String,
    pub presentation: Presentation,
    /// The model of exactly this SQL, when the last compile was of it and
    /// succeeded (publishing then needs no compile of its own).
    pub model: Option<Arc<Value>>,
}

#[derive(Debug, thiserror::Error)]
pub enum FreezeError {
    #[error("the draft is not open ({0:?})")]
    NotOpen(LiveStatus),
}

pub enum Command {
    Join {
        conn: ConnId,
        peer: Peer,
        out: mpsc::Sender<Outgoing>,
        joined: oneshot::Sender<()>,
    },
    Incoming {
        conn: ConnId,
        messages: Vec<Incoming>,
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
        reply: oneshot::Sender<Result<Content, FreezeError>>,
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
    VersionCreated {
        number: i32,
    },
    Shutdown,
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
    out: mpsc::Sender<Outgoing>,
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
    result: Result<Compiled, CompileError>,
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
    compiler: DraftCompiler,
    shared: Arc<Shared>,

    doc: Doc,
    roots: Roots,
    status: LiveStatus,
    conns: HashMap<ConnId, Conn>,
    clients: HashMap<u64, ClientState>,

    writer: Writer,
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
        let idle_at = room
            .idle_since
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
                Some(command) => room.handle(command),
            },
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
        let _ = shared.database_id.set(loaded.database_id);
        let status = match loaded.status {
            DraftStatus::Open => LiveStatus::Open,
            DraftStatus::Published => LiveStatus::Published,
            DraftStatus::Deleted => LiveStatus::Deleted,
        };

        let sha256 = sha256_hex(&sql);
        let mut room = Self {
            id,
            settings: hub.settings.clone(),
            compiler: hub.compiler.clone(),
            writer: Writer::spawn(hub.db.clone(), id, loaded.last_update_id),
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
            Command::Incoming { conn, messages } => {
                for message in messages {
                    self.incoming(conn, message);
                }
            }
            Command::Malformed { conn } => {
                if let Some(c) = self.conns.get(&conn) {
                    let _ = c
                        .out
                        .try_send(Outgoing::Close(close::PROTOCOL, "malformed message"));
                }
                self.leave(conn);
            }
            Command::Leave { conn } => self.leave(conn),
            Command::Content { reply } => {
                self.flush_pending();
                let _ = reply.send(self.content());
            }
            Command::Freeze { by, reply } => {
                if self.status != LiveStatus::Open {
                    let _ = reply.send(Err(FreezeError::NotOpen(self.status)));
                    return;
                }
                self.status = LiveStatus::Publishing;
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
            Command::Unfreeze => {
                if self.status == LiveStatus::Publishing {
                    self.status = LiveStatus::Open;
                    self.broadcast_event(
                        &ServerEvent::Status {
                            status: self.status,
                            version: None,
                            by: None,
                        },
                        None,
                    );
                    // Edits made while frozen were dropped: ask everyone
                    // for what they have that the doc lacks.
                    let step1 = protocol::sync_step1(&self.doc.transact().state_vector());
                    self.broadcast(&step1, None);
                }
            }
            Command::Published { version, by } => {
                self.status = LiveStatus::Published;
                self.dirty = None;
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
            Command::VersionCreated { number } => {
                self.broadcast_event(&ServerEvent::VersionCreated { number }, None);
            }
            // Handled by the loop.
            Command::Shutdown => {}
        }
    }

    // ---- Connections ------------------------------------------------------

    fn join(&mut self, conn: ConnId, peer: Peer, out: mpsc::Sender<Outgoing>) {
        let read_only = !peer.can_edit() || self.status != LiveStatus::Open;
        let mut first = vec![protocol::event(&ServerEvent::Welcome {
            role: peer.role,
            read_only,
            status: self.status,
        })];
        // The client answers with what it has that the doc lacks (edits
        // made offline); its own step 1 gets the doc's content back.
        first.push(protocol::sync_step1(&self.doc.transact().state_vector()));
        if let Some(everyone) = self.awareness_of(self.clients.keys().copied()) {
            first.push(everyone);
        }
        if let Some(compile) = self.compile_for_joiner() {
            first.push(compile);
        }
        for frame in first {
            if out.try_send(Outgoing::Frame(frame)).is_err() {
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
            let _ = conn.out.try_send(Outgoing::Close(code, reason));
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
        let lagging = self
            .conns
            .get(&conn)
            .is_some_and(|c| c.out.try_send(Outgoing::Frame(frame)).is_err());
        if lagging {
            self.drop_lagging(conn);
        }
    }

    fn broadcast(&mut self, frame: &Bytes, except: Option<ConnId>) {
        let lagging: Vec<ConnId> = self
            .conns
            .iter()
            .filter(|(id, _)| Some(**id) != except)
            .filter(|(_, c)| c.out.try_send(Outgoing::Frame(frame.clone())).is_err())
            .map(|(id, _)| *id)
            .collect();
        for conn in lagging {
            self.drop_lagging(conn);
        }
    }

    fn broadcast_event(&mut self, event: &ServerEvent, except: Option<ConnId>) {
        self.broadcast(&protocol::event(event), except);
    }

    /// A connection whose queue is full is let go: dropping its sender
    /// closes the socket, and the client reconnects and syncs again.
    fn drop_lagging(&mut self, conn: ConnId) {
        tracing::info!(draft = %self.id, conn, "collab: dropping a connection that cannot keep up");
        if let Some(c) = self.conns.get(&conn) {
            let _ = c.out.try_send(Outgoing::Close(close::LAGGING, "too slow"));
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
        {
            let mut txn = self.doc.transact_mut();
            if let Err(err) = txn.apply_update(update) {
                tracing::warn!(draft = %self.id, error = %err, "collab: an update does not apply");
                return;
            }
        }
        self.broadcast(&protocol::update(bytes.to_vec()), Some(conn));
        self.queue_write(bytes);
        self.sql_may_have_changed();
    }

    /// A change the server makes itself (moving a renamed table's entries),
    /// sent to everyone and stored like any other.
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
        self.broadcast(&protocol::update(update.clone()), None);
        self.queue_write(update.into());
    }

    fn content(&self) -> Content {
        let txn = self.doc.transact();
        let sql = self.roots.sql(&txn);
        let compiled_now = self.last_compile.as_ref().is_some_and(|last| {
            last.state == CompileState::Ok && last.sha256.as_deref() == Some(&sha256_hex(&sql))
        });
        Content {
            presentation: self.roots.presentation(&txn),
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

    fn compact(&mut self) {
        let state = self
            .doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        self.writer.send(Job::Compact(state));
        self.rows_since_snapshot = 0;
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
        let (compiler, id, sql) = (self.compiler.clone(), self.id, self.sql.clone());
        let results = self.results.0.clone();
        tokio::spawn(async move {
            let sha256 = sha256_hex(&sql);
            let tables = compile::created_at(&sql);
            let result = compiler.compile_for(id, sql).await;
            let _ = results.send(CompileDone {
                sha256,
                tables,
                result,
            });
        });
    }

    fn compiled(&mut self, done: CompileDone) {
        let CompileDone {
            sha256,
            tables,
            result,
        } = done;
        let compiled = match result {
            Ok(compiled) => compiled,
            Err(CompileError::Superseded) => return,
            Err(CompileError::Unavailable(reason)) => {
                tracing::warn!(draft = %self.id, %reason, "collab: cannot compile");
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
            if !renames.is_empty() && self.status == LiveStatus::Open {
                tracing::debug!(draft = %self.id, ?renames, "collab: carrying layout over renames");
                self.server_edit(|roots, txn| roots.rename(txn, &renames));
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
