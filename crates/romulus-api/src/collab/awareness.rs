//! Awareness (who is here, their cursors and pointers), made trustworthy.
//!
//! Awareness states come from clients and are relayed to everyone in the
//! draft. docs/MVP.md "Roles": "The server stamps every awareness state with
//! the authenticated user's id, name and colour; what a client claims about
//! who it is is ignored." So each state is [`sanitize`]d before it is kept
//! or relayed: its `user` field is replaced by the connection's [`Peer`]
//! stamp, fields the editor does not use are dropped, and its size is
//! capped. The room also checks that a connection only speaks for the Yjs
//! client ids it announced first (see `room`), so nobody can move or hide
//! someone else's cursor.

use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::domain::Role;

/// Fields of an awareness state the editor reads; the rest is dropped.
///
/// - `cursor`: the text selection, as y-codemirror.next writes it
///   (`{anchor, head}`, Yjs relative positions);
/// - `pointer`: `{x, y}` in graph coordinates, or null off the graph;
/// - `selection`: the qualified name of the table selected in the graph.
const FIELDS: [&str; 3] = ["cursor", "pointer", "selection"];

/// Larger states are refused (a cursor and a pointer take a few hundred bytes).
pub const MAX_STATE_BYTES: usize = 8 * 1024;

/// Who is behind a connection: from the session and the membership, never
/// from the client.
#[derive(Debug, Clone)]
pub struct Peer {
    pub user_id: Uuid,
    pub name: String,
    pub role: Role,
    pub org_id: Uuid,
}

impl Peer {
    /// Reviewers watch: their document updates are dropped.
    pub fn can_edit(&self) -> bool {
        self.role.can_write()
    }

    /// The `user` field of every awareness state this peer sends. `color`
    /// and `colorLight` are what y-codemirror.next paints remote carets and
    /// selections with: the presence tokens, so they follow the theme.
    pub fn stamp(&self) -> Value {
        let n = presence_index(&self.user_id.to_string());
        json!({
            "id": self.user_id,
            "name": self.name,
            "role": self.role,
            "presence": n,
            "color": format!("var(--color-presence-{n})"),
            "colorLight": format!("color-mix(in oklab, var(--color-presence-{n}) 22%, transparent)"),
        })
    }
}

/// The collaborator color (1–8) of a user id: FNV-1a over the UTF-16 code
/// units, exactly as `presenceIndex` in the web app's avatar computes it.
pub fn presence_index(seed: &str) -> u32 {
    let hash = seed.encode_utf16().fold(0x811c_9dc5_u32, |hash, unit| {
        (hash ^ u32::from(unit)).wrapping_mul(0x0100_0193)
    });
    hash % 8 + 1
}

/// A client's awareness state as it may be relayed: a JSON object with the
/// editor's fields only and `user` set to `stamp`. `None` when the state is
/// not an object or is too large (the update is then ignored).
pub fn sanitize(json: &str, stamp: &Value) -> Option<String> {
    if json.len() > MAX_STATE_BYTES {
        return None;
    }
    let Value::Object(state) = serde_json::from_str(json).ok()? else {
        return None;
    };
    let mut kept: Map<String, Value> = state
        .into_iter()
        .filter(|(key, _)| FIELDS.contains(&key.as_str()))
        .collect();
    kept.insert("user".into(), stamp.clone());
    serde_json::to_string(&kept).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alice() -> Peer {
        Peer {
            user_id: Uuid::from_u128(7),
            name: "Alice".into(),
            role: Role::Developer,
            org_id: Uuid::nil(),
        }
    }

    #[test]
    fn the_user_field_is_always_the_server_s() {
        let forged = r#"{"user":{"id":"x","name":"Mallory","color":"red"},"cursor":{"anchor":1},"evil":"<script>"}"#;
        let state: Value =
            serde_json::from_str(&sanitize(forged, &alice().stamp()).unwrap()).unwrap();
        assert_eq!(state["user"]["name"], "Alice");
        assert_eq!(state["user"]["id"], Uuid::from_u128(7).to_string());
        assert_eq!(state["cursor"]["anchor"], 1);
        assert!(state.get("evil").is_none());
    }

    #[test]
    fn non_objects_and_huge_states_are_refused() {
        let stamp = alice().stamp();
        assert!(sanitize("[1]", &stamp).is_none());
        assert!(sanitize("not json", &stamp).is_none());
        let huge = format!(r#"{{"cursor":"{}"}}"#, "x".repeat(MAX_STATE_BYTES));
        assert!(sanitize(&huge, &stamp).is_none());
    }

    #[test]
    fn presence_colors_match_the_web_app() {
        // presenceIndex("alice") and presenceIndex("00000000-0000-0000-0000-000000000007")
        // in web/src/components/ui/avatar.tsx.
        assert_eq!(presence_index("alice"), 8);
        assert_eq!(presence_index("00000000-0000-0000-0000-000000000007"), 5);
        assert!((1..=8).contains(&presence_index("")));
    }
}
