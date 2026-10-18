//! The service itself: its HTTP server ([`router`], the calls of
//! [`crate::protocol`]), every route but `/health` behind the API's
//! signature ([`crate::signing`]).

#![allow(
    clippy::disallowed_types,
    reason = "romulus-api's JSON-error extractors are its own: this server answers protocol::ErrorBody"
)]

pub mod build;
pub mod console;
pub mod host;
pub mod migrate;
pub mod pg;
pub mod seed;

use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::stream;
use serde::Serialize;
use tokio::sync::mpsc;
use uuid::Uuid;

pub use self::host::{Config, Host, HostError};
use crate::{
    protocol::{Access, Build, ErrorBody, Health, Listing, Migrate, Status, limits},
    signing::{Claims, Op},
};

/// Largest body: a build's schema SQL and model.
const MAX_BODY: usize = 32 * 1024 * 1024;

/// An error answered as [`ErrorBody`].
pub struct Failure(StatusCode, ErrorBody);

impl Failure {
    fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self(
            status,
            ErrorBody {
                code: code.to_owned(),
                message: message.into(),
            },
        )
    }
}

impl From<HostError> for Failure {
    fn from(err: HostError) -> Self {
        let (status, code) = match &err {
            HostError::Busy(_) | HostError::RepairedRecently => (StatusCode::CONFLICT, "busy"),
            HostError::OrgFull(_) => (StatusCode::CONFLICT, "org_full"),
            HostError::HostFull(_) => (StatusCode::SERVICE_UNAVAILABLE, "host_full"),
            HostError::Runner(e) if e.code() == Some("not_found") => {
                (StatusCode::NOT_FOUND, "not_found")
            }
            HostError::Runner(e) if e.code() == Some("no_snapshot") => {
                (StatusCode::CONFLICT, "not_ready")
            }
            HostError::Runner(_) => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            HostError::Pg(_) | HostError::NotStarting(_) => (StatusCode::BAD_GATEWAY, "failed"),
        };
        if status.is_server_error() {
            tracing::warn!(error = %err, "service: {code}");
        }
        Self::new(status, code, err.to_string())
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The claims of the request, if the API signed it for `op` on `id` with
/// this `body`.
fn verify(
    host: &Host,
    headers: &HeaderMap,
    op: Op,
    id: Option<Uuid>,
    body: Option<&[u8]>,
) -> Result<Claims, Failure> {
    let header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let claims = Claims::verify(&host.config.key, header, now())
        .and_then(|claims| claims.allow(op, id, body).map(|()| claims))
        .map_err(|err| {
            tracing::warn!(?op, error = %err, "service: refused a call");
            Failure::new(StatusCode::UNAUTHORIZED, "bad_signature", err.to_string())
        })?;
    Ok(claims)
}

/// NDJSON: one event per line, as the job sends them.
fn ndjson<T: Serialize + Send + 'static>(rx: mpsc::Receiver<T>) -> Response {
    let lines = stream::unfold(rx, |mut rx| async move {
        let event = rx.recv().await?;
        let mut line = serde_json::to_vec(&event).unwrap_or_default();
        line.push(b'\n');
        Some((Ok::<_, Infallible>(Bytes::from(line)), rx))
    });
    (
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(lines),
    )
        .into_response()
}

pub fn router(host: Arc<Host>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/playgrounds", get(list))
        .route("/playgrounds/{id}", get(status).delete(destroy))
        .route("/playgrounds/{id}/build", post(build))
        .route("/playgrounds/{id}/migration", post(migration))
        .route("/playgrounds/{id}/reset", post(reset))
        .route("/playgrounds/{id}/stop", post(stop))
        .route("/playgrounds/{id}/console", get(console))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
        .with_state(host)
}

async fn health(State(host): State<Arc<Host>>) -> Json<Health> {
    let runner = host.runner.health().await;
    let running = host
        .list()
        .await
        .map_or(0, |l| l.playgrounds.iter().filter(|p| p.running).count());
    let runner_ok = runner
        .as_ref()
        .is_ok_and(|h| h.docker && h.runtime_available && h.image);
    let space = host.runner.space().await.ok();
    Json(Health {
        ok: runner_ok,
        runner: runner_ok,
        running,
        max_running: host.config.max_running,
        free_bytes: space.as_ref().map(|s| s.free_bytes),
        total_bytes: space.as_ref().map(|s| s.total_bytes),
    })
}

async fn list(State(host): State<Arc<Host>>, headers: HeaderMap) -> Result<Json<Listing>, Failure> {
    verify(&host, &headers, Op::List, None, None)?;
    Ok(Json(host.list().await?))
}

async fn status(
    State(host): State<Arc<Host>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Status>, Failure> {
    verify(&host, &headers, Op::Status, Some(id), None)?;
    Ok(Json(host.status(id).await?))
}

async fn destroy(
    State(host): State<Arc<Host>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode, Failure> {
    verify(&host, &headers, Op::Destroy, Some(id), None)?;
    host.destroy(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn build(
    State(host): State<Arc<Host>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Failure> {
    let claims = verify(&host, &headers, Op::Build, Some(id), Some(&body))?;
    let request: Build = serde_json::from_slice(&body)
        .map_err(|e| Failure::new(StatusCode::BAD_REQUEST, "bad_request", e.to_string()))?;
    let entry = host.entry(id, claims.org);
    let op = Host::try_op(&entry)?;
    let (tx, rx) = mpsc::channel(32);
    // Its own task: the API going away does not leave a half-built cluster;
    // deleting the playground cancels it.
    let job = tokio::spawn(build::build(
        Arc::clone(&host),
        Arc::clone(&entry),
        request,
        tx,
        op,
    ));
    entry.set_job(Some(job.abort_handle()));
    Ok(ndjson(rx))
}

async fn migration(
    State(host): State<Arc<Host>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Failure> {
    let claims = verify(&host, &headers, Op::Migrate, Some(id), Some(&body))?;
    if claims.access != Access::Develop {
        return Err(Failure::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "migration runs are for developers",
        ));
    }
    let request: Migrate = serde_json::from_slice(&body)
        .map_err(|e| Failure::new(StatusCode::BAD_REQUEST, "bad_request", e.to_string()))?;
    if request.statements.len() > limits::MIGRATION_STATEMENTS {
        return Err(Failure::new(
            StatusCode::BAD_REQUEST,
            "too_long",
            format!(
                "a migration run has at most {} statements",
                limits::MIGRATION_STATEMENTS
            ),
        ));
    }
    let entry = host.entry(id, claims.org);
    // Under the lock, refused before the stream starts: a busy playground
    // is a 409, never a run that fails.
    let op = Host::try_op(&entry)?;
    let by = claims.name.clone().unwrap_or_else(|| "Someone".to_owned());
    let (tx, rx) = mpsc::channel(32);
    let job = tokio::spawn(migrate::run(
        Arc::clone(&host),
        Arc::clone(&entry),
        request,
        by,
        tx,
        op,
    ));
    entry.set_job(Some(job.abort_handle()));
    Ok(ndjson(rx))
}

async fn reset(
    State(host): State<Arc<Host>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Status>, Failure> {
    let claims = verify(&host, &headers, Op::Reset, Some(id), None)?;
    let entry = host.entry(id, claims.org);
    let by = claims.name.clone().unwrap_or_else(|| "Someone".to_owned());
    let reset = {
        // Refused at once when busy: the API answers within its timeout.
        let _op = Host::try_op(&entry)?;
        let _busy = Host::busy(&entry, "resetting");
        host.reset_locked(&entry, &format!("{by} is resetting the rows"))
            .await
    };
    // Not busy any more before the consoles hear it is back (one opening
    // then connects, instead of waiting for a resume already sent).
    let _ = entry.notices.send(host::Notice::Resume);
    reset?;
    Ok(Json(host.status(id).await?))
}

async fn stop(
    State(host): State<Arc<Host>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Status>, Failure> {
    let claims = verify(&host, &headers, Op::Stop, Some(id), None)?;
    let entry = host.entry(id, claims.org);
    host.stop(&entry, "the playground was stopped").await?;
    Ok(Json(host.status(id).await?))
}

async fn console(
    State(host): State<Arc<Host>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, Failure> {
    let claims = verify(&host, &headers, Op::Console, Some(id), None)?;
    let entry = host.entry(id, claims.org);
    Ok(upgrade
        .max_message_size(2 * 1024 * 1024)
        .on_upgrade(move |socket| console::serve(host, entry, claims.access, socket)))
}

/// The idle stop, every minute, until the process ends.
pub fn reaper(host: Arc<Host>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_mins(1));
        tick.tick().await;
        loop {
            tick.tick().await;
            host.reap().await;
        }
    })
}
