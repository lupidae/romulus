//! One loaded draft: its Y.Doc, the connections in it, and the work around
//! them, run by a single task (an actor) so nothing needs a lock.
//!
//! - **Sync**: y-sync with each connection. Document updates from people
//!   who may not edit (reviewers), or while the draft is not open, are
//!   dropped; their awareness still goes through. A Yjs client id is the
//!   person's whose update extended it alone (a tab's first edit), or whose
//!   connection announced it and then extended it ([`Owners`]). An update
//!   extending an id bound to someone else (or the room's own) is refused
//!   with 4409 (reload). An update extending one id alone takes it over
//!   from a connection that only announced it (an editor's is closed 4409:
//!   it never wrote with it, so loses nothing); a reviewer's announcement
//!   never holds an id. An update carrying several ids, one of them
//!   announced by another editor's open tab and not bound yet, is closed
//!   4003 (sync again: that tab brings its own edits first). Others'
//!   structs an editor carries (edits the server lost, back in a sync step
//!   2) are kept, but the comment changes of such an update are not (they
//!   could be anyone's). An update whose structs wait on others the doc
//!   lacks (a tab typing before its step 2 after a reconnect) keeps what
//!   applied; the rest is dropped and the tab asked for a step 2 again
//!   (`SyncStep1`), which brings it with what it waited on: nothing waits
//!   to land later in someone else's transaction, nothing is lost. Stored
//!   updates that wait on structs never stored ([`Waiting`]) are kept out
//!   of the doc (and in every snapshot), retried only once the doc reaches
//!   what they wait on, in a transaction of their own: sent to everyone,
//!   their comment changes taken back.
//! - **Comments**: written by the room for whoever asks, reviewers
//!   included, with an operation on the socket (`comments::Op`, budgeted
//!   per person; deletions of others' comments by developers and admins
//!   go to the audit log). Editors' direct changes to the threads and comments (a
//!   `---` line posted, a rename moving threads) are checked
//!   (`comments::check`, list content removed); what may not be is written
//!   back in the update everyone else receives with it, so nobody sees a
//!   forged comment, even for a moment. A `---` line left in the source
//!   fails the compile (`comments::comment_lines`).
//! - **Awareness**: each state is stamped with its connection's identity
//!   (see `awareness`), and a connection only speaks for the Yjs client ids
//!   it announced first.
//! - **Persistence**: applied updates are batched, merged and appended to
//!   `draft_updates`; after [`Settings::compact_after`] rows, or when the
//!   room unloads, the doc's state replaces the snapshot.
//! - **Modes**: the source is the doc's `sql` (schema mode) or its
//!   `migration` (migration mode), compiled after the base version's SQL
//!   (`SqlCompiler::compile_migration_for`), the schema then derived and
//!   sent as SQL with each compile. A switch ([`Command::SwitchMode`])
//!   seeds or clears the migration, rewrites `sql` when going back, and
//!   notes the mode in `meta.mode`, in one transaction; then `drafts.mode`
//!   is written, by this room's writer. `meta.mode` is the server's to
//!   write: an update changing it is answered by writing it back.
//! - **Compile on type**: a change to the source schedules a compile
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
//! - **Review**: the first update of a person's that changes the SQL makes
//!   them an editor; approvals count while the text is the one approved
//!   (see `review`); everyone hears when that changes.
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
        atomic::{AtomicBool, AtomicUsize, Ordering},
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
    Doc, Observable, ReadTxn, StateVector, Transact, TransactionMut, Update,
    sync::{
        Message, SyncMessage,
        awareness::{AwarenessUpdate, AwarenessUpdateEntry},
    },
    updates::decoder::Decode,
};

use super::{
    HubInner, Present,
    awareness::{Peer, sanitize},
    comments::{self, Changes, Request},
    compile::{self, CompileState, Compiled, Rejected, SqlCompiler, sha256_hex},
    doc::{self, Roots},
    patch,
    protocol::{self, LiveStatus, ReviewRequest, ServerEvent},
    review::{Approval, ApprovalError, Review},
    store::{self, BaseSql, CompileRow, Job, Loaded, Writer},
};
use crate::{
    audit,
    domain::{AuditAction, DraftMode, DraftStatus, PgMajor},
    drafts::tidy::canonical_sql,
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

/// Connections one person may have to one draft (tabs, devices).
pub const MAX_TABS: usize = 8;

/// What the socket task hands the room, already decoded (decoding runs
/// there, so a malformed frame only costs its own connection).
pub enum Incoming {
    Step1(StateVector),
    /// A document update, raw (to relay and store) and decoded (to apply).
    Update(Bytes, Update),
    Awareness(AwarenessUpdate),
    AwarenessQuery,
    /// A comment operation (`protocol::MESSAGE_COMMENT`), its mentions
    /// already checked against the organization's members.
    Comment(Request),
    /// A comment operation the socket refused before anything else (too
    /// many in a frame, or too fast): only answered.
    CommentRefused {
        id: String,
    },
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
            Message::Custom(protocol::MESSAGE_COMMENT, body) => {
                serde_json::from_slice::<Request>(&body)
                    .ok()
                    .filter(|request| request.id.len() <= comments::MAX_REQUEST_ID)
                    .map(|mut request| {
                        // Never more looked up than kept.
                        if let Some(mentions) = request.op.mentions_mut() {
                            let mut seen = HashSet::new();
                            mentions.retain(|id| seen.insert(*id));
                            mentions.truncate(comments::MAX_MENTIONS);
                        }
                        Self::Comment(request)
                    })
                    .ok_or(())
            }
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
    /// The person has the draft open in too many tabs already. Final.
    pub const TOO_MANY: u16 = 4429;
    /// An update would make the draft larger than it may be: the update was
    /// not applied; reload.
    pub const TOO_LARGE: u16 = 4413;
    /// The tab wrote with a client id bound to another person: its copy no
    /// longer matches the server's; reload. Final.
    pub const OUT_OF_STEP: u16 = 4409;
    /// The tab wrote with a client id another person's open tab announced:
    /// reconnect and sync again (that tab brings its own edits first).
    pub const RESYNC: u16 = 4003;
    /// The draft could not be loaded.
    pub const UNAVAILABLE: u16 = 1011;
}

/// The draft's content at one moment.
#[derive(Debug, Clone)]
pub struct Content {
    pub mode: DraftMode,
    /// The schema SQL: the source in schema mode (unused in migration mode).
    pub sql: String,
    /// The migration: the source in migration mode.
    pub migration: String,
    /// The base version (`None`: the database had none), and its SQL (empty
    /// without one): what the migration applies to. They move together.
    pub base_version: Option<Uuid>,
    pub base_sql: Arc<str>,
    /// What the draft compiles on: its database's major.
    pub major: PgMajor,
    pub layout: VersionLayout,
    /// The model of exactly this source (and base), when the last compile
    /// was of it and succeeded (publishing then needs no compile of its
    /// own).
    pub model: Option<Arc<Value>>,
    /// The version "Update draft" last merged into the text (`meta.merged`).
    pub merged: Option<i32>,
    /// Who approved exactly this SQL without having edited it (`review`).
    pub approved_by: Vec<Uuid>,
    /// The comment threads, with their comments (`comments::discussion`).
    pub discussion: Vec<comments::DocThread>,
}

impl Content {
    /// The text that is the source: the SQL, or the migration.
    pub fn source(&self) -> &str {
        match self.mode {
            DraftMode::Schema => &self.sql,
            DraftMode::Migration => &self.migration,
        }
    }
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
    /// The SQL is not the source (migration mode): nothing to rewrite.
    #[error("the draft is in {0:?} mode")]
    Mode(DraftMode),
    /// Someone edited it since the changes were computed: compute again.
    #[error("the SQL changed meanwhile")]
    Changed,
}

/// Why a draft is frozen: what everyone sees meanwhile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeFor {
    Publish,
    /// With the ticket the update's own rewrite shows ([`Command::Rewrite`]):
    /// nobody else rewrites the SQL meanwhile.
    Update(UpdateTicket),
}

/// What lets "Update draft" rewrite the SQL of the room it froze.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateTicket(pub(super) u64);

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
    /// Whether the doc has this comment thread.
    ThreadExists {
        thread: Uuid,
        reply: oneshot::Sender<bool>,
    },
    Freeze {
        by: String,
        purpose: FreezeFor,
        reply: oneshot::Sender<Result<Content, FreezeError>>,
    },
    /// "Update draft"'s layout and group changes; answered once everything
    /// the room changed is written (handled by the loop).
    MergeLayout {
        ticket: UpdateTicket,
        layout: Vec<(String, Option<TablePosition>)>,
        groups: Vec<(String, Option<TableGroup>)>,
        reply: oneshot::Sender<Result<(), RewriteError>>,
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
    /// The database moved to another Postgres major.
    Major {
        major: PgMajor,
        by: String,
    },
    /// News for everyone in the draft, as it is (a version removed or
    /// restored).
    Event(ServerEvent),
    /// May `user_id` approve the text hashing to `sha256`? (Then the
    /// approval is written, and [`ReviewChange::Approved`] follows.)
    CheckApproval {
        user_id: Uuid,
        sha256: String,
        /// With what the approval is of: the source on its base
        /// (`compile::input_hash`), what approvals are kept and counted by.
        reply: oneshot::Sender<Result<String, ApprovalError>>,
    },
    Review(ReviewChange),
    /// Replace the SQL with `changes`, computed against the text whose
    /// SHA-256 is `from`, for `reason` (`tidy`…): whether anything changed.
    Rewrite {
        from: String,
        changes: Vec<patch::Change>,
        reason: &'static str,
        /// The update's ticket: while frozen for an update, only that
        /// update rewrites the SQL (and only while it is).
        ticket: Option<UpdateTicket>,
        /// The version an update merges: noted in the same transaction.
        merged: Option<i32>,
        reply: oneshot::Sender<Result<bool, RewriteError>>,
    },
    /// The draft was updated to a newer version (its base moved): `base` is
    /// that version and its SQL (what a migration applies to now).
    Updated {
        version: i32,
        by: String,
        conflicts: u32,
        base: Option<(Uuid, Arc<str>)>,
    },
    /// Switch the draft to `to` mode, if its source is still the text
    /// whose SHA-256 is `from` and its base still `base` (the seed was
    /// computed from both): the migration becomes `seed` (to migration
    /// mode), or the schema SQL does (back to schema mode, the migration
    /// cleared, `drafts.base_sql` as `base_sql` says). Answered
    /// once the switch is written; `false` when already in that mode
    /// (handled by the loop).
    SwitchMode {
        to: DraftMode,
        from: String,
        base: Option<Uuid>,
        seed: String,
        base_sql: BaseSql,
        by: String,
        reply: oneshot::Sender<Result<bool, RewriteError>>,
    },
    Shutdown,
}

/// What changed in a draft's review, written already.
pub enum ReviewChange {
    /// An approval was written.
    Approved(Approval),
    /// Someone took their approval back.
    Withdrawn(Uuid),
    /// The editors asked for a review, or no longer do.
    Requested(Option<ReviewRequest>),
    /// The organization changed how many approvals publishing needs.
    Required(i32),
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
    pub org_id: std::sync::OnceLock<Uuid>,
    pub present: std::sync::Mutex<Vec<Present>>,
    /// Who approved the text as it is now, without having edited it
    /// (`None` until the room loaded).
    pub approvals: std::sync::Mutex<Option<Vec<Uuid>>>,
}

struct Conn {
    peer: Peer,
    stamp: Value,
    out: Outbox,
    /// Yjs client ids this connection speaks for.
    clients: HashSet<u64>,
}

/// So many of something in a burst, then so many a second.
pub(crate) struct Budget {
    tokens: f64,
    at: Instant,
}

impl Budget {
    /// Comment operations, per person in a draft: someone commenting sends
    /// a few at a time.
    const BURST: f64 = 30.0;
    const PER_SECOND: f64 = 2.0;

    pub(crate) fn new() -> Self {
        Self {
            tokens: Self::BURST,
            at: Instant::now(),
        }
    }

    pub(crate) fn take(&mut self) -> bool {
        let now = Instant::now();
        self.tokens =
            (self.tokens + (now - self.at).as_secs_f64() * Self::PER_SECOND).min(Self::BURST);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Whose each Yjs client id is, for the room's life: the person whose
/// connection announced it (awareness, first come: a tab announces its own
/// as it opens) and then extended it (its first edit). A person keeps at
/// most [`MAX_TABS`] (one per tab they may have open); past that, their
/// oldest is forgotten.
#[derive(Debug, Default)]
struct Owners {
    by_client: HashMap<u64, Uuid>,
    by_person: HashMap<Uuid, std::collections::VecDeque<u64>>,
}

impl Owners {
    fn owner(&self, client: u64) -> Option<Uuid> {
        self.by_client.get(&client).copied()
    }

    fn bind(&mut self, client: u64, person: Uuid) {
        if self.by_client.contains_key(&client) {
            return;
        }
        self.by_client.insert(client, person);
        let theirs = self.by_person.entry(person).or_default();
        theirs.push_back(client);
        while theirs.len() > MAX_TABS {
            if let Some(oldest) = theirs.pop_front() {
                self.by_client.remove(&oldest);
            }
        }
    }
}

/// The clients whose structs an update adds beyond what the doc has (`sv`):
/// the ids it writes new content with.
fn extended_clients(update: &Update, sv: &StateVector) -> Vec<u64> {
    update
        .insertions(true)
        .iter()
        .filter(|(client, ranges)| {
            let known = sv.get(client);
            ranges.iter().any(|range| range.end > known)
        })
        .map(|(client, _)| client.get())
        .collect()
}

/// Stored updates waiting on structs never stored (a write lost before a
/// restart): kept as stored, and in every snapshot, never applied in
/// someone else's transaction.
struct Waiting {
    bytes: Vec<u8>,
    /// Per client, a clock they wait on: once the doc has it, try again.
    missing: Vec<(u64, u32)>,
}

/// Takes what waits out of the doc (yrs's pending structs and deletes).
fn waiting_of(txn: &mut TransactionMut) -> Option<Waiting> {
    let mut missing = Vec::new();
    {
        let store = txn.store();
        if let Some(pending) = store.pending_update() {
            missing.extend(pending.missing.iter().map(|(c, clock)| (c.get(), *clock)));
        }
        if let Some(deletes) = store.pending_ds() {
            for (client, ranges) in deletes.iter() {
                missing.extend(ranges.iter().map(|range| (client.get(), range.start)));
            }
        }
    }
    let update = yrs::WriteTxn::prune_pending(txn)?;
    Some(Waiting {
        bytes: yrs::updates::encoder::Encode::encode_v1(&update),
        missing,
    })
}

/// Whether the doc (`sv`) has any of what waits on.
fn reached(sv: &StateVector, missing: &[(u64, u32)]) -> bool {
    missing
        .iter()
        .any(|&(client, clock)| sv.get(&yrs::block::ClientID::new(client)) > clock)
}

struct ClientState {
    clock: u32,
    /// Sanitized JSON, or `null` once the client said it left.
    json: Arc<str>,
    conn: ConnId,
}

/// A compile, finished.
struct CompileDone {
    /// The mode it compiled the source of.
    mode: DraftMode,
    /// The major it compiled on.
    major: PgMajor,
    /// Of the source compiled (what clients compare with their text).
    sha256: String,
    /// What it compiled (`compile::input_hash`).
    input: String,
    /// In migration mode, the schema the model is, as SQL.
    schema_sql: Option<Arc<str>>,
    /// Where it creates each relation (`compile::created_at`).
    tables: HashMap<String, usize>,
    result: Result<Compiled, Rejected>,
}

/// The last compile, as it is sent to someone joining.
struct LastCompile {
    state: CompileState,
    sha256: Option<String>,
    /// `compile::input_hash` of what it compiled.
    input: Option<String>,
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
    /// Which text is the source (the doc's `meta.mode`; `drafts.mode`
    /// follows it).
    mode: DraftMode,
    /// The base version, and its SQL (empty without one), for migration
    /// mode.
    base_version: Option<Uuid>,
    base_sql: Arc<str>,
    /// What it compiles on: its database's major.
    major: PgMajor,
    /// The text that is not the source, as it must stay: an edit of it (a
    /// tab that had not seen a switch) is undone.
    inactive: String,
    /// Set when a transaction changes `sql` / `migration` (observers).
    touched_sql: Arc<AtomicBool>,
    touched_migration: Arc<AtomicBool>,
    /// What the last transaction changed in the threads and comments
    /// (observers), for [`Room::check_comments`].
    comment_changes: Arc<std::sync::Mutex<Changes>>,
    /// Comment operations each person may still send (survives their
    /// reconnecting, unlike a connection).
    op_budgets: HashMap<Uuid, Budget>,
    conns: HashMap<ConnId, Conn>,
    clients: HashMap<u64, ClientState>,
    /// Whose each Yjs client id is (see [`Owners`]).
    owners: Owners,
    /// Stored updates waiting on structs never stored.
    waiting: Option<Waiting>,

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
    /// While frozen for an update: the ticket its rewrite shows.
    update: Option<UpdateTicket>,
    /// Applied updates not written yet, and when they will be.
    pending: Vec<Bytes>,
    flush_at: Option<Instant>,
    rows_since_snapshot: usize,

    /// The source text (the SQL, or the migration) as of the last update
    /// applied.
    source: String,
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
    /// In migration mode: `good_model` as SQL, for whoever joins.
    schema_sql: Option<Arc<str>>,

    review: Review,
    /// The approvals that counted when everyone last heard (an edit that
    /// changes them is told).
    review_shown: Option<Vec<Uuid>>,

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
    let mut loaded = match store::load(&hub.db, id).await {
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
    // The base's SQL once, off the async workers (a pushed model is rendered).
    let base = loaded.base.take();
    let base_version = base.as_ref().map(|base| base.id);
    let base_sql: Arc<str> = match base {
        Some(base) => tokio::task::spawn_blocking(move || base.sql())
            .await
            .unwrap_or_default()
            .into(),
        None => "".into(),
    };
    let mut room = match Room::new(hub, id, shared, loaded, (base_version, base_sql)) {
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
            command = commands.recv() => {
                if !room.command(command.unwrap_or(Command::Shutdown), generation).await {
                    return;
                }
            }
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
                    if !room.command(command, generation).await {
                        return;
                    }
                } else {
                    tracing::debug!(draft = %id, "collab: room unloaded");
                    return;
                }
            }
        }
    }
}

impl Room {
    /// Runs a command, those that wait on writes included (a switch, an
    /// update's layout); false once the room has stopped (shutdown).
    async fn command(&mut self, command: Command, generation: u64) -> bool {
        match command {
            Command::Shutdown => {
                self.close_all(close::RESTART, "the server is restarting");
                self.persist().await;
                self.hub.remove(self.id, generation);
                return false;
            }
            Command::SwitchMode {
                to,
                from,
                base,
                seed,
                base_sql,
                by,
                reply,
            } => {
                let switched = self.switch_mode(to, (&from, base), &seed, base_sql, by);
                // Written (the doc, then drafts.mode) before answering: the
                // caller reads the draft back.
                self.flush_pending();
                self.writer.flush().await;
                let _ = reply.send(switched);
            }
            Command::MergeLayout {
                ticket,
                layout,
                groups,
                reply,
            } => {
                // Only the update that froze the room, while it is.
                if self.status == LiveStatus::Updating && self.update == Some(ticket) {
                    self.server_edit(|roots, txn| roots.apply_layout(txn, &layout, &groups));
                    // Written before the draft's base moves.
                    self.flush_pending();
                    self.writer.flush().await;
                    let _ = reply.send(Ok(()));
                } else {
                    let _ = reply.send(Err(RewriteError::NotOpen(self.status)));
                }
            }
            command => self.handle(command),
        }
        true
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
        (base_version, base_sql): (Option<Uuid>, Arc<str>),
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
        // Kept as stored, out of the doc (snapshots keep them): never
        // dropped, never applied in someone else's transaction.
        let waiting = waiting_of(&mut doc.transact_mut());
        if waiting.is_some() {
            tracing::error!(draft = %id, "collab: stored updates wait on structs never stored; kept waiting");
        }
        // The doc's note of the mode wins: only rooms write it, in the
        // transaction of the switch; `drafts.mode` is written after, and a
        // write of it that was lost is repaired below.
        let mode = roots.mode(&doc.transact());
        let source = roots.source(&doc.transact(), mode);
        let inactive = roots.source(&doc.transact(), other(mode));
        // A database deleted while this loaded: its drafts are deleted too.
        let database_deleted = hub.attach(loaded.database_id, &shared);
        let status = match loaded.status {
            _ if database_deleted || loaded.database_deleted => LiveStatus::Deleted,
            DraftStatus::Open => LiveStatus::Open,
            DraftStatus::Published => LiveStatus::Published,
            DraftStatus::Deleted => LiveStatus::Deleted,
        };
        let _ = shared.org_id.set(loaded.org_id);
        let doc_bytes = loaded.updates.iter().map(Vec::len).sum();
        let (failed, failures) = mpsc::unbounded_channel();

        let sha256 = sha256_hex(&source);
        let input = compile::input_hash(mode, &base_sql, &source);
        let mut room = Self {
            id,
            settings: hub.settings.clone(),
            compiler: hub.compiler.clone(),
            writer: Writer::spawn(hub.db.clone(), id, loaded.last_update_id, failed),
            failures,
            save_failed: false,
            doc_bytes,
            deferred_renames: Vec::new(),
            update: None,
            hub,
            shared,
            doc,
            roots,
            status,
            mode,
            base_version,
            base_sql,
            major: loaded.pg_major,
            inactive,
            touched_sql: Arc::new(AtomicBool::new(false)),
            touched_migration: Arc::new(AtomicBool::new(false)),
            comment_changes: Arc::default(),
            op_budgets: HashMap::new(),
            conns: HashMap::new(),
            clients: HashMap::new(),
            owners: Owners::default(),
            waiting,
            pending: Vec::new(),
            flush_at: None,
            rows_since_snapshot: loaded.rows_since_snapshot,
            source,
            dirty: None,
            retry_at: None,
            results: mpsc::unbounded_channel(),
            last_compile: None,
            good_model: None,
            good_tables: HashMap::new(),
            review: loaded.review,
            review_shown: None,
            schema_sql: None,
            idle_since: Some(Instant::now()),
        };
        room.share_review(false);
        if let Some(saved) = loaded.compile {
            room.restore_compile(saved, sha256, input);
        }
        if room.last_compile.is_none() && room.status == LiveStatus::Open {
            let now = Instant::now();
            room.dirty = Some((now, now));
        }
        // In migration mode the derived SQL comes with the next compile
        // (none is stored): whoever joins before it sees the model.
        if room.mode == DraftMode::Migration && room.last_compile.is_some() {
            room.last_compile = None;
            let now = Instant::now();
            room.dirty = Some((now, now));
        }
        room.repair_mode(loaded.mode);
        room.watch_texts();
        if room.rows_since_snapshot >= room.settings.compact_after {
            room.compact();
        }
        Ok(room)
    }

    /// The last compile as stored: the model the graph shows, and the result
    /// itself when it is of the source as loaded (`input`).
    fn restore_compile(&mut self, saved: store::SavedCompile, sha256: String, input: String) {
        self.good_model = saved.good_model.map(Arc::new);
        self.good_tables = saved
            .good_tables
            .and_then(|tables| serde_json::from_value(tables).ok())
            .unwrap_or_default();
        // Of this text on another major (the database moved since): the
        // graph keeps its model until the compile on this one.
        if hex(&saved.source_hash) == input && saved.pg_major == self.major {
            self.last_compile = Some(LastCompile {
                state: if saved.model.is_some() {
                    CompileState::Ok
                } else {
                    CompileState::Failed
                },
                sha256: Some(sha256),
                input: Some(input),
                diagnostics: saved.errors.unwrap_or_else(|| Value::Array(Vec::new())),
            });
        }
    }

    /// `drafts.mode` said `column` but the doc says otherwise: a write of
    /// it after a switch was lost. The doc is right (only rooms write its
    /// mode, in the switch's transaction).
    fn repair_mode(&mut self, column: DraftMode) {
        if column == self.mode {
            return;
        }
        tracing::warn!(draft = %self.id, doc = self.mode.as_str(), column = column.as_str(), "collab: drafts.mode lagged the doc; repaired");
        self.writer.send(Job::Mode {
            mode: self.mode,
            base_sql: BaseSql::Keep,
        });
    }

    /// Notes each change of a text, for [`Room::apply`] to tell an edit of
    /// the one that is not the source, and of the threads and comments, for
    /// [`Room::check_comments`].
    fn watch_texts(&self) {
        for (text, flag) in [
            (&self.roots.sql, &self.touched_sql),
            (&self.roots.migration, &self.touched_migration),
        ] {
            let flag = Arc::clone(flag);
            text.observe("room", move |_, _| flag.store(true, Ordering::Relaxed));
        }
        comments::observe(
            &self.roots.threads,
            &self.roots.comments,
            &self.comment_changes,
        );
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
            Command::ThreadExists { thread, reply } => drop(reply.send(self.has_thread(thread))),
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
            Command::Renamed { name } => self.broadcast_event(&ServerEvent::Renamed { name }, None),
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
                base,
            } => {
                if let Some((id, sql)) = base {
                    self.rebase(id, sql);
                }
                let updated = ServerEvent::Updated {
                    version,
                    by,
                    conflicts,
                };
                self.broadcast_event(&updated, None);
            }
            Command::VersionCreated { number } => self.version_created(number),
            Command::Major { major, by } => self.major_changed(major, by),
            Command::Event(event) => self.broadcast_event(&event, None),
            Command::CheckApproval {
                user_id,
                sha256,
                reply,
            } => {
                let _ = reply.send(self.check_approval(user_id, &sha256));
            }
            Command::Review(change) => self.review_changed(change),
            Command::Rewrite {
                from,
                changes,
                reason,
                ticket,
                merged,
                reply,
            } => {
                let _ = reply.send(self.rewrite(&from, &changes, reason, ticket, merged));
            }
            // Handled by `Room::command`.
            Command::Shutdown | Command::MergeLayout { .. } | Command::SwitchMode { .. } => {}
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
        let tabs = self
            .conns
            .values()
            .filter(|c| c.peer.user_id == peer.user_id)
            .count();
        if tabs >= MAX_TABS {
            tracing::info!(draft = %self.id, user = %peer.user_id, "collab: too many tabs");
            out.close(
                close::TOO_MANY,
                "the draft is open in too many of your tabs",
            );
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
        first.push(protocol::event(
            &self.review.event(&self.sha256_for_review()),
        ));
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
            Incoming::Update(bytes, update) => self.apply(conn, &bytes, update),
            Incoming::Awareness(update) => self.awareness(conn, update),
            Incoming::AwarenessQuery => {
                if let Some(everyone) = self.awareness_of(self.clients.keys().copied()) {
                    self.send(conn, everyone);
                }
            }
            Incoming::Comment(request) => self.comment(conn, request),
            Incoming::CommentRefused { id } => {
                let event = ServerEvent::CommentResult {
                    id,
                    ok: false,
                    code: Some("rate_limited".to_owned()),
                    message: Some(
                        "That is a lot of comments at once. Wait a moment, then send it again."
                            .to_owned(),
                    ),
                };
                self.send(conn, protocol::event(&event));
            }
        }
    }

    fn apply(&mut self, conn: ConnId, bytes: &Bytes, update: Update) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        let user_id = c.peer.user_id;
        if !c.peer.can_edit() {
            // Reviewers comment through operations (`Incoming::Comment`):
            // their updates could carry anything, other people's client ids
            // included, and are never applied.
            tracing::debug!(draft = %self.id, user = %user_id, "collab: dropped an update from a reviewer");
            return;
        }
        if self.status != LiveStatus::Open {
            // Brought back by the sync that follows an unfreeze.
            return;
        }
        if update.is_empty() {
            return;
        }
        let before = self.doc.transact().state_vector();
        let extended = extended_clients(&update, &before);
        if self.refused_for_clients(conn, user_id, &extended) {
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
        let touched = Arc::clone(match self.mode {
            DraftMode::Schema => &self.touched_migration,
            DraftMode::Migration => &self.touched_sql,
        });
        touched.store(false, Ordering::Relaxed);
        self.take_comment_changes();
        let Some((waited, applied)) = self.integrate(update, bytes, &before) else {
            return;
        };
        // A tab's own id: the one its edits extend alone, or one its
        // connection announced, now extended.
        if let [client] = extended[..]
            && self.owners.owner(client).is_none()
        {
            self.take_over(client, conn, user_id);
        }
        let announced = self
            .conns
            .get(&conn)
            .map(|c| c.clients.clone())
            .unwrap_or_default();
        for &client in &extended {
            if self.owners.owner(client).is_none() && announced.contains(&client) {
                self.owners.bind(client, user_id);
            }
        }
        // Whose edits these are: this person's ids, or (alone) one nobody
        // holds; anything else was carried for someone.
        let carried_others = extended
            .iter()
            .any(|&client| match self.owners.owner(client) {
                Some(person) => person != user_id,
                None => extended.len() > 1,
            });
        self.doc_bytes += applied.len();
        self.queue_write(applied.clone());
        // What the threads and comments may not hold is written back, and
        // goes out with the update: nobody sees it as the client wrote it.
        match self.check_comments((!carried_others).then_some(conn)) {
            None => self.broadcast(&protocol::update(applied.to_vec()), Some(conn)),
            Some(fix) => {
                if let Ok(merged) = yrs::merge_updates_v1([&applied[..], &fix[..]]) {
                    self.broadcast(&protocol::update(merged), Some(conn));
                } else {
                    self.broadcast(&protocol::update(applied.to_vec()), Some(conn));
                    self.broadcast(&protocol::update(fix.clone()), Some(conn));
                }
                self.send(conn, protocol::update(fix));
            }
        }
        if touched.load(Ordering::Relaxed) {
            self.undo_inactive_edit(conn);
        }
        // `meta.mode` is the server's: a client changing it is overruled.
        if self.roots.mode(&self.doc.transact()) != self.mode {
            let mode = self.mode;
            self.server_edit(|roots, txn| roots.set_mode(txn, mode));
        }
        if self.source_may_have_changed() {
            self.edited_by(user_id);
        }
        self.retry_waiting();
        if waited {
            // A tab typing before its step 2 (after a reconnect): its step 2
            // brings what waited with what it waited on.
            tracing::info!(draft = %self.id, conn, "collab: an update waited on structs the doc lacks; asked for a step 2");
            let sv = self.doc.transact().state_vector();
            self.send(conn, protocol::sync_step1(&sv));
        }
    }

    /// Refuses (and says so) an update extending a client id that is not
    /// the connection's to write with: the room's own or one bound to
    /// another person (4409, reload), or, among several, one another
    /// editor's open tab announced (4003, sync again). Whether it did.
    fn refused_for_clients(&mut self, conn: ConnId, user_id: Uuid, extended: &[u64]) -> bool {
        let own = self.doc.client_id().get();
        if let Some(client) = extended.iter().copied().find(|&client| {
            client == own || self.owners.owner(client).is_some_and(|p| p != user_id)
        }) {
            tracing::warn!(draft = %self.id, conn, client, "collab: refused an update written with another person's client id");
            self.out_of_step(conn, "this tab wrote as someone else: reload");
            return true;
        }
        // Several ids, one of them another editor's open tab announced: a
        // relay racing that tab's own step 2 (or a forgery). One id alone
        // is that id's tab writing: it takes it over, after applying.
        if let Some(client) = extended
            .iter()
            .copied()
            .find(|&client| extended.len() > 1 && self.held_by_another(client, user_id))
        {
            tracing::info!(draft = %self.id, conn, client, "collab: an update carries a client id another person's tab announced; sync again");
            if let Some(c) = self.conns.get(&conn) {
                c.out
                    .close(close::RESYNC, "another tab writes with this id: sync again");
            }
            self.leave(conn);
            return true;
        }
        false
    }

    /// Applies an update: whether some of it waited on structs the doc
    /// lacks (dropped), and what applied (the update, or what it added).
    fn integrate(
        &mut self,
        update: Update,
        bytes: &Bytes,
        before: &StateVector,
    ) -> Option<(bool, Bytes)> {
        let mut txn = self.doc.transact_mut();
        if let Err(err) = txn.apply_update(update) {
            tracing::warn!(draft = %self.id, error = %err, "collab: an update does not apply");
            return None;
        }
        // Structs waiting on others the doc lacks would land later, in
        // whoever's transaction brings those: dropped, and the tab asked for
        // them again with what they wait on.
        let waited = yrs::WriteTxn::prune_pending(&mut txn).is_some();
        let applied: Bytes = if waited {
            txn.encode_state_as_update_v1(before).into()
        } else {
            bytes.clone()
        };
        Some((waited, applied))
    }

    /// Tries the stored updates that wait again, once the doc has some of
    /// what they wait on: in a transaction of their own, sent to everyone,
    /// their comment changes taken back (whose they were is unknown).
    fn retry_waiting(&mut self) {
        let before = self.doc.transact().state_vector();
        if !self
            .waiting
            .as_ref()
            .is_some_and(|waiting| reached(&before, &waiting.missing))
        {
            return;
        }
        let Some(waiting) = self.waiting.take() else {
            return;
        };
        let Ok(update) = Update::decode_v1(&waiting.bytes) else {
            return;
        };
        self.take_comment_changes();
        {
            let mut txn = self.doc.transact_mut();
            if let Err(err) = txn.apply_update(update) {
                tracing::warn!(draft = %self.id, error = %err, "collab: stored updates that waited do not apply");
            }
            self.waiting = waiting_of(&mut txn);
        }
        if self.doc.transact().state_vector() == before {
            return;
        }
        tracing::info!(draft = %self.id, still = self.waiting.is_some(), "collab: stored updates that waited landed");
        let landed = self.doc.transact().encode_state_as_update_v1(&before);
        let message = match self.check_comments(None) {
            None => landed,
            Some(fix) => yrs::merge_updates_v1([&landed[..], &fix[..]]).unwrap_or(landed),
        };
        self.broadcast(&protocol::update(message), None);
    }

    /// Whether another editor's connection announced `client` (its tab's
    /// own id, until its first edit binds it). Reviewers never hold an id:
    /// they never write with one.
    fn held_by_another(&self, client: u64, user: Uuid) -> bool {
        self.clients
            .get(&client)
            .and_then(|state| self.conns.get(&state.conn))
            .is_some_and(|c| c.peer.user_id != user && c.peer.can_edit())
    }

    /// `client` is the tab's that just wrote with it alone: a connection of
    /// someone else that only announced it loses it (its cursor goes; an
    /// editor's is closed 4409, having never written with it), and the id
    /// is bound to `user`.
    fn take_over(&mut self, client: u64, conn: ConnId, user: Uuid) {
        if let Some(state) = self.clients.get(&client)
            && state.conn != conn
            && let Some(holder) = self
                .conns
                .get(&state.conn)
                .map(|c| (state.conn, c.peer.user_id, c.peer.can_edit()))
            && holder.1 != user
        {
            let (held_by, _, editor) = holder;
            tracing::warn!(draft = %self.id, conn = held_by, client, "collab: a connection announced a client id another tab writes with; it loses it");
            let clock = state.clock.wrapping_add(1);
            self.clients.remove(&client);
            if let Some(c) = self.conns.get_mut(&held_by) {
                c.clients.remove(&client);
            }
            self.broadcast(
                &protocol::awareness(AwarenessUpdate {
                    clients: HashMap::from([(
                        yrs::block::ClientID::new(client),
                        AwarenessUpdateEntry {
                            clock,
                            json: "null".into(),
                        },
                    )]),
                }),
                None,
            );
            if editor {
                self.out_of_step(held_by, "another tab writes with this tab's id: reload");
            }
        }
        self.owners.bind(client, user);
    }

    /// Lets a connection go that wrote with someone else's client id: told
    /// (4409, final), never silently ignored.
    fn out_of_step(&mut self, conn: ConnId, reason: &'static str) {
        if let Some(c) = self.conns.get(&conn) {
            c.out.close(close::OUT_OF_STEP, reason);
        }
        self.leave(conn);
    }

    /// A comment operation: written by the room as a server edit, with the
    /// connection's person as its author, if they may; answered either way.
    fn comment(&mut self, conn: ConnId, request: Request) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        let (user, name, can_edit) = (c.peer.user_id, c.peer.name.clone(), c.peer.can_edit());
        let result = self.plan_comment(&request, user, &name, can_edit);
        let event = match result {
            Ok(planned) => {
                let fix = &planned.fix;
                if !fix.is_empty()
                    && let Some(update) = self.server_edit_quiet(|roots, txn| {
                        fix.apply(&roots.threads, &roots.comments, txn)
                    })
                {
                    self.take_comment_changes();
                    self.broadcast(&protocol::update(update), None);
                }
                if let Some(moderation) = planned.moderation {
                    self.audit_deletion(user, &name, moderation);
                }
                ServerEvent::CommentResult {
                    id: request.id,
                    ok: true,
                    code: None,
                    message: None,
                }
            }
            Err(refusal) => ServerEvent::CommentResult {
                id: request.id,
                ok: false,
                code: Some(refusal.code.to_owned()),
                message: Some(refusal.message),
            },
        };
        self.send(conn, protocol::event(&event));
    }

    /// Someone else's comments a developer or an admin deleted: in the
    /// audit log (written by this room's writer).
    fn audit_deletion(&self, user: Uuid, name: &str, moderation: comments::Moderation) {
        let (Some(&org_id), Some(&database_id)) =
            (self.shared.org_id.get(), self.shared.database_id.get())
        else {
            return;
        };
        self.writer.send(Job::Audit(Box::new(audit::Entry {
            org_id,
            database_id: Some(database_id),
            action: AuditAction::CommentDeleted,
            actor_id: Some(user),
            actor_name: Some(name.to_owned()),
            version_number: None,
            reason: None,
            details: audit::AuditDetails {
                draft_id: Some(self.id),
                thread_id: Some(moderation.thread),
                comments: Some(i32::try_from(moderation.comments).unwrap_or(i32::MAX)),
                authors: Some(moderation.authors),
                excerpt: Some(moderation.excerpt),
                ..audit::AuditDetails::default()
            },
        })));
    }

    fn plan_comment(
        &mut self,
        request: &Request,
        user: Uuid,
        name: &str,
        can_edit: bool,
    ) -> Result<comments::Planned, comments::Refusal> {
        let refusal = |code, message: &str| comments::Refusal {
            code,
            message: message.to_owned(),
        };
        match self.status {
            LiveStatus::Open => {}
            LiveStatus::Publishing | LiveStatus::Updating => {
                return Err(refusal(
                    "closed",
                    "The draft is being published or updated. Send it again in a moment.",
                ));
            }
            LiveStatus::Published | LiveStatus::Deleted => {
                return Err(refusal("closed", "This draft is closed."));
            }
        }
        if !self
            .op_budgets
            .entry(user)
            .or_insert_with(Budget::new)
            .take()
        {
            return Err(refusal(
                "rate_limited",
                "That is a lot of comments at once. Wait a moment, then send it again.",
            ));
        }
        // A comment is a few kilobytes at most: room for it, or none.
        if self.doc_bytes + 64 * 1024 > self.settings.max_doc_bytes {
            self.doc_bytes = self.state().len();
            if self.doc_bytes + 64 * 1024 > self.settings.max_doc_bytes {
                return Err(refusal(
                    "too_large",
                    "This draft is as large as a draft may be: delete some comments first.",
                ));
            }
        }
        let txn = self.doc.transact();
        let threads = comments::read_map(&self.roots.threads, &txn);
        let all = comments::read_map(&self.roots.comments, &txn);
        drop(txn);
        comments::plan(
            &request.op,
            &comments::Writer {
                id: user,
                name,
                can_edit,
            },
            chrono::Utc::now(),
            &comments::State {
                threads: &threads,
                comments: &all,
            },
        )
    }

    /// Forgets the changes to the threads and comments noted so far (the
    /// server's own edits).
    fn take_comment_changes(&self) -> Changes {
        std::mem::take(
            &mut *self
                .comment_changes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Checks what the connection's update changed in the threads and
    /// comments (`comments::check`) and writes back what it may not have
    /// (stored, not sent): the fix's update, if there is one. Without a
    /// `writer` (an update carrying others' structs, stored updates that
    /// waited), none of the changes stays: they could be anyone's, and
    /// would be stamped with whoever brought them.
    fn check_comments(&mut self, writer: Option<ConnId>) -> Option<Vec<u8>> {
        let changes = self.take_comment_changes();
        // List content (keyless items) is no key change: looked for apart.
        let stray = comments::stray_content(&self.roots.threads)
            || comments::stray_content(&self.roots.comments);
        if changes.is_empty() && !stray {
            return None;
        }
        let writer = match writer {
            Some(conn) => Some(self.conns.get(&conn)?),
            None => None,
        };
        let fix = {
            let txn = self.doc.transact();
            let threads = comments::read_map(&self.roots.threads, &txn);
            let all = comments::read_map(&self.roots.comments, &txn);
            if let Some(c) = writer {
                comments::check(
                    &changes,
                    &comments::Writer {
                        id: c.peer.user_id,
                        name: &c.peer.name,
                        can_edit: c.peer.can_edit(),
                    },
                    chrono::Utc::now(),
                    &comments::State {
                        threads: &threads,
                        comments: &all,
                    },
                )
            } else {
                comments::revert(&changes)
            }
        };
        if fix.is_empty() && !stray {
            return None;
        }
        tracing::info!(draft = %self.id, threads = fix.threads.len(), comments = fix.comments.len(), stray, "collab: wrote back comment changes a client may not make");
        let update = self.server_edit_quiet(|roots, txn| {
            let mut wrote = fix.apply(&roots.threads, &roots.comments, txn);
            wrote |= comments::remove_stray(&roots.threads, txn);
            wrote |= comments::remove_stray(&roots.comments, txn);
            wrote
        });
        self.take_comment_changes();
        update
    }

    /// A change the server makes itself (moving a renamed table's entries,
    /// a rewrite), sent to everyone and stored like any other.
    fn server_edit(&mut self, edit: impl FnOnce(&Roots, &mut TransactionMut) -> bool) {
        if let Some(update) = self.server_edit_quiet(edit) {
            self.broadcast(&protocol::update(update), None);
        }
    }

    /// [`Room::server_edit`], stored but not sent: its update, if it changed
    /// anything (the caller sends it).
    fn server_edit_quiet(
        &mut self,
        edit: impl FnOnce(&Roots, &mut TransactionMut) -> bool,
    ) -> Option<Vec<u8>> {
        let before = self.doc.transact().state_vector();
        let changed = {
            let mut txn = self.doc.transact_mut();
            edit(&self.roots, &mut txn)
        };
        if !changed {
            return None;
        }
        let update = self.doc.transact().encode_state_as_update_v1(&before);
        self.doc_bytes += update.len();
        self.queue_write(update.clone().into());
        Some(update)
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
        (self.status, self.update) = match purpose {
            FreezeFor::Publish => (LiveStatus::Publishing, None),
            FreezeFor::Update(ticket) => (LiveStatus::Updating, Some(ticket)),
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
        self.update = None;
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
        ticket: Option<UpdateTicket>,
        merged: Option<i32>,
    ) -> Result<bool, RewriteError> {
        // Open (anyone), or frozen for the update that shows its ticket.
        let allowed = match (self.status, ticket) {
            (LiveStatus::Open, None) => true,
            (LiveStatus::Updating, Some(ticket)) => self.update == Some(ticket),
            _ => false,
        };
        if !allowed {
            return Err(RewriteError::NotOpen(self.status));
        }
        if self.mode != DraftMode::Schema {
            return Err(RewriteError::Mode(self.mode));
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
                if let Some(version) = merged {
                    roots.note_merged(txn, version);
                }
            }
            edited
        });
        if self.source_may_have_changed() {
            self.share_review(false);
        }
        Ok(edited)
    }

    /// A client's update changed the text that is not the source (a tab
    /// that had not seen the switch yet): undone for everyone, and that
    /// client told.
    fn undo_inactive_edit(&mut self, conn: ConnId) {
        let text = other(self.mode);
        let current = self.roots.source(&self.doc.transact(), text);
        if current == self.inactive {
            return;
        }
        tracing::info!(draft = %self.id, conn, "collab: undid an edit of the text that is not the source");
        let changes = patch::changes(&current, &self.inactive);
        self.server_edit(|roots, txn| {
            patch::apply(roots.source_text(text), txn, &current, &changes)
        });
        self.send(
            conn,
            protocol::event(&ServerEvent::StaleEdit { mode: self.mode }),
        );
    }

    /// Switches modes (see [`Command::SwitchMode`]); the caller flushes.
    fn switch_mode(
        &mut self,
        to: DraftMode,
        (from, base): (&str, Option<Uuid>),
        seed: &str,
        base_sql: BaseSql,
        by: String,
    ) -> Result<bool, RewriteError> {
        if self.status != LiveStatus::Open {
            return Err(RewriteError::NotOpen(self.status));
        }
        if self.mode == to {
            return Ok(false);
        }
        let current = self.roots.source(&self.doc.transact(), self.mode);
        if sha256_hex(&current) != from || self.base_version != base {
            return Err(RewriteError::Changed);
        }
        match to {
            DraftMode::Migration => self.server_edit(|roots, txn| {
                Roots::replace(&roots.migration, txn, seed);
                roots.set_mode(txn, to);
                true
            }),
            DraftMode::Schema => {
                // The SQL as it was before migration mode, rewritten as the
                // smallest edit into the schema the migration made.
                let sql = self.roots.sql(&self.doc.transact());
                let changes = patch::changes(&sql, seed);
                self.server_edit(|roots, txn| {
                    patch::apply(&roots.sql, txn, &sql, &changes);
                    Roots::replace(&roots.migration, txn, "");
                    roots.set_mode(txn, to);
                    true
                });
                self.schema_sql = None;
            }
        }
        tracing::info!(draft = %self.id, mode = to.as_str(), "collab: switched modes");
        self.mode = to;
        self.source = self.roots.source(&self.doc.transact(), to);
        self.inactive = self.roots.source(&self.doc.transact(), other(to));
        // The doc first, then the column: a lost column write is undone at
        // the next load (the doc follows the column).
        self.flush_pending();
        self.writer.send(Job::Mode { mode: to, base_sql });
        self.last_compile = None;
        self.deferred_renames.clear();
        let now = Instant::now();
        self.dirty = Some((now, now));
        self.broadcast_event(&ServerEvent::Mode { mode: to, by }, None);
        // Approvals were of the other text.
        self.share_review(false);
        Ok(true)
    }

    /// The draft's base moved ("Update draft"): a migration applies to the
    /// new one, compiled again.
    fn rebase(&mut self, id: Uuid, base: Arc<str>) {
        self.base_version = Some(id);
        if *base == *self.base_sql {
            return;
        }
        self.base_sql = base;
        if self.mode == DraftMode::Migration {
            self.last_compile = None;
            let now = Instant::now();
            self.dirty = Some((now, now));
            // Approvals were of the migration on the old base.
            self.share_review(false);
        }
    }

    fn has_thread(&self, thread: Uuid) -> bool {
        let txn = self.doc.transact();
        yrs::Map::get(&self.roots.threads, &txn, &thread.to_string()).is_some()
    }

    fn content(&self) -> Content {
        let txn = self.doc.transact();
        let source = self.roots.source(&txn, self.mode);
        let input = compile::input_hash(self.mode, &self.base_sql, &source);
        let compiled_now = self.last_compile.as_ref().is_some_and(|last| {
            last.state == CompileState::Ok && last.input.as_deref() == Some(&input)
        });
        Content {
            mode: self.mode,
            layout: self.roots.layout(&txn),
            model: compiled_now.then(|| self.good_model.clone()).flatten(),
            merged: self.roots.merged(&txn),
            approved_by: self.review.approved_by(&input),
            discussion: self.roots.discussion(&txn),
            sql: self.roots.sql(&txn),
            migration: self.roots.migration(&txn),
            base_version: self.base_version,
            base_sql: Arc::clone(&self.base_sql),
            major: self.major,
        }
    }

    // ---- Review ---------------------------------------------------------------

    /// The hash approvals are compared with: what is reviewed (the source;
    /// in migration mode on its base, `compile::input_hash`), when there is
    /// any approval to compare (hashing a large text for nothing on every
    /// keystroke is not free).
    fn sha256_for_review(&self) -> String {
        if self.review.approvals.is_empty() {
            String::new()
        } else {
            compile::input_hash(self.mode, &self.base_sql, &self.source)
        }
    }

    /// Someone's update changed the SQL: they are an editor now (written
    /// once), and the approvals may no longer count.
    fn edited_by(&mut self, user_id: Uuid) {
        let new = self.review.editors.insert(user_id);
        if new {
            self.writer.send(Job::Editor(user_id));
        }
        self.share_review(new);
    }

    /// Tells everyone the review when what counts changed (or `always`),
    /// and the hub how many approvals count.
    fn share_review(&mut self, always: bool) {
        let sha256 = self.sha256_for_review();
        let approved = self.review.approved_by(&sha256);
        *self
            .shared
            .approvals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(approved.clone());
        if !always && self.review_shown.as_ref() == Some(&approved) {
            return;
        }
        // Nobody to tell yet: whoever joins gets it.
        let first = self.review_shown.is_none();
        self.review_shown = Some(approved);
        if !first || always {
            self.broadcast_event(&self.review.event(&sha256), None);
        }
    }

    fn review_changed(&mut self, change: ReviewChange) {
        match change {
            ReviewChange::Approved(approval) => self.review.approve(approval),
            ReviewChange::Withdrawn(user_id) => self.review.withdraw(user_id),
            ReviewChange::Requested(requested) => self.review.requested = requested,
            ReviewChange::Required(required) => self.review.required = required,
        }
        self.share_review(true);
    }

    /// Whether `user_id` may approve the source as it is now, the text
    /// hashing to `sha256` (what the person saw); with what their approval
    /// is then of: that text on its base (`compile::input_hash`), so that
    /// in migration mode an "Update draft" outdates it.
    fn check_approval(&self, user_id: Uuid, sha256: &str) -> Result<String, ApprovalError> {
        if self.status != LiveStatus::Open {
            return Err(ApprovalError::NotOpen);
        }
        if self.review.editors.contains(&user_id) {
            return Err(ApprovalError::Editor);
        }
        let current = self.roots.source(&self.doc.transact(), self.mode);
        if sha256_hex(&current) != sha256 {
            return Err(ApprovalError::Changed);
        }
        Ok(compile::input_hash(self.mode, &self.base_sql, &current))
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
            if known.is_some_and(|state| state.conn != conn)
                || self
                    .owners
                    .owner(client)
                    .is_some_and(|person| person != c.peer.user_id)
            {
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
    /// The doc's whole state, with the stored updates that still wait.
    fn state(&self) -> Vec<u8> {
        let state = self
            .doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        match &self.waiting {
            Some(waiting) => {
                yrs::merge_updates_v1([&state[..], &waiting.bytes[..]]).unwrap_or(state)
            }
            None => state,
        }
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

    /// Whether it did.
    fn source_may_have_changed(&mut self) -> bool {
        let source = self.roots.source(&self.doc.transact(), self.mode);
        if source == self.source {
            return false;
        }
        self.source = source;
        let now = Instant::now();
        self.dirty = Some(self.dirty.map_or((now, now), |(first, _)| (first, now)));
        true
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
        let (compiler, key, source) = (
            self.compiler.clone(),
            self.id.to_string(),
            self.source.clone(),
        );
        let (mode, base, major) = (self.mode, Arc::clone(&self.base_sql), self.major);
        let results = self.results.0.clone();
        tokio::spawn(async move {
            let sha256 = sha256_hex(&source);
            let input = compile::input_hash(mode, &base, &source);
            // A `---` line typed and never posted: the SQL never keeps one.
            let unposted = comments::comment_line_diagnostics(&source, &base);
            let (tables, result) = match mode {
                _ if !unposted.is_empty() => (
                    HashMap::new(),
                    Ok(Compiled {
                        model: None,
                        diagnostics: unposted,
                    }),
                ),
                DraftMode::Schema => (
                    compile::created_at(&source),
                    compiler.compile_for(major, &key, source).await,
                ),
                // Renames typed in a migration are `ALTER … RENAME`: the
                // statement-index rule does not apply.
                DraftMode::Migration => (
                    HashMap::new(),
                    compiler
                        .compile_migration_for(major, &key, &base, &source)
                        .await,
                ),
            };
            let schema_sql = match (&result, mode) {
                (
                    Ok(Compiled {
                        model: Some(model), ..
                    }),
                    DraftMode::Migration,
                ) => {
                    let model = model.clone();
                    tokio::task::spawn_blocking(move || canonical_sql(&model))
                        .await
                        .ok()
                        .map(Arc::from)
                }
                _ => None,
            };
            let _ = results.send(CompileDone {
                mode,
                major,
                sha256,
                input,
                schema_sql,
                tables,
                result,
            });
        });
    }

    /// Removes the graph edits markers after the first (see the module
    /// documentation). Not a rewrite: the characters removed are markers.
    fn collapse_markers(&mut self) {
        let extra = doc::extra_markers(&self.source);
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
        let (current, mode) = (self.source.clone(), self.mode);
        self.server_edit(|roots, txn| {
            patch::apply(roots.source_text(mode), txn, &current, &changes)
        });
        self.source = self.roots.source(&self.doc.transact(), mode);
        self.share_review(false);
    }

    fn compiled(&mut self, done: CompileDone) {
        let CompileDone {
            mode,
            major,
            sha256,
            input,
            schema_sql,
            tables,
            result,
        } = done;
        // Of the other text (the draft switched modes since it started), or
        // on another major (the database moved: a compile on it follows).
        if mode != self.mode || major != self.major {
            return;
        }
        let compiled = match result {
            Ok(compiled) => compiled,
            Err(Rejected::Superseded) => return,
            Err(Rejected::Api(err)) => {
                tracing::warn!(draft = %self.id, reason = %err, "collab: cannot compile");
                self.compiler_unavailable();
                return;
            }
        };
        let diagnostics = serde_json::to_value(&compiled.diagnostics).unwrap_or_default();
        let source_hash = unhex(&input);
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
            self.schema_sql.clone_from(&schema_sql);
            self.writer.send(Job::Compile(Box::new(CompileRow {
                source_hash,
                pg_major: major,
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
                    schema_sql,
                },
                None,
            );
            self.last_compile = Some(LastCompile {
                state: CompileState::Ok,
                sha256: Some(sha256),
                input: Some(input),
                diagnostics,
            });
        } else {
            self.writer.send(Job::Compile(Box::new(CompileRow {
                source_hash,
                pg_major: major,
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
                    schema_sql: None,
                },
                None,
            );
            self.last_compile = Some(LastCompile {
                state: CompileState::Failed,
                sha256: Some(sha256),
                input: Some(input),
                diagnostics,
            });
        }
    }

    fn version_created(&mut self, number: i32) {
        self.broadcast_event(&ServerEvent::VersionCreated { number }, None);
    }

    /// The database moved to another major: everyone hears it, and the
    /// source compiles on it (the result on the old one is not of this
    /// draft any more; the graph keeps its model meanwhile).
    fn major_changed(&mut self, major: PgMajor, by: String) {
        if major == self.major {
            return;
        }
        self.major = major;
        self.last_compile = None;
        self.broadcast_event(
            &ServerEvent::Major {
                pg_major: major,
                by,
            },
            None,
        );
        if self.status == LiveStatus::Open
            || self.status == LiveStatus::Publishing
            || self.status == LiveStatus::Updating
        {
            let now = Instant::now();
            self.dirty = Some((now, now));
        }
    }

    /// The compiler could not compile: everyone hears it, and it is tried
    /// again in a while.
    fn compiler_unavailable(&mut self) {
        self.retry_at = Some(Instant::now() + self.settings.retry_unavailable);
        self.last_compile = Some(LastCompile {
            state: CompileState::Unavailable,
            sha256: None,
            input: None,
            diagnostics: Value::Array(Vec::new()),
        });
        self.broadcast_event(
            &ServerEvent::Compile {
                state: CompileState::Unavailable,
                sha256: None,
                diagnostics: Vec::new(),
                model: None,
                schema_sql: None,
            },
            None,
        );
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
            "schema_sql": self.schema_sql.as_deref().filter(|_| self.mode == DraftMode::Migration),
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

/// The mode whose text is the other one.
fn other(mode: DraftMode) -> DraftMode {
    match mode {
        DraftMode::Schema => DraftMode::Migration,
        DraftMode::Migration => DraftMode::Schema,
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

    #[test]
    fn what_waits_is_tried_again_only_once_the_doc_has_what_it_waits_on() {
        let missing = vec![(7, 5)];
        let mut sv = StateVector::default();
        assert!(!reached(&sv, &missing));
        sv.set_max(yrs::block::ClientID::new(7), 5);
        assert!(!reached(&sv, &missing), "clock 5 is not there yet");
        sv.set_max(yrs::block::ClientID::new(8), 100);
        assert!(
            !reached(&sv, &missing),
            "another client's clocks do not count"
        );
        sv.set_max(yrs::block::ClientID::new(7), 6);
        assert!(reached(&sv, &missing));
    }

    #[test]
    fn a_client_id_is_its_first_writer_s_and_a_person_keeps_so_many() {
        let (alice, bob) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut owners = Owners::default();
        owners.bind(1, alice);
        owners.bind(1, bob);
        assert_eq!(owners.owner(1), Some(alice), "never taken over");
        for client in 2..=20 {
            owners.bind(client, alice);
        }
        assert_eq!(owners.by_client.len(), MAX_TABS);
        assert_eq!(owners.owner(1), None, "her oldest is forgotten");
        assert_eq!(owners.owner(20), Some(alice));
    }

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
