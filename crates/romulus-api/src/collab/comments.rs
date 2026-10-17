//! Comment threads in a draft's doc (docs/MVP.md "Review round 1", item 6):
//! Figma-like threads pinned to a table, a column, a point of the graph or a
//! statement, with replies, @mentions and resolve.
//!
//! ```text
//! threads:  Y.Map<thread id, {anchor, by, at, resolved}>
//! comments: Y.Map<comment id, {thread, author: {id, name}, body, mentions, at, edited}>
//! ```
//!
//! Ids are UUIDs (the client picks them); values are plain JSON objects, like
//! the layout's: a reply is a key of its own, so two people replying at once
//! never overwrite each other. Times are RFC 3339 strings.
//!
//! **Collaborative data, written by the server, checked when it is not.**
//! Threads and comments are written by the room itself, for whoever asked
//! with an operation on the socket ([`Op`], [`plan`]): start a thread,
//! reply, edit or delete one's comment, resolve or reopen. The server's
//! writes carry the author from the connection, the server's time, and
//! mentions checked against the organization's members. Reviewers comment
//! this way only: their document updates are never applied (they could
//! carry anything, another client's ids included).
//!
//! Editors also write to the two maps directly, in transactions that must be
//! one with their text: a `---` line typed in the SQL becomes a thread in
//! the transaction that removes it (one undo step), and a rename from the
//! graph moves its threads with the layout. The room checks every such
//! change ([`check`]) and writes back what it may not be, in the update
//! everyone else receives ([`Fix`]):
//!
//! - authors are stamped like awareness: a new thread's `by` and a new
//!   comment's `author` are the connection's user, whatever the client
//!   wrote; a time more than [`CLOCK_SKEW`] away from the server's is the
//!   server's;
//! - a comment is edited or deleted by its author only; a thread is deleted
//!   by whoever started it, and only when no one else ever wrote in it (a
//!   transaction deleting others' replies along with it is refused whole);
//!   developers and admins delete anyone's comments and threads through
//!   operations only, and those deletions are audited ([`Moderation`]);
//! - mentions only come through operations (direct writes keep none);
//! - the maps hold keyed plain JSON values only: nested types are refused,
//!   and list content (items without a key, invisible to the map) is
//!   removed ([`stray_content`]);
//! - anyone in the draft resolves and reopens (stamped likewise); a thread
//!   moves (its anchor) by an editor (renames carry it) or its author;
//! - sizes are capped ([`MAX_BODY_CHARS`], [`MAX_THREADS`]…), and so is
//!   what the discussion weighs, in all ([`MAX_DISCUSSION_BYTES`]) and per
//!   person ([`MAX_PERSON_BYTES`]): far below what a doc may weigh, which is
//!   the SQL's, so comments never make a draft uneditable.
//!
//! Threads never reach `COMMENT ON`: database comments are schema, these
//! are review. A `---` line left in a draft's text (typed, never posted) is
//! refused by the compile ([`comment_lines`]): the SQL never keeps one.
//!
//! Anchors are qualified names (and a column): a table renamed in the text
//! carries its threads along with its layout entry (`Roots::rename`, one
//! transaction); a dropped one leaves them pointing at nothing, shown as
//! outdated rather than lost.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Duration, DurationRound, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;
use yrs::{Any, Map, MapRef, Out, ReadTxn, TransactionMut, types::ToJson};

pub const THREADS: &str = "threads";
pub const COMMENTS: &str = "comments";

/// Threads a draft holds at most.
pub const MAX_THREADS: usize = 500;
/// Comments a draft holds at most, and one thread.
pub const MAX_COMMENTS: usize = 5000;
pub const MAX_COMMENTS_PER_THREAD: usize = 200;
/// A comment's text, in characters.
pub const MAX_BODY_CHARS: usize = 4000;
/// Members mentioned in one comment.
pub const MAX_MENTIONS: usize = 20;
/// What a draft's whole discussion may weigh (its threads and comments, as
/// JSON): a sixteenth of what a doc may, the rest is the SQL's.
pub const MAX_DISCUSSION_BYTES: usize = 1024 * 1024;
/// What one person's threads and comments in a draft may weigh.
pub const MAX_PERSON_BYTES: usize = 256 * 1024;
/// A statement's head, in characters (what anchors a thread to it).
pub const MAX_STATEMENT_CHARS: usize = 300;
/// A qualified name or a column, in characters.
const MAX_NAME_CHARS: usize = 300;
/// How far a client's clock may be from the server's before its times are
/// replaced by the server's.
pub const CLOCK_SKEW: TimeDelta = TimeDelta::minutes(10);

/// What a thread is pinned to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Anchor {
    /// A table (or view): `schema.name`.
    Table {
        table: String,
        /// The head of the statement it was written above, when that is not
        /// the table's `CREATE` (an index, a comment…): where the SQL pane
        /// shows it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        statement: Option<String>,
    },
    Column {
        table: String,
        column: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        statement: Option<String>,
    },
    /// A point of the graph's canvas, in graph coordinates.
    Point { x: f64, y: f64 },
    /// A statement about no table (a type, an extension…), by its head.
    Statement { statement: String },
}

impl Anchor {
    /// The anchor as it may be kept: names and statements within bounds,
    /// points finite and whole; `None` when it cannot be.
    pub(crate) fn normalized(&self) -> Option<Self> {
        let name = |s: &str| !s.trim().is_empty() && s.chars().count() <= MAX_NAME_CHARS;
        let statement = |s: &Option<String>| {
            s.as_ref()
                .is_none_or(|s| !s.trim().is_empty() && s.chars().count() <= MAX_STATEMENT_CHARS)
        };
        match self {
            Self::Table {
                table,
                statement: s,
            } => (name(table) && table.contains('.') && statement(s)).then(|| self.clone()),
            Self::Column {
                table,
                column,
                statement: s,
            } => (name(table) && table.contains('.') && name(column) && statement(s))
                .then(|| self.clone()),
            Self::Point { x, y } => {
                (x.is_finite() && y.is_finite() && x.abs() < 1e7 && y.abs() < 1e7).then(|| {
                    Self::Point {
                        x: x.round(),
                        y: y.round(),
                    }
                })
            }
            Self::Statement { statement: s } => statement(&Some(s.clone())).then(|| self.clone()),
        }
    }

    /// The table it is about, if any.
    pub fn table(&self) -> Option<&str> {
        match self {
            Self::Table { table, .. } | Self::Column { table, .. } => Some(table),
            Self::Point { .. } | Self::Statement { .. } => None,
        }
    }

    fn with_table(&self, new: &str) -> Self {
        let mut moved = self.clone();
        if let Self::Table { table, .. } | Self::Column { table, .. } = &mut moved {
            new.clone_into(table);
        }
        moved
    }
}

/// A thread, as the doc holds it (its comments are apart).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    pub anchor: Anchor,
    /// Who started it.
    pub by: Uuid,
    pub at: DateTime<Utc>,
    #[serde(default)]
    pub resolved: Option<Resolution>,
}

/// Who resolved a thread, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Resolution {
    pub by: Uuid,
    pub name: String,
    pub at: DateTime<Utc>,
}

/// One comment of a thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub thread: Uuid,
    pub author: Author,
    pub body: String,
    #[serde(default)]
    pub mentions: Vec<Uuid>,
    pub at: DateTime<Utc>,
    #[serde(default)]
    pub edited: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Author {
    pub id: Uuid,
    pub name: String,
}

/// A value of one of the two maps, as a change saw it.
#[derive(Debug, Clone, PartialEq)]
pub enum Slot {
    Absent,
    /// A plain JSON value (what clients should write).
    Plain(Value),
    /// A nested Yjs type: never kept.
    Nested,
}

impl Slot {
    fn of(value: Option<&Out>, txn: &impl ReadTxn) -> Self {
        match value {
            None => Self::Absent,
            Some(Out::Any(any)) => Self::Plain(any_to_value(any)),
            Some(other) => {
                let _ = other.to_json(txn);
                Self::Nested
            }
        }
    }

    fn plain(&self) -> Option<&Value> {
        match self {
            Self::Plain(value) => Some(value),
            Self::Absent | Self::Nested => None,
        }
    }
}

/// One key of `threads` or `comments` that a transaction changed: its value
/// before and after.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyChange {
    pub key: String,
    pub old: Slot,
    pub new: Slot,
}

/// The changes one transaction made to the two maps (filled by the room's
/// observers, see [`observe`]).
#[derive(Debug, Default, Clone)]
pub struct Changes {
    pub threads: Vec<KeyChange>,
    pub comments: Vec<KeyChange>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.threads.is_empty() && self.comments.is_empty()
    }
}

/// Collects into `into` the key changes of every transaction touching the
/// two maps.
pub fn observe(
    threads: &MapRef,
    comments: &MapRef,
    into: &std::sync::Arc<std::sync::Mutex<Changes>>,
) {
    use yrs::Observable;
    let collect = |which: fn(&mut Changes) -> &mut Vec<KeyChange>| {
        let into = std::sync::Arc::clone(into);
        move |txn: &TransactionMut, event: &yrs::types::map::MapEvent| {
            let mut changes = into
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (key, change) in event.keys(txn) {
                let (old, new) = match change {
                    yrs::types::EntryChange::Inserted(new) => {
                        (Slot::Absent, Slot::of(Some(new), txn))
                    }
                    yrs::types::EntryChange::Updated(old, new) => {
                        (Slot::of(Some(old), txn), Slot::of(Some(new), txn))
                    }
                    yrs::types::EntryChange::Removed(old) => {
                        (Slot::of(Some(old), txn), Slot::Absent)
                    }
                };
                which(&mut changes).push(KeyChange {
                    key: key.to_string(),
                    old,
                    new,
                });
            }
        }
    };
    threads.observe("room", collect(|c| &mut c.threads));
    comments.observe("room", collect(|c| &mut c.comments));
}

/// Who made the changes being checked.
#[derive(Debug, Clone)]
pub struct Writer<'a> {
    pub id: Uuid,
    pub name: &'a str,
    /// May edit the draft (developers and admins): moves anchors anywhere
    /// (renames carry threads).
    pub can_edit: bool,
}

/// What the room writes back after a transaction's changes were checked:
/// per map, keys to set to a value or (`None`) remove.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Fix {
    pub threads: BTreeMap<String, Option<Value>>,
    pub comments: BTreeMap<String, Option<Value>>,
}

impl Fix {
    pub fn is_empty(&self) -> bool {
        self.threads.is_empty() && self.comments.is_empty()
    }

    /// Writes it (a server edit); whether there was anything.
    pub fn apply(&self, threads: &MapRef, comments: &MapRef, txn: &mut TransactionMut) -> bool {
        for (map, entries) in [(threads, &self.threads), (comments, &self.comments)] {
            for (key, value) in entries {
                match value {
                    Some(value) => {
                        map.insert(txn, key.as_str(), value_to_any(value));
                    }
                    None => {
                        map.remove(txn, key);
                    }
                }
            }
        }
        !self.is_empty()
    }
}

/// The two maps as they are after a transaction, for the checks that look
/// beyond the keys it changed.
pub struct State<'a> {
    pub threads: &'a BTreeMap<String, Value>,
    pub comments: &'a BTreeMap<String, Value>,
}

/// Checks a transaction's changes to the threads and comments (see the
/// module documentation), by `writer`, at `now`: what must be written back.
pub fn check(changes: &Changes, writer: &Writer<'_>, now: DateTime<Utc>, state: &State<'_>) -> Fix {
    let mut checker = Checker::new(writer, millis(now), state, changes);
    for change in &changes.threads {
        checker.thread(change);
    }
    for change in &changes.comments {
        checker.comment(change);
    }
    checker.sweep(&changes.threads);
    checker.drop_empty(&changes.threads);
    checker.fix
}

/// Every change of a transaction undone: what its keys held before (the
/// comment changes of an update that carried other people's structs).
pub fn revert(changes: &Changes) -> Fix {
    let back = |list: &[KeyChange]| {
        list.iter()
            .map(|change| (change.key.clone(), change.old.plain().cloned()))
            .collect()
    };
    Fix {
        threads: back(&changes.threads),
        comments: back(&changes.comments),
    }
}

/// What the discussion weighs (its values as JSON): in all, and per person
/// (the threads they started, the comments they wrote).
#[derive(Debug, Default)]
struct Weights {
    total: usize,
    by_person: HashMap<Uuid, usize>,
}

impl Weights {
    fn measure(state: &State<'_>) -> Self {
        let mut weights = Self::default();
        for value in state.threads.values() {
            weights.add(parse::<Thread>(value).map(|t| t.by), weight(value));
        }
        for value in state.comments.values() {
            weights.add(parse::<Comment>(value).map(|c| c.author.id), weight(value));
        }
        weights
    }

    fn add(&mut self, person: Option<Uuid>, bytes: usize) {
        self.total += bytes;
        if let Some(person) = person {
            *self.by_person.entry(person).or_default() += bytes;
        }
    }

    fn of(&self, person: Uuid) -> usize {
        self.by_person.get(&person).copied().unwrap_or(0)
    }

    /// Why `bytes` more by `person` may not be written, if they may not.
    fn refuse(&self, person: Uuid, bytes: usize) -> Option<Refusal> {
        if self.of(person) + bytes > MAX_PERSON_BYTES {
            Some(refuse(
                "too_much",
                "You have written as much as one person may in this draft's discussion. Delete some of your comments to make room.",
            ))
        } else if self.total + bytes > MAX_DISCUSSION_BYTES {
            Some(refuse(
                "discussion_full",
                "This draft's discussion is as long as it may be. Delete threads that are done to make room.",
            ))
        } else {
            None
        }
    }
}

/// What a value weighs, as JSON.
fn weight(value: &Value) -> usize {
    serde_json::to_string(value).map_or(0, |json| json.len())
}

/// [`check`]'s state: what the maps hold after the transaction, and the fix
/// so far.
struct Checker<'a> {
    writer: &'a Writer<'a>,
    now: DateTime<Utc>,
    state: &'a State<'a>,
    fix: Fix,
    /// Per thread after the transaction: its comments, and whether anyone
    /// but the writer wrote one (counting those the transaction deleted: they
    /// are not the writer's to delete, and come back).
    per_thread: HashMap<Uuid, (usize, bool)>,
    /// New entries that must go, over a cap (in key order: keys are random).
    extra_threads: usize,
    extra_comments: usize,
    extra_in_thread: HashMap<Uuid, usize>,
    /// Bytes over what the discussion, and the writer's part of it, may
    /// weigh: new entries go (and grown ones shrink back) until none.
    over_total: usize,
    over_mine: usize,
}

impl<'a> Checker<'a> {
    fn new(
        writer: &'a Writer<'a>,
        now: DateTime<Utc>,
        state: &'a State<'a>,
        changes: &Changes,
    ) -> Self {
        let mut per_thread: HashMap<Uuid, (usize, bool)> = HashMap::new();
        for value in state.comments.values() {
            if let Some(comment) = parse::<Comment>(value) {
                let entry = per_thread.entry(comment.thread).or_default();
                entry.0 += 1;
                entry.1 |= comment.author.id != writer.id;
            }
        }
        for change in &changes.comments {
            let removed = matches!(change.new, Slot::Absent | Slot::Nested);
            if let (true, Some(old)) = (removed, change.old.plain().and_then(parse::<Comment>))
                && old.author.id != writer.id
            {
                per_thread.entry(old.thread).or_default().1 = true;
            }
        }
        let weights = Weights::measure(state);
        Self {
            writer,
            now,
            state,
            fix: Fix::default(),
            over_total: weights.total.saturating_sub(MAX_DISCUSSION_BYTES),
            over_mine: weights.of(writer.id).saturating_sub(MAX_PERSON_BYTES),
            extra_threads: state.threads.len().saturating_sub(MAX_THREADS),
            extra_comments: state.comments.len().saturating_sub(MAX_COMMENTS),
            extra_in_thread: per_thread
                .iter()
                .map(|(id, (count, _))| (*id, count.saturating_sub(MAX_COMMENTS_PER_THREAD)))
                .collect(),
            per_thread,
        }
    }

    fn thread(&mut self, change: &KeyChange) {
        let restore = change.old.plain().cloned();
        let Ok(id) = Uuid::parse_str(&change.key) else {
            self.fix.threads.insert(change.key.clone(), restore);
            return;
        };
        let old = change.old.plain().and_then(parse::<Thread>);
        let decision = match (&change.new, old) {
            (Slot::Absent, None) => return,
            (Slot::Absent, Some(old)) => {
                let others = self.per_thread.get(&id).is_some_and(|(_, others)| *others);
                // Theirs, and nobody else's comment left in it: it goes.
                if old.by == self.writer.id && !others {
                    return;
                }
                None
            }
            (Slot::Nested, _) => None,
            (Slot::Plain(raw), old) => match (parse::<Thread>(raw), old) {
                (None, _) => None,
                (Some(_), None) if self.extra_threads > 0 || self.over() => {
                    self.extra_threads = self.extra_threads.saturating_sub(1);
                    self.shed(weight(raw));
                    self.fix.threads.insert(change.key.clone(), None);
                    return;
                }
                (Some(new), None) => {
                    new_thread(&new, self.writer, self.now).map(|kept| (raw, kept))
                }
                (Some(new), Some(old)) => {
                    Some((raw, changed_thread(&old, &new, self.writer, self.now)))
                }
            },
        };
        decide(&mut self.fix.threads, &change.key, decision, restore);
    }

    fn comment(&mut self, change: &KeyChange) {
        let restore = change.old.plain().cloned();
        if Uuid::parse_str(&change.key).is_err() {
            self.fix.comments.insert(change.key.clone(), restore);
            return;
        }
        let old = change.old.plain().and_then(parse::<Comment>);
        let decision = match (&change.new, old) {
            (Slot::Absent, None) => return,
            (Slot::Absent, Some(old)) => {
                if old.author.id == self.writer.id {
                    return;
                }
                None
            }
            (Slot::Nested, _) => None,
            (Slot::Plain(raw), old) => match (parse::<Comment>(raw), old) {
                (None, _) => None,
                (Some(new), None) => {
                    let exists = self.thread_exists(new.thread);
                    let heavy = self.over();
                    let over = self.extra_in_thread.entry(new.thread).or_default();
                    if self.extra_comments > 0 || *over > 0 || !exists || heavy {
                        self.extra_comments = self.extra_comments.saturating_sub(1);
                        *over = over.saturating_sub(1);
                        self.shed(weight(raw));
                        self.fix.comments.insert(change.key.clone(), None);
                        return;
                    }
                    new_comment(&new, self.writer, self.now).map(|kept| (raw, kept))
                }
                (Some(new), Some(old)) => {
                    let grew = change
                        .old
                        .plain()
                        .map_or(0, |was| weight(raw).saturating_sub(weight(was)));
                    if grew > 0 && self.over() {
                        self.shed(grew);
                        None
                    } else {
                        changed_comment(&old, &new, self.writer, self.now).map(|kept| (raw, kept))
                    }
                }
            },
        };
        decide(&mut self.fix.comments, &change.key, decision, restore);
    }

    fn over(&self) -> bool {
        self.over_total > 0 || self.over_mine > 0
    }

    /// `bytes` the fix takes off the discussion (all of them the writer's).
    fn shed(&mut self, bytes: usize) {
        self.over_total = self.over_total.saturating_sub(bytes);
        self.over_mine = self.over_mine.saturating_sub(bytes);
    }

    /// A thread the transaction started goes when none of its comments
    /// stays (they went over a cap): never an empty thread.
    fn drop_empty(&mut self, threads: &[KeyChange]) {
        for change in threads {
            let started = change.old.plain().is_none() && change.new.plain().is_some();
            let Ok(id) = Uuid::parse_str(&change.key) else {
                continue;
            };
            if !started || self.fix.threads.get(&change.key) == Some(&None) {
                continue;
            }
            let kept = self.state.comments.iter().any(|(key, value)| {
                self.fix.comments.get(key) != Some(&None)
                    && parse::<Comment>(value).is_some_and(|c| c.thread == id)
            });
            if !kept {
                self.fix.threads.insert(change.key.clone(), None);
            }
        }
    }

    /// Whether a thread exists once the fix so far is applied.
    fn thread_exists(&self, id: Uuid) -> bool {
        let key = id.to_string();
        match self.fix.threads.get(&key) {
            Some(value) => value.is_some(),
            None => self.state.threads.contains_key(&key),
        }
    }

    /// A thread its author deleted takes their comments still in it along.
    fn sweep(&mut self, threads: &[KeyChange]) {
        for change in threads {
            let deleted = matches!(change.new, Slot::Absent)
                && change.old.plain().is_some()
                && !self.fix.threads.contains_key(&change.key);
            let Ok(id) = Uuid::parse_str(&change.key) else {
                continue;
            };
            if !deleted {
                continue;
            }
            for (key, value) in self.state.comments {
                if parse::<Comment>(value).is_some_and(|c| c.thread == id)
                    && !self.fix.comments.contains_key(key)
                {
                    self.fix.comments.insert(key.clone(), None);
                }
            }
        }
    }
}

/// What a checked change leaves: the value kept, when the raw one is not
/// already it; or, when the change may not stand (`None`), the old value
/// back (or nothing, for a new entry).
fn decide<T: Serialize>(
    fix: &mut BTreeMap<String, Option<Value>>,
    key: &str,
    decision: Option<(&Value, T)>,
    restore: Option<Value>,
) {
    match decision {
        Some((raw, kept)) => {
            if !same(raw, &kept) {
                fix.insert(key.to_owned(), Some(to_value(&kept)));
            }
        }
        None => {
            fix.insert(key.to_owned(), restore);
        }
    }
}

/// A new thread as it may be kept: started by the writer, now-ish, not
/// resolved by anyone else.
fn new_thread(new: &Thread, writer: &Writer<'_>, now: DateTime<Utc>) -> Option<Thread> {
    Some(Thread {
        anchor: new.anchor.normalized()?,
        by: writer.id,
        at: near(new.at, now),
        resolved: new.resolved.as_ref().map(|_| resolution(writer, now)),
    })
}

/// A thread someone changed: its author and time stay; the anchor moves for
/// editors and the author; anyone resolves or reopens it, stamped.
fn changed_thread(old: &Thread, new: &Thread, writer: &Writer<'_>, now: DateTime<Utc>) -> Thread {
    let anchor = if new.anchor != old.anchor && (writer.can_edit || old.by == writer.id) {
        new.anchor
            .normalized()
            .unwrap_or_else(|| old.anchor.clone())
    } else {
        old.anchor.clone()
    };
    let resolved = match (&old.resolved, &new.resolved) {
        (None, Some(_)) => Some(resolution(writer, now)),
        (Some(_), None) => None,
        (old, _) => old.clone(),
    };
    Thread {
        anchor,
        by: old.by,
        at: old.at,
        resolved,
    }
}

fn resolution(writer: &Writer<'_>, now: DateTime<Utc>) -> Resolution {
    Resolution {
        by: writer.id,
        name: writer.name.to_owned(),
        at: now,
    }
}

/// A new comment as it may be kept: by the writer, now-ish, with text.
fn new_comment(new: &Comment, writer: &Writer<'_>, now: DateTime<Utc>) -> Option<Comment> {
    valid_body(&new.body).then(|| Comment {
        thread: new.thread,
        author: Author {
            id: writer.id,
            name: writer.name.to_owned(),
        },
        body: new.body.clone(),
        // Mentions come through operations, checked against the members.
        mentions: Vec::new(),
        at: near(new.at, now),
        edited: None,
    })
}

/// A comment someone changed: only its author edits it (its text and
/// mentions), marked edited.
fn changed_comment(
    old: &Comment,
    new: &Comment,
    writer: &Writer<'_>,
    now: DateTime<Utc>,
) -> Option<Comment> {
    if old.author.id != writer.id || !valid_body(&new.body) {
        return None;
    }
    // Mentions come through operations, checked against the members.
    let mentions = old.mentions.clone();
    let edited = if new.body == old.body {
        old.edited
    } else {
        Some(new.edited.map_or(now, |at| near(at, now)))
    };
    Some(Comment {
        thread: old.thread,
        author: old.author.clone(),
        body: new.body.clone(),
        mentions,
        at: old.at,
        edited,
    })
}

fn valid_body(body: &str) -> bool {
    !body.trim().is_empty() && body.chars().count() <= MAX_BODY_CHARS
}

fn mentions(given: &[Uuid]) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    given
        .iter()
        .copied()
        .filter(|id| seen.insert(*id))
        .take(MAX_MENTIONS)
        .collect()
}

/// `at`, unless it is further than [`CLOCK_SKEW`] from `now` (then `now`),
/// to the millisecond, as the browser writes times.
fn near(at: DateTime<Utc>, now: DateTime<Utc>) -> DateTime<Utc> {
    if (at - now).abs() > CLOCK_SKEW {
        now
    } else {
        millis(at)
    }
}

fn millis(at: DateTime<Utc>) -> DateTime<Utc> {
    at.duration_trunc(Duration::milliseconds(1)).unwrap_or(at)
}

/// Whether the raw value already is `kept`, field for field (extra fields
/// count as a difference: they are dropped).
fn same<T: Serialize>(raw: &Value, kept: &T) -> bool {
    let kept = to_value(kept);
    normalize_numbers(raw) == normalize_numbers(&kept)
}

/// Numbers compared as numbers (`1.0` from Yjs, `1` from serde).
fn normalize_numbers(value: &Value) -> Value {
    match value {
        Value::Number(n) => n
            .as_f64()
            .map_or_else(|| value.clone(), |f| serde_json::json!(f)),
        Value::Array(items) => Value::Array(items.iter().map(normalize_numbers).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), normalize_numbers(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: &Value) -> Option<T> {
    T::deserialize(value).ok()
}

fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn any_to_value(any: &Any) -> Value {
    let mut json = String::new();
    any.to_json(&mut json);
    serde_json::from_str(&json).unwrap_or(Value::Null)
}

fn value_to_any(value: &Value) -> Any {
    Any::from_json(&value.to_string()).unwrap_or(Any::Null)
}

/// A map's entries as JSON (nested types skipped).
pub fn read_map<T: ReadTxn>(map: &MapRef, txn: &T) -> BTreeMap<String, Value> {
    map.iter(txn)
        .filter_map(|(key, value)| match value {
            Out::Any(any) => Some((key.to_owned(), any_to_value(&any))),
            _ => None,
        })
        .collect()
}

/// Moves the threads of each renamed table (`(old, new)` qualified names)
/// to its new name; whether any moved. In the transaction that moves its
/// layout entry (`Roots::rename`).
pub fn rename(threads: &MapRef, txn: &mut TransactionMut, renames: &[(String, String)]) -> bool {
    let mut moved = Vec::new();
    for (key, value) in read_map(threads, txn) {
        let Some(mut thread) = parse::<Thread>(&value) else {
            continue;
        };
        let Some(table) = thread.anchor.table() else {
            continue;
        };
        if let Some((_, new)) = renames.iter().find(|(old, _)| old == table) {
            thread.anchor = thread.anchor.with_table(new);
            moved.push((key, thread));
        }
    }
    for (key, thread) in &moved {
        threads.insert(txn, key.as_str(), value_to_any(&to_value(thread)));
    }
    !moved.is_empty()
}

/// A thread with its comments, oldest first: what a draft's discussion is
/// when it is published.
#[derive(Debug, Clone, PartialEq)]
pub struct DocThread {
    pub id: Uuid,
    pub thread: Thread,
    pub comments: Vec<(Uuid, Comment)>,
}

/// Every thread of the doc that has a comment, oldest first (entries that do
/// not read as threads or comments are left out).
pub fn discussion<T: ReadTxn>(threads: &MapRef, comments: &MapRef, txn: &T) -> Vec<DocThread> {
    let mut by_thread: HashMap<Uuid, Vec<(Uuid, Comment)>> = HashMap::new();
    for (key, value) in read_map(comments, txn) {
        if let (Ok(id), Some(comment)) = (Uuid::parse_str(&key), parse::<Comment>(&value)) {
            by_thread
                .entry(comment.thread)
                .or_default()
                .push((id, comment));
        }
    }
    let mut out: Vec<DocThread> = read_map(threads, txn)
        .into_iter()
        .filter_map(|(key, value)| {
            let id = Uuid::parse_str(&key).ok()?;
            let thread = parse::<Thread>(&value)?;
            let mut comments = by_thread.remove(&id)?;
            comments.sort_by(|a, b| a.1.at.cmp(&b.1.at).then(a.0.cmp(&b.0)));
            Some(DocThread {
                id,
                thread,
                comments,
            })
        })
        .collect();
    out.sort_by(|a, b| a.thread.at.cmp(&b.thread.at).then(a.id.cmp(&b.id)));
    out
}

/// An operation on a draft's discussion, asked on the socket
/// (`protocol::MESSAGE_COMMENT`): the room writes what it means ([`plan`]).
/// The client picks the new ids (UUIDs), so it can open what it starts.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    Start {
        thread: Uuid,
        comment: Uuid,
        anchor: Anchor,
        body: String,
        #[serde(default)]
        mentions: Vec<Uuid>,
    },
    Reply {
        thread: Uuid,
        comment: Uuid,
        body: String,
        #[serde(default)]
        mentions: Vec<Uuid>,
    },
    Edit {
        comment: Uuid,
        body: String,
        #[serde(default)]
        mentions: Vec<Uuid>,
    },
    /// One's own comment, or anyone's for developers and admins; the
    /// thread goes with its last one.
    Delete {
        comment: Uuid,
    },
    /// A thread and every comment in it: one's own discussion, or anyone's
    /// for developers and admins.
    DeleteThread {
        thread: Uuid,
    },
    Resolve {
        thread: Uuid,
        resolved: bool,
    },
}

impl Op {
    /// The members it mentions, to check against the organization's.
    pub fn mentions_mut(&mut self) -> Option<&mut Vec<Uuid>> {
        match self {
            Self::Start { mentions, .. }
            | Self::Reply { mentions, .. }
            | Self::Edit { mentions, .. } => Some(mentions),
            Self::Delete { .. } | Self::DeleteThread { .. } | Self::Resolve { .. } => None,
        }
    }
}

/// An operation and the id its answer carries (`ServerEvent::CommentResult`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub op: Op,
}

/// The longest request id kept (the answer echoes it).
pub const MAX_REQUEST_ID: usize = 64;

/// Why an operation was refused: a stable code and a sentence for people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

fn refuse(code: &'static str, message: &str) -> Refusal {
    Refusal {
        code,
        message: message.to_owned(),
    }
}

fn no_thread() -> Refusal {
    refuse("not_found", "This thread was deleted.")
}

fn bad_body() -> Refusal {
    refuse("invalid", "Write between 1 and 4000 characters.")
}

/// What an operation writes, and whose comments it deleted when they were
/// not the writer's (for the audit log).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Planned {
    pub fix: Fix,
    pub moderation: Option<Moderation>,
}

impl From<Fix> for Planned {
    fn from(fix: Fix) -> Self {
        Self {
            fix,
            moderation: None,
        }
    }
}

/// Comments a developer or an admin deleted that someone else wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct Moderation {
    pub thread: Uuid,
    /// How many comments went (the thread's, when it went whole).
    pub comments: usize,
    /// Whose (names, as they were), the writer's own left out.
    pub authors: Vec<String>,
    /// The beginning of the first one, to know what it was.
    pub excerpt: String,
}

/// How much of a deleted comment the audit log keeps.
const EXCERPT_CHARS: usize = 200;

pub(crate) fn excerpt(body: &str) -> String {
    let line = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= EXCERPT_CHARS {
        line
    } else {
        let cut: String = line.chars().take(EXCERPT_CHARS - 1).collect();
        format!("{cut}…")
    }
}

/// What an operation by `writer` at `now` writes to the two maps (a
/// [`Fix`], applied as a server edit), or why it may not: the author is the
/// writer, the time the server's, a comment is edited by its author only
/// and deleted by its author or a developer or an admin, sizes and weights
/// are capped. `mentions` must be the organization's members already (the
/// socket checks them).
pub fn plan(
    op: &Op,
    writer: &Writer<'_>,
    now: DateTime<Utc>,
    state: &State<'_>,
) -> Result<Planned, Refusal> {
    let planner = Planner {
        writer,
        now: millis(now),
        state,
        weights: Weights::measure(state),
    };
    match op {
        Op::Start {
            thread,
            comment,
            anchor,
            body,
            mentions,
        } => planner
            .start(*thread, *comment, anchor, body, mentions)
            .map(Planned::from),
        Op::Reply {
            thread,
            comment,
            body,
            mentions,
        } => planner
            .reply(*thread, *comment, body, mentions)
            .map(Planned::from),
        Op::Edit {
            comment,
            body,
            mentions,
        } => planner.edit(*comment, body, mentions).map(Planned::from),
        Op::Delete { comment } => planner.delete(*comment),
        Op::DeleteThread { thread } => planner.delete_thread(*thread),
        Op::Resolve { thread, resolved } => planner.resolve(*thread, *resolved).map(Planned::from),
    }
}

/// [`plan`]'s view of the maps, per operation.
struct Planner<'a> {
    writer: &'a Writer<'a>,
    now: DateTime<Utc>,
    state: &'a State<'a>,
    weights: Weights,
}

impl Planner<'_> {
    fn thread(&self, id: Uuid) -> Option<Thread> {
        self.state
            .threads
            .get(&id.to_string())
            .and_then(parse::<Thread>)
    }

    fn comment(&self, id: Uuid) -> Option<Comment> {
        self.state
            .comments
            .get(&id.to_string())
            .and_then(parse::<Comment>)
    }

    fn in_thread(&self, thread: Uuid) -> usize {
        self.state
            .comments
            .values()
            .filter_map(parse::<Comment>)
            .filter(|c| c.thread == thread)
            .count()
    }

    fn new_comment(&self, thread: Uuid, body: &str, given: &[Uuid]) -> Comment {
        Comment {
            thread,
            author: Author {
                id: self.writer.id,
                name: self.writer.name.to_owned(),
            },
            body: body.to_owned(),
            mentions: mentions(given),
            at: self.now,
            edited: None,
        }
    }

    fn start(
        &self,
        thread: Uuid,
        comment: Uuid,
        anchor: &Anchor,
        body: &str,
        given: &[Uuid],
    ) -> Result<Fix, Refusal> {
        if self.state.threads.contains_key(&thread.to_string())
            || self.state.comments.contains_key(&comment.to_string())
        {
            return Err(refuse("exists", "That comment was sent already."));
        }
        let anchor = anchor.normalized().ok_or_else(|| {
            refuse(
                "invalid",
                "Pin the comment to a table, a column or the canvas.",
            )
        })?;
        if !valid_body(body) {
            return Err(bad_body());
        }
        if self.state.threads.len() >= MAX_THREADS || self.state.comments.len() >= MAX_COMMENTS {
            return Err(refuse(
                "too_many",
                "This draft has as many comments as it can take. Delete some first.",
            ));
        }
        let new = Thread {
            anchor,
            by: self.writer.id,
            at: self.now,
            resolved: None,
        };
        let (new, first) = (
            to_value(&new),
            to_value(&self.new_comment(thread, body, given)),
        );
        if let Some(refusal) = self
            .weights
            .refuse(self.writer.id, weight(&new) + weight(&first))
        {
            return Err(refusal);
        }
        let mut fix = Fix::default();
        fix.threads.insert(thread.to_string(), Some(new));
        fix.comments.insert(comment.to_string(), Some(first));
        Ok(fix)
    }

    fn reply(
        &self,
        thread: Uuid,
        comment: Uuid,
        body: &str,
        given: &[Uuid],
    ) -> Result<Fix, Refusal> {
        self.thread(thread).ok_or_else(no_thread)?;
        if self.state.comments.contains_key(&comment.to_string()) {
            return Err(refuse("exists", "That reply was sent already."));
        }
        if !valid_body(body) {
            return Err(bad_body());
        }
        if self.in_thread(thread) >= MAX_COMMENTS_PER_THREAD
            || self.state.comments.len() >= MAX_COMMENTS
        {
            return Err(refuse(
                "too_many",
                "This thread has as many comments as it can take. Start a new one.",
            ));
        }
        let reply = to_value(&self.new_comment(thread, body, given));
        if let Some(refusal) = self.weights.refuse(self.writer.id, weight(&reply)) {
            return Err(refusal);
        }
        let mut fix = Fix::default();
        fix.comments.insert(comment.to_string(), Some(reply));
        Ok(fix)
    }

    fn edit(&self, comment: Uuid, body: &str, given: &[Uuid]) -> Result<Fix, Refusal> {
        let old = self
            .comment(comment)
            .ok_or_else(|| refuse("not_found", "This comment was deleted."))?;
        if old.author.id != self.writer.id {
            return Err(refuse("not_yours", "Only its author can edit a comment."));
        }
        if !valid_body(body) {
            return Err(bad_body());
        }
        let mut fix = Fix::default();
        let mentions = mentions(given);
        if body != old.body || mentions != old.mentions {
            let was = weight(&to_value(&old));
            let edited = to_value(&Comment {
                body: body.to_owned(),
                mentions,
                edited: Some(self.now),
                ..old
            });
            let grows = weight(&edited).saturating_sub(was);
            if let Some(refusal) = self.weights.refuse(self.writer.id, grows) {
                return Err(refusal);
            }
            fix.comments.insert(comment.to_string(), Some(edited));
        }
        Ok(fix)
    }

    fn delete(&self, comment: Uuid) -> Result<Planned, Refusal> {
        let old = self
            .comment(comment)
            .ok_or_else(|| refuse("not_found", "This comment was deleted already."))?;
        let theirs = old.author.id != self.writer.id;
        if theirs && !self.writer.can_edit {
            return Err(refuse(
                "not_yours",
                "Only its author, a developer or an admin can delete a comment.",
            ));
        }
        let mut fix = Fix::default();
        fix.comments.insert(comment.to_string(), None);
        // Nobody's discussion any more: the thread goes with its last
        // comment. Others' replies are never deleted with it.
        if self.in_thread(old.thread) <= 1 {
            fix.threads.insert(old.thread.to_string(), None);
        }
        let moderation = theirs.then(|| Moderation {
            thread: old.thread,
            comments: 1,
            authors: vec![old.author.name.clone()],
            excerpt: excerpt(&old.body),
        });
        Ok(Planned { fix, moderation })
    }

    fn delete_thread(&self, thread: Uuid) -> Result<Planned, Refusal> {
        self.thread(thread).ok_or_else(no_thread)?;
        let mut in_it: Vec<(&String, Comment)> = self
            .state
            .comments
            .iter()
            .filter_map(|(key, value)| parse::<Comment>(value).map(|c| (key, c)))
            .filter(|(_, c)| c.thread == thread)
            .collect();
        in_it.sort_by_key(|(_, c)| c.at);
        let mut authors: Vec<String> = Vec::new();
        for (_, c) in &in_it {
            if c.author.id != self.writer.id && !authors.contains(&c.author.name) {
                authors.push(c.author.name.clone());
            }
        }
        if !authors.is_empty() && !self.writer.can_edit {
            return Err(refuse(
                "not_yours",
                "Only a developer or an admin can delete a thread others wrote in.",
            ));
        }
        let mut fix = Fix::default();
        fix.threads.insert(thread.to_string(), None);
        for (key, _) in &in_it {
            fix.comments.insert((*key).clone(), None);
        }
        let moderation = (!authors.is_empty()).then(|| Moderation {
            thread,
            comments: in_it.len(),
            excerpt: in_it
                .first()
                .map(|(_, c)| excerpt(&c.body))
                .unwrap_or_default(),
            authors,
        });
        Ok(Planned { fix, moderation })
    }

    fn resolve(&self, thread: Uuid, resolved: bool) -> Result<Fix, Refusal> {
        let old = self.thread(thread).ok_or_else(no_thread)?;
        let mut fix = Fix::default();
        if old.resolved.is_some() != resolved {
            let changed = Thread {
                resolved: resolved.then(|| resolution(self.writer, self.now)),
                ..old
            };
            fix.threads
                .insert(thread.to_string(), Some(to_value(&changed)));
        }
        Ok(fix)
    }
}

/// Whether a map holds list content: items without a key, which no map
/// reading sees (a place to hide megabytes). Clients never write any.
pub fn stray_content(map: &MapRef) -> bool {
    let branch: &yrs::branch::Branch = map.as_ref();
    branch.block_len > 0 || branch.content_len > 0
}

/// Deletes a map's list content ([`stray_content`]); whether there was any.
pub fn remove_stray(map: &MapRef, txn: &mut TransactionMut) -> bool {
    use yrs::Array;
    let list = yrs::ArrayRef::from(yrs::branch::BranchPtr::from(map.as_ref()));
    let len = list.len(txn);
    if len == 0 {
        return false;
    }
    list.remove_range(txn, 0, len);
    true
}

/// A line written as a comment (`--- words`): three dashes, then words.
/// Lines of dashes only and `----` are SQL comments, as in the web app's
/// `commentLine`.
pub fn is_comment_line(line: &str) -> bool {
    line.trim_start()
        .strip_prefix("---")
        .is_some_and(|rest| !rest.starts_with('-') && !rest.trim().is_empty())
}

/// Byte ranges of the `---` lines of a draft's text that its base version
/// does not have (those came with the version, and stay): comments typed and
/// never posted. A draft's SQL never keeps one ([`comment_line_diagnostics`]).
pub fn comment_lines(source: &str, base: &str) -> Vec<std::ops::Range<usize>> {
    let mut inherited: HashMap<&str, usize> = HashMap::new();
    for line in base.lines().filter(|l| is_comment_line(l)) {
        *inherited.entry(line.trim()).or_default() += 1;
    }
    let mut out = Vec::new();
    let mut start = 0;
    for line in source.split_inclusive('\n') {
        let text = line.trim_end_matches(['\n', '\r']);
        if is_comment_line(text) {
            match inherited.get_mut(text.trim()) {
                Some(left) if *left > 0 => *left -= 1,
                _ => out.push(start..start + text.len()),
            }
        }
        start += line.len();
    }
    out
}

/// The compile's errors for [`comment_lines`]: the text does not compile
/// while one is left (publishing waits), each located on its line.
pub fn comment_line_diagnostics(source: &str, base: &str) -> Vec<romulus_sandbox::Diagnostic> {
    use romulus_sandbox::{Code, Diagnostic, LineIndex};
    let lines = comment_lines(source, base);
    if lines.is_empty() {
        return Vec::new();
    }
    let index = LineIndex::new(source);
    lines
        .into_iter()
        .map(|range| {
            Diagnostic::error(
                Code::CommentLine,
                "This --- line is a comment that was never posted.",
            )
            .with_hint(
                "Put the caret after its words and press Enter to post it to the discussion, or write it with two dashes (-- …) to keep it in the SQL.",
            )
            .with_span(index.span(source, range))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const ALICE: Uuid = Uuid::from_u128(1);
    const BOB: Uuid = Uuid::from_u128(2);

    fn alice() -> Writer<'static> {
        Writer {
            id: ALICE,
            name: "Alice",
            can_edit: true,
        }
    }

    fn bob_reviewer() -> Writer<'static> {
        Writer {
            id: BOB,
            name: "Bob",
            can_edit: false,
        }
    }

    fn now() -> DateTime<Utc> {
        "2026-10-16T12:00:00Z".parse().unwrap()
    }

    const T1: &str = "00000000-0000-4000-8000-000000000001";
    const T2: &str = "00000000-0000-4000-8000-000000000002";
    const C1: &str = "00000000-0000-4000-8000-0000000000c1";
    const C2: &str = "00000000-0000-4000-8000-0000000000c2";

    fn thread(by: Uuid) -> Value {
        json!({
            "anchor": {"kind": "column", "table": "public.users", "column": "email"},
            "by": by, "at": "2026-10-16T11:59:30Z", "resolved": null,
        })
    }

    fn comment(author: Uuid, name: &str, body: &str) -> Value {
        json!({
            "thread": T1, "author": {"id": author, "name": name}, "body": body,
            "mentions": [], "at": "2026-10-16T11:59:30Z", "edited": null,
        })
    }

    fn inserted(key: &str, value: Value) -> KeyChange {
        KeyChange {
            key: key.into(),
            old: Slot::Absent,
            new: Slot::Plain(value),
        }
    }

    fn updated(key: &str, old: Value, new: Value) -> KeyChange {
        KeyChange {
            key: key.into(),
            old: Slot::Plain(old),
            new: Slot::Plain(new),
        }
    }

    fn removed(key: &str, old: Value) -> KeyChange {
        KeyChange {
            key: key.into(),
            old: Slot::Plain(old),
            new: Slot::Absent,
        }
    }

    fn run(
        changes: &Changes,
        writer: &Writer<'_>,
        threads: &[(&str, Value)],
        comments: &[(&str, Value)],
    ) -> Fix {
        let threads: BTreeMap<String, Value> = threads
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        let comments: BTreeMap<String, Value> = comments
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        check(
            changes,
            writer,
            now(),
            &State {
                threads: &threads,
                comments: &comments,
            },
        )
    }

    #[test]
    fn a_well_formed_thread_needs_no_fix() {
        let changes = Changes {
            threads: vec![inserted(T1, thread(ALICE))],
            comments: vec![inserted(C1, comment(ALICE, "Alice", "Why text?"))],
        };
        let fix = run(
            &changes,
            &alice(),
            &[(T1, thread(ALICE))],
            &[(C1, comment(ALICE, "Alice", "Why text?"))],
        );
        assert_eq!(fix, Fix::default());
    }

    #[test]
    fn authors_are_stamped_whatever_the_client_wrote() {
        let forged = comment(ALICE, "Alice", "I agree");
        let changes = Changes {
            threads: vec![inserted(T1, thread(ALICE))],
            comments: vec![inserted(C1, forged.clone())],
        };
        let fix = run(
            &changes,
            &bob_reviewer(),
            &[(T1, thread(ALICE))],
            &[(C1, forged)],
        );
        assert_eq!(fix.threads[T1].as_ref().unwrap()["by"], json!(BOB));
        let stamped = fix.comments[C1].as_ref().unwrap();
        assert_eq!(stamped["author"], json!({"id": BOB, "name": "Bob"}));
        assert_eq!(stamped["body"], "I agree");
    }

    #[test]
    fn times_far_from_the_server_s_are_the_server_s() {
        let mut skewed = comment(ALICE, "Alice", "hi");
        skewed["at"] = json!("2020-01-01T00:00:00Z");
        let changes = Changes {
            threads: vec![],
            comments: vec![inserted(C1, skewed.clone())],
        };
        let fix = run(&changes, &alice(), &[(T1, thread(ALICE))], &[(C1, skewed)]);
        assert_eq!(
            fix.comments[C1].as_ref().unwrap()["at"],
            "2026-10-16T12:00:00Z"
        );
    }

    #[test]
    fn only_authors_edit_or_delete_their_comments() {
        let mine = comment(ALICE, "Alice", "first");
        let mut edited = mine.clone();
        edited["body"] = json!("rewritten by Bob");
        let changes = Changes {
            threads: vec![],
            comments: vec![updated(C1, mine.clone(), edited.clone())],
        };
        let fix = run(
            &changes,
            &bob_reviewer(),
            &[(T1, thread(ALICE))],
            &[(C1, edited)],
        );
        assert_eq!(fix.comments[C1], Some(mine.clone()), "put back");

        let changes = Changes {
            threads: vec![],
            comments: vec![removed(C1, mine.clone())],
        };
        let fix = run(&changes, &bob_reviewer(), &[(T1, thread(ALICE))], &[]);
        assert_eq!(fix.comments[C1], Some(mine.clone()), "restored");

        // Alice edits hers: marked edited.
        let mut edit = mine.clone();
        edit["body"] = json!("second thoughts");
        let changes = Changes {
            threads: vec![],
            comments: vec![updated(C1, mine.clone(), edit.clone())],
        };
        let fix = run(&changes, &alice(), &[(T1, thread(ALICE))], &[(C1, edit)]);
        let kept = fix.comments[C1].as_ref().unwrap();
        assert_eq!(kept["body"], "second thoughts");
        assert_eq!(kept["edited"], "2026-10-16T12:00:00Z");
    }

    #[test]
    fn anyone_resolves_stamped_and_reviewers_cannot_move_others_threads() {
        let open = thread(ALICE);
        let mut resolved = open.clone();
        resolved["resolved"] = json!({"by": ALICE, "name": "Alice", "at": "2026-10-16T11:00:00Z"});
        resolved["anchor"] = json!({"kind": "table", "table": "public.orders"});
        let changes = Changes {
            threads: vec![updated(T1, open.clone(), resolved.clone())],
            comments: vec![],
        };
        let fix = run(&changes, &bob_reviewer(), &[(T1, resolved)], &[]);
        let kept = fix.threads[T1].as_ref().unwrap();
        assert_eq!(kept["resolved"]["by"], json!(BOB), "stamped");
        assert_eq!(kept["resolved"]["name"], "Bob");
        assert_eq!(kept["anchor"], open["anchor"], "not Bob's to move");

        // An editor moves it (a rename carries it).
        let mut moved = open.clone();
        moved["anchor"] = json!({"kind": "column", "table": "public.members", "column": "email"});
        let changes = Changes {
            threads: vec![updated(T1, open, moved.clone())],
            comments: vec![],
        };
        assert_eq!(run(&changes, &alice(), &[(T1, moved)], &[]), Fix::default());
    }

    #[test]
    fn a_thread_goes_with_its_author_only_when_nobody_else_wrote_in_it() {
        let changes = Changes {
            threads: vec![removed(T1, thread(ALICE))],
            comments: vec![removed(C1, comment(ALICE, "Alice", "mine"))],
        };
        assert_eq!(run(&changes, &alice(), &[], &[]), Fix::default());

        // Bob replied: the thread stays (Alice's own comment may go).
        let changes = Changes {
            threads: vec![removed(T1, thread(ALICE))],
            comments: vec![removed(C1, comment(ALICE, "Alice", "mine"))],
        };
        let fix = run(
            &changes,
            &alice(),
            &[],
            &[(C2, comment(BOB, "Bob", "reply"))],
        );
        assert_eq!(fix.threads[T1], Some(thread(ALICE)));
        assert!(fix.comments.is_empty());

        // Someone else's thread is not theirs to delete.
        let changes = Changes {
            threads: vec![removed(T1, thread(ALICE))],
            comments: vec![],
        };
        let fix = run(
            &changes,
            &bob_reviewer(),
            &[],
            &[(C1, comment(ALICE, "Alice", "x"))],
        );
        assert_eq!(fix.threads[T1], Some(thread(ALICE)));
    }

    #[test]
    fn deleting_a_thread_takes_its_author_s_leftover_comments() {
        let changes = Changes {
            threads: vec![removed(T1, thread(ALICE))],
            comments: vec![],
        };
        let fix = run(
            &changes,
            &alice(),
            &[],
            &[(C1, comment(ALICE, "Alice", "left behind"))],
        );
        assert_eq!(fix.comments[C1], None);
        assert!(fix.threads.is_empty());
    }

    #[test]
    fn sizes_are_capped_and_bad_entries_dropped() {
        let long = "x".repeat(MAX_BODY_CHARS + 1);
        let changes = Changes {
            threads: vec![
                inserted("not-a-uuid", thread(ALICE)),
                inserted(
                    "00000000-0000-4000-8000-000000000002",
                    json!({"anchor": {"kind": "table", "table": ""}, "by": ALICE, "at": "2026-10-16T12:00:00Z"}),
                ),
            ],
            comments: vec![
                inserted(C1, comment(ALICE, "Alice", &long)),
                inserted(C2, comment(ALICE, "Alice", "   ")),
                inserted(
                    "00000000-0000-4000-8000-0000000000c3",
                    json!({"thread": Uuid::from_u128(99), "author": {"id": ALICE, "name": "Alice"},
                           "body": "orphan", "at": "2026-10-16T12:00:00Z"}),
                ),
            ],
        };
        let fix = run(&changes, &alice(), &[(T1, thread(ALICE))], &[]);
        assert_eq!(fix.threads["not-a-uuid"], None);
        assert_eq!(fix.threads["00000000-0000-4000-8000-000000000002"], None);
        assert_eq!(fix.comments[C1], None);
        assert_eq!(fix.comments[C2], None);
        assert_eq!(fix.comments["00000000-0000-4000-8000-0000000000c3"], None);
    }

    #[test]
    fn extra_fields_are_dropped_and_direct_writes_mention_nobody() {
        let mut extra = comment(ALICE, "Alice", "hi @Bob");
        extra["mentions"] = json!([BOB, BOB]);
        extra["color"] = json!("red");
        let changes = Changes {
            threads: vec![],
            comments: vec![inserted(C1, extra.clone())],
        };
        let fix = run(&changes, &alice(), &[(T1, thread(ALICE))], &[(C1, extra)]);
        let kept = fix.comments[C1].as_ref().unwrap();
        // Mentions come through operations, checked against the members.
        assert_eq!(kept["mentions"], json!([]));
        assert!(kept.get("color").is_none());
    }

    #[test]
    fn a_thread_with_replies_of_others_stays_even_when_they_go_with_it() {
        // Alice deletes her thread, her comment and Bob's reply at once.
        let changes = Changes {
            threads: vec![removed(T1, thread(ALICE))],
            comments: vec![
                removed(C1, comment(ALICE, "Alice", "mine")),
                removed(C2, comment(BOB, "Bob", "theirs")),
            ],
        };
        let fix = run(&changes, &alice(), &[], &[]);
        assert_eq!(fix.threads[T1], Some(thread(ALICE)), "the thread stays");
        assert_eq!(
            fix.comments[C2],
            Some(comment(BOB, "Bob", "theirs")),
            "the reply stays"
        );
        assert!(!fix.comments.contains_key(C1), "her own may go");
    }

    fn state<'a>(
        threads: &'a BTreeMap<String, Value>,
        comments: &'a BTreeMap<String, Value>,
    ) -> State<'a> {
        State { threads, comments }
    }

    #[test]
    fn operations_write_as_their_author_now() {
        let (threads, comments) = (BTreeMap::new(), BTreeMap::new());
        let t = Uuid::from_u128(10);
        let c = Uuid::from_u128(11);
        let op = Op::Start {
            thread: t,
            comment: c,
            anchor: Anchor::Table {
                table: "public.users".into(),
                statement: None,
            },
            body: "why?".into(),
            mentions: vec![BOB, BOB],
        };
        let fix = plan(&op, &bob_reviewer(), now(), &state(&threads, &comments))
            .unwrap()
            .fix;
        let written = fix.comments[&c.to_string()].as_ref().unwrap();
        assert_eq!(written["author"], json!({"id": BOB, "name": "Bob"}));
        assert_eq!(written["mentions"], json!([BOB]));
        assert_eq!(written["at"], "2026-10-16T12:00:00Z");
        assert_eq!(
            fix.threads[&t.to_string()].as_ref().unwrap()["by"],
            json!(BOB)
        );
        // Sent twice: refused.
        let threads: BTreeMap<String, Value> = fix
            .threads
            .iter()
            .map(|(k, v)| (k.clone(), v.clone().unwrap()))
            .collect();
        let comments: BTreeMap<String, Value> = fix
            .comments
            .iter()
            .map(|(k, v)| (k.clone(), v.clone().unwrap()))
            .collect();
        let again = plan(&op, &bob_reviewer(), now(), &state(&threads, &comments));
        assert_eq!(again.unwrap_err().code, "exists");
    }

    #[test]
    fn operations_change_only_their_author_s_comments() {
        let threads = BTreeMap::from([(T1.to_owned(), thread(ALICE))]);
        let comments = BTreeMap::from([
            (C1.to_owned(), comment(ALICE, "Alice", "mine")),
            (C2.to_owned(), comment(BOB, "Bob", "theirs")),
        ]);
        let st = state(&threads, &comments);
        let c1 = Uuid::parse_str(C1).unwrap();
        let edit = Op::Edit {
            comment: c1,
            body: "Bob's now".into(),
            mentions: vec![],
        };
        assert_eq!(
            plan(&edit, &bob_reviewer(), now(), &st).unwrap_err().code,
            "not_yours"
        );
        let delete = Op::Delete { comment: c1 };
        assert_eq!(
            plan(&delete, &bob_reviewer(), now(), &st).unwrap_err().code,
            "not_yours"
        );
        // Alice deletes hers: Bob's reply stays, and so does the thread.
        let fix = plan(&delete, &alice(), now(), &st).unwrap().fix;
        assert_eq!(fix.comments[C1], None);
        assert!(fix.threads.is_empty());
        // Anyone resolves, stamped.
        let resolve = Op::Resolve {
            thread: Uuid::parse_str(T1).unwrap(),
            resolved: true,
        };
        let fix = plan(&resolve, &bob_reviewer(), now(), &st).unwrap().fix;
        assert_eq!(fix.threads[T1].as_ref().unwrap()["resolved"]["name"], "Bob");
    }

    #[test]
    fn developers_and_admins_delete_anyone_s_comments_and_it_is_noted() {
        let threads = BTreeMap::from([(T1.to_owned(), thread(BOB))]);
        let comments = BTreeMap::from([
            (C1.to_owned(), comment(BOB, "Bob", "spam   spam")),
            (C2.to_owned(), comment(ALICE, "Alice", "mine")),
        ]);
        let st = state(&threads, &comments);
        let theirs = Op::Delete {
            comment: Uuid::parse_str(C1).unwrap(),
        };
        let planned = plan(&theirs, &alice(), now(), &st).unwrap();
        assert_eq!(planned.fix.comments[C1], None);
        let moderation = planned.moderation.expect("audited");
        assert_eq!(moderation.authors, vec!["Bob".to_owned()]);
        assert_eq!(moderation.excerpt, "spam spam");
        // A whole thread: a developer's to delete, not a reviewer's.
        let whole = Op::DeleteThread {
            thread: Uuid::parse_str(T1).unwrap(),
        };
        let bob_alone = BTreeMap::from([(C1.to_owned(), comment(BOB, "Bob", "spam"))]);
        assert!(plan(&whole, &bob_reviewer(), now(), &state(&threads, &bob_alone)).is_ok());
        assert_eq!(
            plan(&whole, &bob_reviewer(), now(), &st).unwrap_err().code,
            "not_yours"
        );
        let planned = plan(&whole, &alice(), now(), &st).unwrap();
        assert_eq!(planned.fix.threads[T1], None);
        assert_eq!(
            (
                planned.fix.comments[C1].clone(),
                planned.fix.comments[C2].clone()
            ),
            (None, None)
        );
        assert_eq!(planned.moderation.map(|m| m.comments), Some(2));
    }

    #[test]
    fn the_discussion_and_each_person_s_part_of_it_weigh_so_much() {
        let long = "\u{1D538}".repeat(MAX_BODY_CHARS);
        let mut threads = BTreeMap::new();
        let mut comments = BTreeMap::new();
        let mut bytes = 0;
        let mut n = 0u128;
        while bytes < MAX_PERSON_BYTES {
            let value = comment(BOB, "Bob", &long);
            bytes += weight(&value);
            comments.insert(Uuid::from_u128(1000 + n).to_string(), value);
            n += 1;
        }
        threads.insert(T1.to_owned(), thread(BOB));
        let reply = |by: &Writer<'_>, st: &State<'_>| {
            plan(
                &Op::Reply {
                    thread: Uuid::parse_str(T1).unwrap(),
                    comment: Uuid::new_v4(),
                    body: "one more".into(),
                    mentions: vec![],
                },
                by,
                now(),
                st,
            )
        };
        let st = state(&threads, &comments);
        assert_eq!(reply(&bob_reviewer(), &st).unwrap_err().code, "too_much");
        assert!(reply(&alice(), &st).is_ok(), "others still write");
        // Many people's: the discussion is full for everyone.
        let mut everyone = BTreeMap::new();
        let mut total = 0;
        while total < MAX_DISCUSSION_BYTES {
            let author = Uuid::from_u128(5000 + n);
            let value = comment(author, "Someone", &long);
            total += weight(&value);
            everyone.insert(Uuid::from_u128(9000 + n).to_string(), value);
            n += 1;
        }
        let st = state(&threads, &everyone);
        assert_eq!(reply(&alice(), &st).unwrap_err().code, "discussion_full");
    }

    #[test]
    fn a_direct_write_past_the_writer_s_part_is_taken_back() {
        let long = "\u{1D538}".repeat(MAX_BODY_CHARS);
        let mut threads = BTreeMap::from([(T1.to_owned(), thread(ALICE))]);
        let mut comments = BTreeMap::new();
        for n in 0..20u128 {
            comments.insert(
                Uuid::from_u128(1000 + n).to_string(),
                comment(ALICE, "Alice", &long),
            );
        }
        // Her `---` line: a thread and its comment, in one transaction.
        threads.insert(T2.to_owned(), thread(ALICE));
        comments.insert(C1.to_owned(), comment(ALICE, "Alice", "why?"));
        let changes = Changes {
            threads: vec![inserted(T2, thread(ALICE))],
            comments: vec![inserted(C1, comment(ALICE, "Alice", "why?"))],
        };
        let fix = check(&changes, &alice(), now(), &state(&threads, &comments));
        assert_eq!(fix.comments.get(C1), Some(&None));
        assert_eq!(fix.threads.get(T2), Some(&None), "no thread left empty");
    }

    #[test]
    fn dash_lines_are_refused_unless_the_version_had_them() {
        assert!(is_comment_line("--- why?"));
        assert!(is_comment_line("   ---why"));
        assert!(!is_comment_line("---"));
        assert!(!is_comment_line("--------"));
        assert!(!is_comment_line("---- note"));
        assert!(!is_comment_line("-- --- quoted"));
        let base = "--- Users\nCREATE TABLE users ();\n";
        let source = "--- Users\nCREATE TABLE users ();\n--- Users\n--- new\n";
        let lines = comment_lines(source, base);
        assert_eq!(lines.len(), 2, "the version's one stays, once");
        assert_eq!(&source[lines[1].clone()], "--- new");
        let diagnostics = comment_line_diagnostics(source, base);
        assert_eq!(diagnostics[0].code, romulus_sandbox::Code::CommentLine);
    }

    #[test]
    fn points_are_whole_and_finite() {
        let anchor = Anchor::Point { x: 10.4, y: -3.6 };
        assert_eq!(
            anchor.normalized(),
            Some(Anchor::Point { x: 10.0, y: -4.0 })
        );
        assert_eq!(Anchor::Point { x: 1e9, y: 0.0 }.normalized(), None);
    }
}
