//! The messages on a draft's websocket: y-sync (as y-websocket speaks it)
//! plus one message type of our own.
//!
//! | tag | what | from |
//! |---|---|---|
//! | 0 | sync: step 1 (state vector), step 2 and update (Yjs updates) | both |
//! | 1 | awareness update | both; the server stamps `user` (see `awareness`) |
//! | 3 | awareness query | client |
//! | 100 | [`ServerEvent`], JSON | **server only** |
//!
//! Compile results, the draft's status and renames travel as
//! [`ServerEvent`]s rather than in the Y.Doc (a `compiled` Y.Map). A Y.Map is
//! writable by every editor, so the server would have to inspect each update
//! to keep clients from forging a result; a message type the server never
//! relays cannot be forged. And a model of a few hundred kilobytes per
//! compile would grow the persisted history of every draft for nothing:
//! only the latest result matters, and `draft_compiles` keeps it.

use std::sync::Arc;

use axum::body::Bytes;
use serde::Serialize;
use utoipa::ToSchema;
use yrs::{
    StateVector,
    encoding::read::Cursor,
    sync::{Message, MessageReader, SyncMessage, awareness::AwarenessUpdate},
    updates::{decoder::DecoderV1, encoder::Encode},
};

use super::compile::CompileState;
use crate::domain::Role;

/// The tag of [`ServerEvent`] messages.
pub const MESSAGE_EVENT: u8 = 100;

/// The messages in one websocket frame (y-websocket sends one per frame;
/// the protocol allows several). Decoding stops at the first malformed one.
pub fn decode(frame: &[u8]) -> Result<Vec<Message>, yrs::encoding::read::Error> {
    let mut decoder = DecoderV1::new(Cursor::new(frame));
    MessageReader::new(&mut decoder).collect()
}

pub fn sync_step1(state_vector: &StateVector) -> Bytes {
    Message::Sync(SyncMessage::SyncStep1(state_vector.clone()))
        .encode_v1()
        .into()
}

pub fn sync_step2(update: Vec<u8>) -> Bytes {
    Message::Sync(SyncMessage::SyncStep2(update))
        .encode_v1()
        .into()
}

pub fn update(update: Vec<u8>) -> Bytes {
    Message::Sync(SyncMessage::Update(update))
        .encode_v1()
        .into()
}

pub fn awareness(update: AwarenessUpdate) -> Bytes {
    Message::Awareness(update).encode_v1().into()
}

pub fn event(event: &ServerEvent) -> Bytes {
    // Serialising plain data cannot fail.
    let json = serde_json::to_vec(event).unwrap_or_default();
    Message::Custom(MESSAGE_EVENT, json).encode_v1().into()
}

/// Where a draft is in its life, as the people in it see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LiveStatus {
    /// Editable (by developers and admins).
    Open,
    /// Someone is publishing it: edits are refused until that succeeds
    /// (then `published`) or fails (back to `open`).
    Publishing,
    /// It became a version; nothing changes it any more.
    Published,
    /// Deleted: the editor should leave.
    Deleted,
}

/// A message only the server sends (tag 100 on the websocket), as JSON.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    /// The first event after connecting: how the server treats this
    /// connection.
    Welcome {
        /// The connection's role in the organization.
        role: Role,
        /// Document updates from this connection are dropped (reviewers,
        /// and everyone once the draft is no longer open).
        read_only: bool,
        status: LiveStatus,
    },
    /// The latest compile of the draft's SQL. Sent to everyone after each
    /// compile, and to each connection when it joins.
    Compile {
        state: CompileState,
        /// Hex SHA-256 of the SQL compiled. A client whose text hashes the
        /// same can place `diagnostics` as they are; otherwise a newer
        /// compile is on its way.
        sha256: Option<String>,
        /// Errors (when `failed`), then notes. Spans count UTF-16 code
        /// units in `utf16_start`/`utf16_end`, like `Y.Text` in the browser.
        diagnostics: Vec<romulus_sandbox::Diagnostic>,
        /// The model when `ok`. When `failed`, the last model that compiled,
        /// sent only on joining (others already have it); keep showing the
        /// previous one when absent.
        #[schema(value_type = Option<Object>)]
        model: Option<Arc<serde_json::Value>>,
    },
    /// The draft's status changed (publishing started, failed or
    /// succeeded; the draft was deleted).
    Status {
        status: LiveStatus,
        /// The version it became, once `published`.
        version: Option<i32>,
        /// Who published or deleted it.
        by: Option<String>,
    },
    /// The draft was renamed.
    Renamed { name: String },
    /// The database has a new version (another draft was published): if
    /// it is not this draft's base, publishing now needs "Update draft".
    VersionCreated { number: i32 },
}

#[cfg(test)]
mod tests {
    use yrs::updates::decoder::Decode;

    use super::*;

    #[test]
    fn events_are_tag_100_with_a_json_body() {
        let bytes = event(&ServerEvent::Renamed { name: "x".into() });
        let messages = decode(&bytes).unwrap();
        let [Message::Custom(MESSAGE_EVENT, body)] = messages.as_slice() else {
            panic!("{messages:?}");
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(body).unwrap(),
            serde_json::json!({"type": "renamed", "name": "x"})
        );
    }

    #[test]
    fn sync_messages_round_trip() {
        let messages = decode(&update(vec![0, 0])).unwrap();
        assert_eq!(messages, [Message::Sync(SyncMessage::Update(vec![0, 0]))]);
        let sv = StateVector::decode_v1(&StateVector::default().encode_v1()).unwrap();
        assert_eq!(
            decode(&sync_step1(&sv)).unwrap(),
            [Message::Sync(SyncMessage::SyncStep1(sv))]
        );
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(decode(&[0, 9, 9]).is_err());
    }
}
