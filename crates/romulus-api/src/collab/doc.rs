//! A draft's Y.Doc: what it holds and how the server reads and writes it.
//!
//! docs/MVP.md "The core design":
//!
//! ```text
//! sql:       Y.Text                        the schema SQL: canonical in schema mode
//! migration: Y.Text                        the migration from the base version:
//!                                          canonical in migration mode
//! layout:    Y.Map<qualified name, {x, y}> positions, in graph coordinates
//! groups:    Y.Map<group id, {name, color, members: qualified names[]}>
//! meta:      Y.Map: `rewrite` = {count, reason}  (see `Roots::note_rewrite`),
//!            `merged` = the version "Update draft" merged last (`Roots::note_merged`),
//!            `mode` = "schema" | "migration" (absent: schema), written by the
//!            server only (`Roots::set_mode`; `drafts.mode` is the truth)
//! ```
//!
//! One text is the source at a time (docs/MVP.md "Review round 1", item 2):
//! in migration mode the schema is derived (compiled from the base version
//! and the migration) and `sql` waits, unused, until the draft goes back to
//! schema mode, which rewrites it from that derived schema.
//!
//! Map values are plain JSON objects (Yjs `Any`), not nested Y.Maps: a move
//! or a group edit replaces the whole value, last writer wins per key.
//!
//! The server writes to a doc in a few places only: the initial content of a
//! new draft, moving a table's layout and group entries when a rename is
//! typed in the text (see `room`), rewriting the SQL as a minimal edit
//! (Tidy, see `patch`), and switching modes (the migration seeded or
//! cleared, `meta.mode`, in one transaction). Everything else comes from
//! clients.

use std::collections::BTreeMap;

use serde::Serialize;
use yrs::{
    Any, Doc, GetString, Map, MapRef, OffsetKind, Options, ReadTxn, StateVector, Text, TextRef,
    Transact, TransactionMut, Update, types::ToJson, updates::decoder::Decode,
};

use crate::{
    domain::DraftMode,
    versions::layout::{TableGroup, TablePosition, VersionLayout},
};

pub const SQL: &str = "sql";
pub const MIGRATION: &str = "migration";
pub const LAYOUT: &str = "layout";
pub const GROUPS: &str = "groups";
pub const META: &str = "meta";

/// The line above the statements graph edits append (web/src/features/
/// schema/edits/append.ts `MARKER`).
pub const GRAPH_EDITS_MARKER: &str = "-- edits from the graph";

/// A doc with the options every peer uses: text offsets in UTF-16 code units,
/// like the browser's Yjs.
pub fn new_doc() -> Doc {
    Doc::with_options(Options {
        offset_kind: OffsetKind::Utf16,
        ..Options::default()
    })
}

/// The root types of a doc.
#[derive(Clone)]
pub struct Roots {
    pub sql: TextRef,
    pub migration: TextRef,
    pub layout: MapRef,
    pub groups: MapRef,
    pub meta: MapRef,
}

impl Roots {
    pub fn of(doc: &Doc) -> Self {
        Self {
            sql: doc.get_or_insert_text(SQL),
            migration: doc.get_or_insert_text(MIGRATION),
            layout: doc.get_or_insert_map(LAYOUT),
            groups: doc.get_or_insert_map(GROUPS),
            meta: doc.get_or_insert_map(META),
        }
    }

    /// Notes, in the transaction of a rewrite of the SQL by the server
    /// (Tidy, Update draft's merge), that it happened: `meta.rewrite` =
    /// `{count, reason}`. Every client clears its undo history when it sees
    /// it change: a rewrite reuses characters clients typed, in other places,
    /// and undoing an edit from before it would delete them wherever they
    /// are now. In the doc rather than an event, so a client that syncs the
    /// rewrite later (offline meanwhile) sees it too, in the same update.
    pub fn note_rewrite(&self, txn: &mut TransactionMut, reason: &str) {
        let count = self.rewrites(txn).map_or(0, |note| note.count);
        let note = RewriteNote {
            count: count + 1,
            reason: reason.to_owned(),
        };
        self.meta.insert(txn, "rewrite", to_any(&note));
    }

    /// Notes, in the transaction of "Update draft"'s rewrite, the version it
    /// merged (`meta.merged`): an update that failed before moving the base
    /// knows from it that the text already holds that version.
    pub fn note_merged(&self, txn: &mut TransactionMut, version: i32) {
        self.meta.insert(txn, "merged", to_any(&version));
    }

    /// The version the last update merged into the text, if any.
    pub fn merged<T: ReadTxn>(&self, txn: &T) -> Option<i32> {
        from_any(&self.meta.get(txn, "merged")?.to_json(txn))
    }

    /// The last rewrite noted, if any.
    pub fn rewrites<T: ReadTxn>(&self, txn: &T) -> Option<RewriteNote> {
        from_any(&self.meta.get(txn, "rewrite")?.to_json(txn))
    }

    pub fn sql<T: ReadTxn>(&self, txn: &T) -> String {
        self.sql.get_string(txn)
    }

    pub fn migration<T: ReadTxn>(&self, txn: &T) -> String {
        self.migration.get_string(txn)
    }

    /// The text that is the source in `mode`.
    pub fn source<T: ReadTxn>(&self, txn: &T, mode: DraftMode) -> String {
        match mode {
            DraftMode::Schema => self.sql(txn),
            DraftMode::Migration => self.migration(txn),
        }
    }

    /// The text that is the source in `mode`, to edit.
    pub fn source_text(&self, mode: DraftMode) -> &TextRef {
        match mode {
            DraftMode::Schema => &self.sql,
            DraftMode::Migration => &self.migration,
        }
    }

    /// `meta.mode` as the doc says (absent or unreadable: schema).
    pub fn mode<T: ReadTxn>(&self, txn: &T) -> DraftMode {
        self.meta
            .get(txn, "mode")
            .and_then(|value| from_any(&value.to_json(txn)))
            .unwrap_or_default()
    }

    /// Writes `meta.mode`, if it says otherwise; whether it did.
    pub fn set_mode(&self, txn: &mut TransactionMut, mode: DraftMode) -> bool {
        if self.meta.get(txn, "mode").is_some() && self.mode(txn) == mode {
            return false;
        }
        self.meta.insert(txn, "mode", mode.as_str());
        true
    }

    /// Replaces a whole text (a mode switch seeding or clearing the
    /// migration: nobody is editing it then).
    pub fn replace(text: &TextRef, txn: &mut TransactionMut, with: &str) {
        let len = text.len(txn);
        if len > 0 {
            text.remove_range(txn, 0, len);
        }
        if !with.is_empty() {
            text.insert(txn, 0, with);
        }
    }

    pub fn layout<T: ReadTxn>(&self, txn: &T) -> VersionLayout {
        VersionLayout {
            layout: read_map(&self.layout, txn),
            groups: read_map(&self.groups, txn),
        }
    }

    /// Moves the layout entry and group memberships of each `(old, new)`
    /// name, unless `new` already has a position of its own. Returns whether
    /// anything changed.
    pub fn rename(&self, txn: &mut TransactionMut, renames: &[(String, String)]) -> bool {
        let mut changed = false;
        for (old, new) in renames {
            if self.layout.get(txn, new).is_some() {
                continue;
            }
            if let Some(position) = self.layout.get(txn, old) {
                let value = position.to_json(txn);
                self.layout.remove(txn, old);
                self.layout.insert(txn, new.as_str(), value);
                changed = true;
            }
        }
        let groups: BTreeMap<String, TableGroup> = read_map(&self.groups, txn);
        for (id, mut group) in groups {
            let mut touched = false;
            for member in &mut group.members {
                if let Some((_, new)) = renames.iter().find(|(old, _)| old == member) {
                    member.clone_from(new);
                    touched = true;
                }
            }
            if touched {
                let mut seen = std::collections::HashSet::new();
                group.members.retain(|member| seen.insert(member.clone()));
                self.groups.insert(txn, id.as_str(), to_any(&group));
                changed = true;
            }
        }
        changed
    }
}

/// `meta.rewrite`: how many times the server rewrote the SQL, and why last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct RewriteNote {
    pub count: u64,
    pub reason: String,
}

/// Byte ranges of every graph edits marker line but the first (with the
/// blank line above it, when there is one). Two editors making their first
/// graph edit before seeing each other's each append one; the room removes
/// the extras before compiling.
pub fn extra_markers(sql: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut seen = false;
    let mut start = 0;
    for line in sql.split_inclusive('\n') {
        let end = start + line.len();
        if line.trim_end_matches(['\n', '\r', ' ', '\t']) == GRAPH_EDITS_MARKER {
            if seen {
                let from = if sql[..start].ends_with("\n\n") {
                    start - 1
                } else {
                    start
                };
                out.push(from..end);
            }
            seen = true;
        }
        start = end;
    }
    out
}

impl Roots {
    /// "Update draft"'s layout and group changes (`None`: remove). Returns
    /// whether there were any.
    pub fn apply_layout(
        &self,
        txn: &mut TransactionMut,
        layout: &[(String, Option<TablePosition>)],
        groups: &[(String, Option<TableGroup>)],
    ) -> bool {
        for (key, position) in layout {
            match position {
                Some(position) => {
                    self.layout.insert(txn, key.as_str(), to_any(position));
                }
                None => {
                    self.layout.remove(txn, key);
                }
            }
        }
        for (id, group) in groups {
            match group {
                Some(group) => {
                    self.groups.insert(txn, id.as_str(), to_any(group));
                }
                None => {
                    self.groups.remove(txn, id);
                }
            }
        }
        !(layout.is_empty() && groups.is_empty())
    }
}

/// The values of a map that read as `T`; the others (a client writing
/// something else) are skipped.
fn read_map<T: serde::de::DeserializeOwned, R: ReadTxn>(
    map: &MapRef,
    txn: &R,
) -> BTreeMap<String, T> {
    let Any::Map(entries) = map.to_json(txn) else {
        return BTreeMap::new();
    };
    entries
        .iter()
        .filter_map(|(key, value)| Some((key.clone(), from_any(value)?)))
        .collect()
}

fn from_any<T: serde::de::DeserializeOwned>(value: &Any) -> Option<T> {
    let mut json = String::new();
    value.to_json(&mut json);
    serde_json::from_str(&json).ok()
}

fn to_any<T: Serialize>(value: &T) -> Any {
    serde_json::to_string(value)
        .ok()
        .and_then(|json| Any::from_json(&json).ok())
        .unwrap_or(Any::Null)
}

/// The first update of a new draft: its SQL and presentation, as one Yjs
/// update. Written once, by the server, so no two clients ever insert the
/// same starting text.
pub fn initial_update(sql: &str, presentation: &VersionLayout) -> Vec<u8> {
    initial_update_in(DraftMode::Schema, sql, "", presentation)
}

/// [`initial_update`] of a draft in `mode`, with its migration's text (a
/// migration draft starts with one inviting to write it).
pub fn initial_update_in(
    mode: DraftMode,
    sql: &str,
    migration: &str,
    presentation: &VersionLayout,
) -> Vec<u8> {
    let doc = new_doc();
    let roots = Roots::of(&doc);
    {
        let mut txn = doc.transact_mut();
        roots.sql.insert(&mut txn, 0, sql);
        if !migration.is_empty() {
            roots.migration.insert(&mut txn, 0, migration);
        }
        if mode != DraftMode::Schema {
            roots.set_mode(&mut txn, mode);
        }
        for (name, position) in &presentation.layout {
            roots
                .layout
                .insert(&mut txn, name.as_str(), to_any(position));
        }
        for (id, group) in &presentation.groups {
            roots.groups.insert(&mut txn, id.as_str(), to_any(group));
        }
    }
    let txn = doc.transact();
    txn.encode_state_as_update_v1(&StateVector::default())
}

/// The content of a doc state (a snapshot plus updates), without a room:
/// tests and tools.
pub fn read(updates: &[Vec<u8>]) -> Result<(String, VersionLayout), yrs::encoding::read::Error> {
    let doc = new_doc();
    let roots = Roots::of(&doc);
    {
        let mut txn = doc.transact_mut();
        for update in updates {
            let _ = txn.apply_update(Update::decode_v1(update)?);
        }
    }
    let txn = doc.transact();
    Ok((roots.sql(&txn), roots.layout(&txn)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::versions::layout::TablePosition;

    fn presentation() -> VersionLayout {
        VersionLayout {
            layout: BTreeMap::from([
                ("public.users".into(), TablePosition { x: 10.0, y: 20.0 }),
                ("public.orders".into(), TablePosition { x: 300.0, y: 20.5 }),
            ]),
            groups: BTreeMap::from([(
                "g1".into(),
                TableGroup {
                    name: "Billing".into(),
                    color: "amber".into(),
                    members: vec!["public.orders".into(), "public.users".into()],
                },
            )]),
        }
    }

    #[test]
    fn the_initial_update_holds_the_text_and_presentation() {
        let update = initial_update("CREATE TABLE users ();", &presentation());
        let (sql, read_back) = read(&[update]).unwrap();
        assert_eq!(sql, "CREATE TABLE users ();");
        assert_eq!(read_back, presentation());
    }

    #[test]
    fn a_rename_moves_the_position_and_group_membership() {
        let doc = new_doc();
        let roots = Roots::of(&doc);
        {
            let mut txn = doc.transact_mut();
            txn.apply_update(Update::decode_v1(&initial_update("", &presentation())).unwrap())
                .unwrap();
        }
        let renames = [("public.users".to_owned(), "public.members".to_owned())];
        assert!(roots.rename(&mut doc.transact_mut(), &renames));
        let after = roots.layout(&doc.transact());
        assert_eq!(
            after.layout["public.members"],
            TablePosition { x: 10.0, y: 20.0 }
        );
        assert!(!after.layout.contains_key("public.users"));
        assert_eq!(
            after.groups["g1"].members,
            ["public.orders", "public.members"]
        );
    }

    #[test]
    fn rewrites_are_counted_with_their_reason() {
        let doc = new_doc();
        let roots = Roots::of(&doc);
        assert_eq!(roots.rewrites(&doc.transact()), None);
        roots.note_rewrite(&mut doc.transact_mut(), "tidy");
        roots.note_rewrite(&mut doc.transact_mut(), "update");
        assert_eq!(
            roots.rewrites(&doc.transact()),
            Some(RewriteNote {
                count: 2,
                reason: "update".into()
            })
        );
    }

    #[test]
    fn markers_after_the_first_are_extra_with_the_blank_line_above() {
        let m = GRAPH_EDITS_MARKER;
        let sql = format!("A;\n\n{m}\nX;\n\n{m}\nY;\n{m}\nZ;\n");
        let extra = extra_markers(&sql);
        let mut kept = sql.clone();
        for range in extra.iter().rev() {
            kept.replace_range(range.clone(), "");
        }
        assert_eq!(kept, format!("A;\n\n{m}\nX;\nY;\nZ;\n"));
        let none: Vec<std::ops::Range<usize>> = Vec::new();
        assert_eq!(extra_markers(&format!("A;\n\n{m}\nX;\n")), none);
        // Only whole lines count.
        assert_eq!(extra_markers(&format!("{m}\n-- not {m}\n")), none);
    }

    #[test]
    fn a_rename_never_overwrites_a_position() {
        let doc = new_doc();
        let roots = Roots::of(&doc);
        {
            let mut txn = doc.transact_mut();
            txn.apply_update(Update::decode_v1(&initial_update("", &presentation())).unwrap())
                .unwrap();
        }
        let renames = [("public.users".to_owned(), "public.orders".to_owned())];
        roots.rename(&mut doc.transact_mut(), &renames);
        let after = roots.layout(&doc.transact());
        assert_eq!(
            after.layout["public.orders"],
            TablePosition { x: 300.0, y: 20.5 }
        );
        // Its group membership still follows the name, once.
        assert_eq!(after.groups["g1"].members, ["public.orders"]);
    }
}
