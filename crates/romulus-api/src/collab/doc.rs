//! A draft's Y.Doc: what it holds and how the server reads and writes it.
//!
//! docs/MVP.md "The core design":
//!
//! ```text
//! sql:     Y.Text                          canonical DDL
//! layout:  Y.Map<qualified name, {x, y}>   positions, in graph coordinates
//! groups:  Y.Map<group id, {name, color, members: qualified names[]}>
//! ```
//!
//! Map values are plain JSON objects (Yjs `Any`), not nested Y.Maps: a move
//! or a group edit replaces the whole value, last writer wins per key.
//!
//! The server writes to a doc in three places only: the initial content of a
//! new draft, moving a table's layout and group entries when a rename is
//! typed in the text (see `room`), and rewriting the SQL as a minimal edit
//! (Tidy, see `patch`). Everything else comes from clients.

use std::collections::BTreeMap;

use serde::Serialize;
use yrs::{
    Any, Doc, GetString, Map, MapRef, OffsetKind, Options, ReadTxn, StateVector, Text, TextRef,
    Transact, TransactionMut, Update, types::ToJson, updates::decoder::Decode,
};

use crate::versions::layout::{TableGroup, VersionLayout};

pub const SQL: &str = "sql";
pub const LAYOUT: &str = "layout";
pub const GROUPS: &str = "groups";

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
    pub layout: MapRef,
    pub groups: MapRef,
}

impl Roots {
    pub fn of(doc: &Doc) -> Self {
        Self {
            sql: doc.get_or_insert_text(SQL),
            layout: doc.get_or_insert_map(LAYOUT),
            groups: doc.get_or_insert_map(GROUPS),
        }
    }

    pub fn sql<T: ReadTxn>(&self, txn: &T) -> String {
        self.sql.get_string(txn)
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
    let doc = new_doc();
    let roots = Roots::of(&doc);
    {
        let mut txn = doc.transact_mut();
        roots.sql.insert(&mut txn, 0, sql);
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
