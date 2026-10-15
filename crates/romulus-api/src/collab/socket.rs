//! The websocket endpoint and the task that runs each socket.

use std::{panic::AssertUnwindSafe, time::Duration};

use axum::{
    extract::{
        State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    response::Response,
};
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use tokio::sync::{broadcast::error::RecvError, mpsc};
use uuid::Uuid;

use super::{
    Hub,
    awareness::Peer,
    protocol::{self, ServerEvent},
    room::{Command, Incoming, Outgoing, close},
};
use crate::{
    ApiError, ApiResult, AppState, auth::Member, csrf::SameOrigin, domain::DraftStatus,
    error::ErrorBody, extract::Path, orgs::events::MembershipEvents,
};

/// Largest frame accepted: a whole draft's text pasted at once, with room.
const MAX_FRAME: usize = 8 * 1024 * 1024;
/// Frames queued for a socket before it counts as too slow.
const QUEUE: usize = 1024;
/// Keeps proxies from closing a quiet socket.
const PING_EVERY: Duration = Duration::from_secs(25);
/// How long a closing socket waits for the client's close frame.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// Opens a draft live: the y-sync protocol over a websocket.
///
/// Members only (reviewers watch: their document updates are dropped).
/// Messages: y-sync (sync, awareness) plus server events (tag 100, see the
/// `ServerEvent` schema). Close codes: 1012 server restarting, 4000 too
/// slow, 4001 membership changed (reconnect), 4404 draft deleted (do not).
#[utoipa::path(
    get,
    path = "/orgs/{org}/drafts/{id}/ws",
    operation_id = "draft_socket",
    tag = "drafts",
    params(
        ("org" = String, Path, description = "Organization slug"),
        ("id" = Uuid, Path, description = "Draft id"),
    ),
    responses(
        (status = 101, description = "Switching to the websocket", body = ServerEvent),
        (status = 403, description = "Another origin", body = ErrorBody),
        (status = 404, description = "No such draft, or not a member", body = ErrorBody),
        (status = 409, description = "The draft is published: open its version instead", body = ErrorBody),
    )
)]
pub(super) async fn connect(
    _: SameOrigin,
    member: Member,
    Path((_, id)): Path<(String, Uuid)>,
    State(state): State<AppState>,
    upgrade: WebSocketUpgrade,
) -> ApiResult<Response> {
    let draft = sqlx::query!(
        r#"SELECT d.status AS "status: DraftStatus"
           FROM drafts d JOIN databases b ON b.id = d.database_id
           WHERE d.id = $1 AND b.org_id = $2 AND b.deleted_at IS NULL"#,
        id,
        member.org.id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("No such draft."))?;
    match draft.status {
        DraftStatus::Open => {}
        DraftStatus::Deleted => return Err(ApiError::not_found("No such draft.")),
        DraftStatus::Published => {
            return Err(ApiError::Conflict {
                code: "draft_published",
                message: "This draft was published. Open its version instead.".into(),
            });
        }
    }
    let peer = Peer {
        user_id: member.user.id,
        name: member.user.name,
        role: member.role,
        org_id: member.org.id,
    };
    let (hub, membership) = (state.collab.clone(), state.membership.clone());
    Ok(upgrade
        .max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| run(hub, membership, id, peer, socket)))
}

async fn run(hub: Hub, membership: MembershipEvents, draft: Uuid, peer: Peer, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (out, mut queue) = mpsc::channel(QUEUE);
    // Subscribed before joining: a change committed meanwhile is not missed.
    let mut changes = membership.subscribe();
    let (org_id, user_id) = (peer.org_id, peer.user_id);
    let connection = match hub.join(draft, peer, out).await {
        Ok(connection) => connection,
        Err(err) => {
            tracing::warn!(%draft, error = %err, "collab: cannot join");
            let code = if matches!(err, super::HubError::Closing) {
                close::RESTART
            } else {
                close::UNAVAILABLE
            };
            shut(sink, stream, code, "the draft is unavailable").await;
            return;
        }
    };
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.reset();

    let closing = loop {
        tokio::select! {
            frame = stream.next() => match frame {
                Some(Ok(Message::Binary(data))) => match decode(&data) {
                    Some(messages) => connection.send(Command::Incoming {
                        conn: connection.id,
                        messages,
                    }),
                    None => {
                        connection.send(Command::Malformed { conn: connection.id });
                    }
                },
                Some(Ok(Message::Text(_))) => break Some((close::PROTOCOL, "binary y-sync only")),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_)) | Err(_)) | None => break None,
            },
            out = queue.recv() => match out {
                Some(Outgoing::Frame(frame)) => {
                    if sink.send(Message::Binary(frame)).await.is_err() {
                        break None;
                    }
                }
                Some(Outgoing::Close(code, reason)) => break Some((code, reason)),
                // The room let this connection go (too slow, or unloaded).
                None => break Some((close::LAGGING, "reconnect")),
            },
            change = changes.recv() => match change {
                Ok(change) if !change.concerns(org_id, user_id) => {}
                // Missed messages may have named this socket: close to be safe.
                Ok(_) | Err(RecvError::Lagged(_) | RecvError::Closed) => {
                    break Some((close::MEMBERSHIP, "your access to this organization changed"));
                }
            },
            _ = ping.tick() => {
                if sink.send(Message::Ping(axum::body::Bytes::new())).await.is_err() {
                    break None;
                }
            }
        }
    };
    // Dropping the connection tells the room it left.
    drop(connection);
    if let Some((code, reason)) = closing {
        shut(sink, stream, code, reason).await;
    }
}

/// Sends a close frame, then reads until the client answers it (a moment at
/// most). Dropping a socket that still has unread data resets the
/// connection, and the client can lose the close frame, and with it the code
/// that says whether to reconnect.
async fn shut(
    mut sink: SplitSink<WebSocket, Message>,
    mut stream: SplitStream<WebSocket>,
    code: u16,
    reason: &'static str,
) {
    if sink.send(close_frame(code, reason)).await.is_err() {
        return;
    }
    let _ = tokio::time::timeout(CLOSE_GRACE, async {
        while let Some(Ok(message)) = stream.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
}

/// The y-sync messages of a frame, or `None` when it is not y-sync. yrs
/// asserts on some malformed input in debug builds; that only ever costs
/// this connection.
fn decode(frame: &[u8]) -> Option<Vec<Incoming>> {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let messages = protocol::decode(frame).ok()?;
        messages
            .into_iter()
            .filter_map(Incoming::from_message)
            .collect::<Result<Vec<_>, ()>>()
            .ok()
    }))
    .ok()
    .flatten()
}

fn close_frame(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }))
}
