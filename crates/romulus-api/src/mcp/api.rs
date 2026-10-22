//! How a tool reaches romulus: through the REST API's own router, as the
//! token's person. Each call is an HTTP request built here and handed to the
//! routes (never through the network, the edge or the CSRF guard: those are
//! for requests from outside), carrying [`Delegated`]: `CurrentUser` and
//! `Member` resolve to the person, so the role checks, validation, rate
//! limits and audit events are exactly the web app's. There is no second
//! code path for permissions.
//!
//! A read-only token's calls are GETs, but for what is a read by nature
//! though it posts (a playground query: [`Api::post_reading`]); the lent
//! identity refuses any other write on its own.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex, OnceLock, PoisonError},
    time::Duration,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Method, Request, StatusCode, header},
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::time::Instant;
use tower::ServiceExt;
use url::Url;

use crate::{
    AppState,
    auth::access_tokens::{Agent, Delegated, Reading, read_only},
    error::ErrorBody,
};

/// The routes, built once (they are the same for every call).
static ROUTES: OnceLock<Router<AppState>> = OnceLock::new();

/// The largest answer a tool reads (a big schema's diff, both models).
const ANSWER_LIMIT: usize = 64 * 1024 * 1024;

/// What one call may take at most (less when the request's deadline is
/// nearer): a handler that takes longer is dropped, as the API's own
/// timeout would.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(25);

/// A call the API refused, or could not answer: its status and its error.
#[derive(Debug, Clone)]
pub struct Failure {
    pub status: StatusCode,
    /// Boxed: a failure travels in every tool's `Result`.
    pub body: Box<ErrorBody>,
}

impl Failure {
    pub fn code(&self) -> &str {
        &self.body.code
    }

    fn timeout() -> Self {
        Self {
            status: StatusCode::GATEWAY_TIMEOUT,
            body: Box::new(ErrorBody {
                code: "timeout".into(),
                message: "romulus did not answer this call in time.".into(),
                details: None,
            }),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: Box::new(ErrorBody {
                code: "internal".into(),
                message: message.into(),
                details: None,
            }),
        }
    }
}

/// A write that succeeded: where (from its path), for the audit of a tool
/// that failed after it.
#[derive(Debug, Clone)]
pub struct Wrote {
    /// The organization's slug.
    pub org: Option<String>,
    /// The database's slug.
    pub database: Option<String>,
}

/// The REST API, as the token's person, until `deadline`.
#[derive(Clone)]
pub struct Api {
    state: AppState,
    agent: Agent,
    client: Option<ConnectInfo<SocketAddr>>,
    deadline: Instant,
    writes: Arc<Mutex<Vec<Wrote>>>,
}

impl Api {
    pub fn new(
        state: AppState,
        agent: Agent,
        client: Option<ConnectInfo<SocketAddr>>,
        deadline: Instant,
    ) -> Self {
        Self {
            state,
            agent,
            client,
            deadline,
            writes: Arc::default(),
        }
    }

    /// What is left until the request's deadline.
    pub fn time_left(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// The first write that succeeded, if any.
    pub fn first_write(&self) -> Option<Wrote> {
        self.writes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .first()
            .cloned()
    }

    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &[&str],
        query: &[(&str, String)],
    ) -> Result<T, Failure> {
        self.call(Method::GET, path, query, None::<&()>, false)
            .await
    }

    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &[&str],
        body: &impl Serialize,
    ) -> Result<T, Failure> {
        self.call(Method::POST, path, &[], Some(body), false).await
    }

    /// A POST that only reads (a playground query): a read-only token may.
    pub async fn post_reading<T: DeserializeOwned>(
        &self,
        path: &[&str],
        body: &impl Serialize,
    ) -> Result<T, Failure> {
        self.call(Method::POST, path, &[], Some(body), true).await
    }

    pub async fn put<T: DeserializeOwned>(
        &self,
        path: &[&str],
        body: &impl Serialize,
    ) -> Result<T, Failure> {
        self.call(Method::PUT, path, &[], Some(body), false).await
    }

    pub async fn patch<T: DeserializeOwned>(
        &self,
        path: &[&str],
        body: &impl Serialize,
    ) -> Result<T, Failure> {
        self.call(Method::PATCH, path, &[], Some(body), false).await
    }

    /// A DELETE answered without a body (204).
    pub async fn delete(&self, path: &[&str]) -> Result<(), Failure> {
        self.send(Method::DELETE, path, &[], None, false)
            .await
            .map(drop)
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &[&str],
        query: &[(&str, String)],
        body: Option<&impl Serialize>,
        reading: bool,
    ) -> Result<T, Failure> {
        let body = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|err| Failure::internal(err.to_string()))?;
        let bytes = self.send(method, path, query, body, reading).await?;
        serde_json::from_slice(&bytes).map_err(|err| {
            tracing::error!(error = %err, ?path, "mcp: an answer of the API did not read");
            Failure::internal("romulus answered in a way this tool did not expect.")
        })
    }

    /// The request, answered: the body of a success, or the API's error.
    async fn send(
        &self,
        method: Method,
        path: &[&str],
        query: &[(&str, String)],
        body: Option<Vec<u8>>,
        reading: bool,
    ) -> Result<axum::body::Bytes, Failure> {
        let writes = method != Method::GET && !reading;
        if !self.agent.token.can_write() && writes {
            return Err(failure_of(&read_only()));
        }
        // Dot segments would move the path (`url` drops them): nothing names
        // a thing so.
        if path.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
            return Err(failure_of(&crate::ApiError::not_found(
                "No such thing: check the names and ids given.",
            )));
        }
        let uri = uri(path, query);
        let uri_path = uri
            .split_once('?')
            .map_or(uri.as_str(), |(path, _)| path)
            .to_owned();
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::ACCEPT, "application/json");
        if body.is_some() {
            request = request.header(header::CONTENT_TYPE, "application/json");
        }
        let mut request = request
            .body(body.map_or_else(Body::empty, Body::from))
            .map_err(|err| Failure::internal(err.to_string()))?;
        request
            .extensions_mut()
            .insert(Delegated(self.agent.clone()));
        if reading {
            request.extensions_mut().insert(Reading);
        }
        if let Some(client) = self.client {
            request.extensions_mut().insert(client);
        }
        let routes = ROUTES
            .get_or_init(|| crate::router_and_spec().0)
            .clone()
            .with_state(self.state.clone());
        let limit = DEFAULT_TIMEOUT.min(self.time_left());
        let response = match tokio::time::timeout(limit, routes.oneshot(request)).await {
            Ok(Ok(response)) => response,
            Ok(Err(never)) => match never {},
            Err(_) => {
                tracing::warn!(?path, "mcp: a call ran out of time");
                return Err(Failure::timeout());
            }
        };
        // The routes are called without the app's layers: a plan's cap
        // reached is told from here.
        if let Some(details) = crate::operator::limits::reached(&response) {
            crate::operator::limits::observe(&self.state, &uri_path, &details).await;
        }
        let status = response.status();
        if writes && status.is_success() {
            let segment = |i: usize| path.get(i).map(|s| (*s).to_owned());
            let under_org = path.first() == Some(&"orgs");
            let wrote = Wrote {
                org: segment(1).filter(|_| under_org),
                database: segment(3).filter(|_| under_org && path.get(2) == Some(&"databases")),
            };
            self.writes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(wrote);
        }
        let bytes = to_bytes(response.into_body(), ANSWER_LIMIT)
            .await
            .map_err(|err| Failure::internal(err.to_string()))?;
        if status.is_success() {
            return Ok(bytes);
        }
        let body = serde_json::from_slice::<ErrorBody>(&bytes).unwrap_or_else(|_| ErrorBody {
            code: if status.is_server_error() {
                "unavailable".into()
            } else {
                "bad_request".into()
            },
            message: String::from_utf8_lossy(&bytes)
                .trim()
                .chars()
                .take(500)
                .collect(),
            details: None,
        });
        Err(Failure {
            status,
            body: Box::new(body),
        })
    }
}

/// An `ApiError` raised here, as the API would have answered it.
pub fn failure_of(err: &crate::ApiError) -> Failure {
    Failure {
        status: err.status(),
        body: Box::new(err.body()),
    }
}

/// `/api/` + each segment percent-encoded (whatever an agent sends, it stays
/// one segment: no `..`, no `?`), then the query.
fn uri(path: &[&str], query: &[(&str, String)]) -> String {
    let mut url = Url::parse("http://romulus.internal/api/").expect("a valid base URL");
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.pop_if_empty();
        for segment in path {
            segments.push(segment);
        }
    }
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (name, value) in query {
            pairs.append_pair(name, value);
        }
    }
    let mut uri = url.path().to_owned();
    if let Some(query) = url.query() {
        uri.push('?');
        uri.push_str(query);
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_stay_segments() {
        assert_eq!(
            uri(&["orgs", "acme", "databases"], &[]),
            "/api/orgs/acme/databases"
        );
        assert_eq!(
            uri(&["orgs", "../me", "x?y=1#z"], &[]),
            "/api/orgs/..%2Fme/x%3Fy=1%23z"
        );
        assert_eq!(
            uri(&["orgs", "acme", "search"], &[("q", "a b&c".into())]),
            "/api/orgs/acme/search?q=a+b%26c"
        );
    }
}
