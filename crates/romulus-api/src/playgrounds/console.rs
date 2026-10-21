//! The console's websocket: `GET …/playgrounds/{id}/console` [member].
//! It checks `Origin` (`csrf::SameOrigin`) and the membership, then
//! proxies to the playground service, which runs one Postgres session per
//! socket as the playground's read-only role (docs/playground.md, "Console
//! API"). The API:
//!
//! - re-encodes what the browser sends ([`ConsoleRequest`] only, its SQL at
//!   most `limits::SQL_BYTES`), signs the session with the person's access
//!   (reviewers read, with the shorter deadline);
//! - limits each person's runs: a rate (`rate::Limits::playground_console`)
//!   and [`super::RUNS_PER_PERSON`] in flight across their tabs (a refused
//!   run is answered here, as a `finished` with the reason);
//! - records each run that ran a statement as its `finished` event passes
//!   (the shared history: never results) and tells everyone in the
//!   playground
//!   ([`ConsoleNotice::History`]), with who is there
//!   ([`ConsoleNotice::Presence`]) and resets, migration runs and deletion
//!   ([`ConsoleNotice::Announcement`]);
//! - records in the audit log a restart the host made for this session (a
//!   role changed from inside the playground: `restarted`), once;
//! - closes the socket on a membership change (4001), a sign-out (4401), the
//!   playground's deletion (4404) and shutdown (1012), as collab does.
//!
//! At most [`super::SESSIONS_PER_PERSON`] consoles per person and playground.

use std::{collections::HashMap, time::Duration};

use axum::{
    extract::{
        FromRequestParts, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::request::Parts,
    response::Response,
};
use axum_extra::extract::cookie::CookieJar;
use romulus_playground::protocol::{ConsoleEvent, ConsoleRequest, RunStatus, limits};
use tokio::{
    sync::broadcast::{self, error::RecvError},
    time::Instant,
};
use uuid::Uuid;

use super::{
    Announcement, ConsoleNotice, RUNS_PER_PERSON, RunGuard, details, find, ready,
    runs::{self, NewRun},
    touch,
};
use crate::{
    ApiError, ApiResult, AppState, audit,
    auth::{Member, session},
    csrf::SameOrigin,
    databases,
    domain::{AuditAction, PlaygroundRunKind, PlaygroundRunStatus},
    drafts::Person,
    error::ErrorBody,
    extract::Path,
    orgs::events::MembershipChange,
    token,
};

mod close {
    pub const RESTART: u16 = 1012;
    pub const UNAVAILABLE: u16 = 1011;
    pub const MEMBERSHIP: u16 = 4001;
    pub const SESSION: u16 = 4401;
    pub const DELETED: u16 = 4404;
    pub const TOO_MANY: u16 = 4429;
}

/// How often the session is checked against the database (it may expire).
const SESSION_CHECK: Duration = Duration::from_mins(1);

/// Membership changes and sign-outs from the moment the request arrives
/// (taken before [`Member`] reads the membership, as collab's socket does).
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

/// Opens the playground's console: one Postgres session (read-only) per
/// socket. Text frames: `ConsoleRequest` in, `ConsoleEvent` and
/// `ConsoleNotice` out. Close codes: 1011 the host is unavailable, 1012
/// server restarting, 4001 membership changed (reconnect), 4401 session
/// ended, 4404 deleted, 4429 too many consoles open.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/playgrounds/{id}/console",
    operation_id = "playground_console",
    tag = "playgrounds",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    responses(
        (status = 101, description = "Switching to the websocket", body = ConsoleEvent),
        (status = 403, description = "Another origin", body = ErrorBody),
        (status = 402, description = "`plan_limit`: the organization used its playground hours (hosted billing)", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`playground_not_ready`, `playground_expired`", body = ErrorBody),
        (status = 503, description = "`host_unavailable`", body = ErrorBody),
    )
)]
#[allow(clippy::too_many_arguments, reason = "an upgrade handler's extractors")]
pub(super) async fn connect(
    watch: Watch,
    _: SameOrigin,
    member: Member,
    jar: CookieJar,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
    upgrade: WebSocketUpgrade,
) -> ApiResult<Response> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    ready(&row)?;
    state.playgrounds.host.backend()?;
    // Connecting starts a stopped playground: not past the organization's
    // playground hours.
    crate::billing::hours::check(&state, member.org.id).await?;
    let session = jar
        .get(&state.config.session_cookie)
        .map(|cookie| token::hash(cookie.value()))
        .ok_or(ApiError::Unauthorized)?;
    let socket = Socket {
        state: state.clone(),
        watch,
        session,
        playground: id,
        org: member.org.id,
        database: database.id,
        version: row.version_number,
        audited: details(&row),
        person: Person {
            id: member.user.id,
            name: member.user.name.clone(),
        },
        caller: row.caller(&member),
    };
    Ok(upgrade
        .max_message_size(limits::SQL_BYTES + 4096)
        .on_upgrade(move |ws| socket.run(ws)))
}

struct Socket {
    state: AppState,
    watch: Watch,
    session: Vec<u8>,
    playground: Uuid,
    org: Uuid,
    database: Uuid,
    version: i32,
    /// What the audit log keeps about the playground.
    audited: audit::AuditDetails,
    person: Person,
    caller: super::host::Caller,
}

/// What the API remembers of a run until its `finished` arrives.
struct Pending {
    sql: String,
    explain: bool,
    /// Counts against the person's runs in flight.
    _counted: RunGuard,
}

async fn shut(mut ws: WebSocket, code: u16, reason: &str) {
    let _ = ws
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

fn text(value: &impl serde::Serialize) -> Message {
    Message::Text(serde_json::to_string(value).unwrap_or_default().into())
}

/// A request from the browser, checked; `None` for anything else.
fn checked(frame: &str) -> Option<ConsoleRequest> {
    let request: ConsoleRequest = serde_json::from_str(frame).ok()?;
    let id_ok = |id: &str| !id.is_empty() && id.len() <= 64;
    match &request {
        ConsoleRequest::Run { id, sql, .. } => {
            (id_ok(id) && sql.len() <= limits::SQL_BYTES).then_some(request)
        }
        ConsoleRequest::Cancel { id } => id_ok(id).then_some(request),
        ConsoleRequest::Open => Some(request),
    }
}

/// A run refused here, as the host would finish it.
fn refused(id: &str, message: String) -> Message {
    text(&ConsoleEvent::Finished {
        id: id.to_owned(),
        status: RunStatus::Error,
        statements: 0,
        rows: None,
        ms: 0,
        error: Some(message),
    })
}

impl Socket {
    #[allow(clippy::too_many_lines, reason = "the socket's one event loop")]
    async fn run(mut self, mut ws: WebSocket) {
        let playgrounds = self.state.playgrounds.clone();
        let Some(_seat) = playgrounds.sit(self.playground, self.person.clone()) else {
            shut(
                ws,
                close::TOO_MANY,
                "you have this playground open in too many tabs",
            )
            .await;
            return;
        };
        let mut notices = playgrounds.subscribe(self.playground);
        let mut closing = playgrounds.closing();
        let link = match playgrounds.host.backend() {
            Ok(backend) => backend.console(self.playground, self.caller.clone()).await,
            Err(err) => Err(err),
        };
        let mut link = match link {
            Ok(link) => link,
            Err(err) => {
                tracing::warn!(playground = %self.playground, error = %err, "console: cannot reach the host");
                let _ = ws
                    .send(text(&ConsoleEvent::Paused {
                        reason: "The playground host is not answering. Try again in a moment."
                            .to_owned(),
                    }))
                    .await;
                shut(ws, close::UNAVAILABLE, "the playground host is unavailable").await;
                return;
            }
        };
        let _ = touch(&self.state.db, self.playground).await;
        let _ = ws
            .send(text(&ConsoleNotice::Presence {
                people: playgrounds.present(self.playground),
            }))
            .await;
        let mut pending: HashMap<String, Pending> = HashMap::new();
        let mut check = tokio::time::interval(SESSION_CHECK);
        check.reset();
        let mut ping = tokio::time::interval(Duration::from_secs(25));
        ping.reset();
        let mut heard = Instant::now();

        let closing_with = loop {
            tokio::select! {
                frame = ws.recv() => {
                    heard = Instant::now();
                    match frame {
                        Some(Ok(Message::Text(frame))) => {
                            let Some(request) = checked(&frame) else {
                                break Some((1003, "console requests only"));
                            };
                            if let ConsoleRequest::Run { id, sql, explain } = &request {
                                if pending.len() >= 8 {
                                    continue;
                                }
                                if let Err(ApiError::RateLimited { retry_after }) = self.state.limits.playground_console.check(self.person.id) {
                                    let message = format!(
                                        "You are running queries faster than the playground allows: wait {} s.",
                                        retry_after.as_secs().max(1)
                                    );
                                    if ws.send(refused(id, message)).await.is_err() {
                                        break None;
                                    }
                                    continue;
                                }
                                // A run starts a stopped playground: not past the
                                // organization's playground hours.
                                if let Err(ApiError::PlanLimit { message, .. }) =
                                    crate::billing::hours::check(&self.state, self.org).await
                                {
                                    if ws.send(refused(id, message)).await.is_err() {
                                        break None;
                                    }
                                    continue;
                                }
                                let Some(counted) = playgrounds.start_run(self.person.id) else {
                                    let message = format!("You have {RUNS_PER_PERSON} runs in progress (in this tab or others): wait for one to finish.");
                                    if ws.send(refused(id, message)).await.is_err() {
                                        break None;
                                    }
                                    continue;
                                };
                                pending.insert(id.clone(), Pending { sql: sql.clone(), explain: *explain, _counted: counted });
                            }
                            // `open` starts a stopped playground, as a run does.
                            if matches!(request, ConsoleRequest::Open)
                                && let Err(ApiError::PlanLimit { message, .. }) =
                                    crate::billing::hours::check(&self.state, self.org).await
                            {
                                let refusal = ConsoleEvent::Unavailable { code: "plan_limit".into(), reason: message };
                                if ws.send(text(&refusal)).await.is_err() {
                                    break None;
                                }
                                continue;
                            }
                            let Ok(encoded) = serde_json::to_string(&request) else { continue };
                            if link.to_host.send(encoded).await.is_err() {
                                break Some((close::UNAVAILABLE, "the playground host went away"));
                            }
                        }
                        Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                        Some(Ok(Message::Binary(_))) => break Some((1003, "text frames only")),
                        Some(Ok(Message::Close(_)) | Err(_)) | None => break None,
                    }
                }
                from_host = link.from_host.recv() => match from_host {
                    Some(frame) => {
                        // Results pass as they are; the end of a run is recorded.
                        if frame.starts_with(r#"{"type":"restarted""#) {
                            self.audit_restart().await;
                        }
                        if frame.starts_with(r#"{"type":"finished""#)
                            && let Ok(ConsoleEvent::Finished { id, status, statements, rows, ms, error }) = serde_json::from_str(&frame)
                            && let Some(run) = pending.remove(&id)
                            // A run that ran nothing (refused, a syntax
                            // error) is the person's own business.
                            && statements > 0
                        {
                            self.record(&run, status, rows, ms, error.as_deref()).await;
                        }
                        if ws.send(Message::Text(frame.into())).await.is_err() {
                            break None;
                        }
                    }
                    None => break Some((close::UNAVAILABLE, "the playground host went away")),
                },
                notice = notices.recv() => match notice {
                    Ok(notice) => {
                        let deleted = matches!(notice, ConsoleNotice::Announcement { event: Announcement::Deleted, .. });
                        if ws.send(text(&notice)).await.is_err() {
                            break None;
                        }
                        if deleted {
                            break Some((close::DELETED, "the playground was deleted"));
                        }
                    }
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => break Some((close::RESTART, "the server is restarting")),
                },
                change = self.watch.membership.recv() => match change {
                    Ok(change) if !change.concerns(self.org, self.person.id) => {}
                    Ok(_) | Err(RecvError::Lagged(_) | RecvError::Closed) => {
                        break Some((close::MEMBERSHIP, "your access to this organization changed"));
                    }
                },
                ended = self.watch.sessions.recv() => match ended {
                    Ok(hash) if hash != self.session => {}
                    Ok(_) => break Some((close::SESSION, "signed out")),
                    Err(RecvError::Lagged(_)) => check.reset_immediately(),
                    Err(RecvError::Closed) => break Some((close::RESTART, "the server is restarting")),
                },
                _ = check.tick() => {
                    if matches!(session::is_live(&self.state.db, &self.session).await, Ok(false)) {
                        break Some((close::SESSION, "the session expired"));
                    }
                }
                _ = ping.tick() => {
                    if heard.elapsed() > Duration::from_secs(70) {
                        break Some((4002, "no answer"));
                    }
                    if ws.send(Message::Ping(axum::body::Bytes::new())).await.is_err() {
                        break None;
                    }
                }
                _ = closing.changed() => break Some((close::RESTART, "the server is restarting")),
            }
        };
        // Dropping the link ends the host's session (its transaction rolls back).
        drop(link);
        if let Some((code, reason)) = closing_with {
            shut(ws, code, reason).await;
        }
    }

    /// The host restarted the playground for this session: a role was
    /// changed from inside it. Recorded as romulus's doing.
    async fn audit_restart(&self) {
        let recorded = audit::record(
            &self.state.db,
            &audit::Entry {
                org_id: self.org,
                database_id: Some(self.database),
                action: AuditAction::PlaygroundRestarted,
                actor_id: None,
                actor_name: None,
                version_number: Some(self.version),
                reason: None,
                details: self.audited.clone(),
            },
        )
        .await;
        if let Err(err) = recorded {
            tracing::warn!(playground = %self.playground, error = %err, "cannot audit a restart");
        }
    }

    async fn record(
        &self,
        run: &Pending,
        status: RunStatus,
        rows: Option<u64>,
        ms: u64,
        error: Option<&str>,
    ) {
        let recorded = runs::record_console(
            &self.state.db,
            NewRun {
                playground: self.playground,
                user: (self.person.id, &self.person.name),
                kind: if run.explain {
                    PlaygroundRunKind::Explain
                } else {
                    PlaygroundRunKind::Query
                },
                sql: Some(&run.sql),
                duration_ms: ms,
                status: match status {
                    RunStatus::Ok => PlaygroundRunStatus::Ok,
                    RunStatus::Error => PlaygroundRunStatus::Error,
                    RunStatus::Cancelled => PlaygroundRunStatus::Cancelled,
                },
                rows,
                error,
                report: None,
                migration_input_sha256: None,
            },
        )
        .await;
        match recorded {
            Ok(run) => {
                let _ = touch(&self.state.db, self.playground).await;
                self.state
                    .playgrounds
                    .tell(self.playground, ConsoleNotice::History { run });
            }
            Err(err) => {
                tracing::warn!(playground = %self.playground, error = %err, "cannot record a console run");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_console_requests_within_limits_pass() {
        assert!(checked(r#"{"type":"run","id":"r1","sql":"select 1"}"#).is_some());
        assert!(checked(r#"{"type":"cancel","id":"r1"}"#).is_some());
        assert!(checked(r#"{"type":"run","id":"","sql":"x"}"#).is_none());
        assert!(checked(&format!(r#"{{"type":"cancel","id":"{}"}}"#, "x".repeat(65))).is_none());
        let huge = format!(
            r#"{{"type":"run","id":"r","sql":"{}"}}"#,
            "x".repeat(limits::SQL_BYTES + 1)
        );
        assert!(checked(&huge).is_none());
        assert!(checked(r#"{"type":"terminate","pid":1}"#).is_none());
        assert!(checked("not json").is_none());
    }
}
