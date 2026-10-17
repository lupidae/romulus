//! The websocket endpoint and the task that runs each socket.

use std::{panic::AssertUnwindSafe, time::Duration};

use axum::{
    extract::{
        FromRequestParts, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::request::Parts,
    response::Response,
};
use axum_extra::extract::cookie::CookieJar;
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use sqlx::PgPool;
use tokio::{
    sync::broadcast::{self, error::RecvError},
    time::Instant,
};
use uuid::Uuid;

use super::{
    Hub,
    awareness::Peer,
    protocol::{self, ServerEvent},
    room::{Budget, Incoming, Outgoing, close, queue},
};
use crate::{
    ApiError, ApiResult, AppState,
    auth::{Member, session},
    csrf::SameOrigin,
    domain::DraftStatus,
    error::ErrorBody,
    extract::Path,
    orgs::events::MembershipChange,
    token,
};

/// Largest frame accepted: a whole draft's text pasted at once, with room.
const MAX_FRAME: usize = 8 * 1024 * 1024;
/// How long a closing socket waits for the client's close frame.
const CLOSE_GRACE: Duration = Duration::from_secs(2);
/// Comment operations one frame may carry (a browser sends one); those past
/// it are refused unread.
pub const MAX_COMMENTS_PER_FRAME: usize = 8;

/// What a socket must hear about from the moment its request arrives:
/// membership changes and sign-outs. Taken as the handler's first extractor,
/// before [`Member`] reads the membership, so a change committed after that
/// read is delivered here, and one committed before it is what it reads:
/// none slips between the two.
pub(super) struct Watch {
    membership: broadcast::Receiver<MembershipChange>,
    sessions: broadcast::Receiver<Vec<u8>>,
}

impl FromRequestParts<AppState> for Watch {
    type Rejection = ApiError;

    fn from_request_parts(
        _: &mut Parts,
        state: &AppState,
    ) -> impl Future<Output = Result<Self, ApiError>> + Send {
        std::future::ready(Ok(Self {
            membership: state.membership.subscribe(),
            sessions: state.sessions.subscribe(),
        }))
    }
}

/// Opens a draft live: the y-sync protocol over a websocket.
///
/// Members only (reviewers watch: their document updates are dropped).
/// Messages: y-sync (sync, awareness) plus server events (tag 100, see the
/// `ServerEvent` schema). Close codes: 1012 server restarting, 4000 too
/// slow, 4001 membership changed, 4002 no answer to pings (reconnect);
/// 4401 session ended, 4404 draft deleted, 4413 draft too large (do not).
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
    watch: Watch,
    _: SameOrigin,
    member: Member,
    jar: CookieJar,
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
    // `Member` found the session through this cookie.
    let session = jar
        .get(&state.config.session_cookie)
        .map(|cookie| token::hash(cookie.value()))
        .ok_or(ApiError::Unauthorized)?;
    let peer = Peer {
        user_id: member.user.id,
        name: member.user.name,
        role: member.role,
        org_id: member.org.id,
    };
    let socket = Socket {
        hub: state.collab.clone(),
        db: state.db.clone(),
        watch,
        session,
        draft: id,
    };
    Ok(upgrade
        .max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |ws| socket.run(peer, ws)))
}

struct Socket {
    hub: Hub,
    db: PgPool,
    watch: Watch,
    /// The `id_hash` of the session that opened it.
    session: Vec<u8>,
    draft: Uuid,
}

impl Socket {
    async fn run(mut self, peer: Peer, ws: WebSocket) {
        let (sink, mut stream) = ws.split();
        let settings = self.hub.settings().clone();
        let (out, mut inbox) = queue(settings.max_queued_bytes);
        let (org_id, user_id) = (peer.org_id, peer.user_id);
        let connection = match self.hub.join(self.draft, peer, out).await {
            Ok(connection) => connection,
            Err(err) => {
                tracing::warn!(draft = %self.draft, error = %err, "collab: cannot join");
                let code = if matches!(err, super::HubError::Closing) {
                    close::RESTART
                } else {
                    close::UNAVAILABLE
                };
                shut(sink, stream, code, "the draft is unavailable").await;
                return;
            }
        };
        let mut sink = sink;
        let mut ping = tokio::time::interval(settings.ping_every);
        ping.reset();
        let mut check = tokio::time::interval(settings.session_check);
        check.reset();
        let mut heard = Instant::now();
        // Comment operations this socket may still have looked up (before
        // any query; the room budgets them per person again).
        let mut comments = Budget::new();

        let closing = loop {
            tokio::select! {
                frame = stream.next() => {
                    heard = Instant::now();
                    match frame {
                        Some(Ok(Message::Binary(data))) => match decode(&data) {
                            // Waits while the room is busy: this socket then
                            // reads no more (backpressure on the client). A
                            // room that is gone left its last word (a close)
                            // in the inbox, or closed it.
                            Some(mut messages) => {
                                ration(&mut messages, &mut comments);
                                if members_only(&self.db, org_id, &mut messages).await.is_err() {
                                    break Some((close::UNAVAILABLE, "the database is unavailable"));
                                }
                                let _ = connection.incoming(messages).await;
                            }
                            None => connection.malformed(),
                        },
                        Some(Ok(Message::Text(_))) => {
                            break Some((close::PROTOCOL, "binary y-sync only"));
                        }
                        Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                        Some(Ok(Message::Close(_)) | Err(_)) | None => break None,
                    }
                }
                out = inbox.recv() => match out {
                    Some(Outgoing::Frame(frame)) => {
                        if sink.send(Message::Binary(frame)).await.is_err() {
                            break None;
                        }
                    }
                    Some(Outgoing::Close(code, reason)) => break Some((code, reason)),
                    // The room let this connection go (unloaded).
                    None => break Some((close::LAGGING, "reconnect")),
                },
                change = self.watch.membership.recv() => match change {
                    Ok(change) if !change.concerns(org_id, user_id) => {}
                    // Missed messages may have named this socket: close to be safe.
                    Ok(_) | Err(RecvError::Lagged(_) | RecvError::Closed) => {
                        break Some((close::MEMBERSHIP, "your access to this organization changed"));
                    }
                },
                ended = self.watch.sessions.recv() => match ended {
                    Ok(hash) if hash != self.session => {}
                    Ok(_) => break Some((close::SESSION, "signed out")),
                    // Missed some: ask the database instead.
                    Err(RecvError::Lagged(_)) => check.reset_immediately(),
                    Err(RecvError::Closed) => break Some((close::RESTART, "the server is restarting")),
                },
                _ = check.tick() => {
                    if matches!(session::is_live(&self.db, &self.session).await, Ok(false)) {
                        break Some((close::SESSION, "the session expired"));
                    }
                }
                _ = ping.tick() => {
                    if heard.elapsed() > settings.ping_every * 5 / 2 {
                        break Some((close::SILENT, "no answer"));
                    }
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
        drop(inbox);
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

/// Refuses, unread, the comment operations of a frame past
/// [`MAX_COMMENTS_PER_FRAME`] or past the socket's budget: nothing is looked
/// up for them.
fn ration(messages: &mut [Incoming], budget: &mut Budget) {
    let mut seen = 0;
    for message in messages {
        if let Incoming::Comment(request) = message {
            seen += 1;
            if seen > MAX_COMMENTS_PER_FRAME || !budget.take() {
                *message = Incoming::CommentRefused {
                    id: std::mem::take(&mut request.id),
                };
            }
        }
    }
}

/// Keeps, in the comment operations of `messages`, only the mentions of
/// members of the organization (the room has no database to ask): one
/// query for the whole frame.
async fn members_only(
    db: &PgPool,
    org: Uuid,
    messages: &mut [Incoming],
) -> Result<(), sqlx::Error> {
    let mut asked: Vec<Uuid> = Vec::new();
    for message in messages.iter_mut() {
        if let Incoming::Comment(request) = message
            && let Some(mentions) = request.op.mentions_mut()
        {
            asked.extend(mentions.iter().copied());
        }
    }
    if asked.is_empty() {
        return Ok(());
    }
    asked.sort_unstable();
    asked.dedup();
    let members = crate::versions::discussion::members_among(db, org, &asked).await?;
    for message in messages {
        if let Incoming::Comment(request) = message
            && let Some(mentions) = request.op.mentions_mut()
        {
            mentions.retain(|id| members.contains(id));
        }
    }
    Ok(())
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
