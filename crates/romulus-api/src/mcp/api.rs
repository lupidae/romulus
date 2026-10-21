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

use std::{net::SocketAddr, sync::OnceLock, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Method, Request, StatusCode, header},
};
use serde::{Serialize, de::DeserializeOwned};
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

/// What a call may take, unless the tool says otherwise.
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

/// The REST API, as the token's person.
#[derive(Clone)]
pub struct Api {
    state: AppState,
    agent: Agent,
    client: Option<ConnectInfo<SocketAddr>>,
}

impl Api {
    pub fn new(state: AppState, agent: Agent, client: Option<ConnectInfo<SocketAddr>>) -> Self {
        Self {
            state,
            agent,
            client,
        }
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
        if !self.agent.token.can_write() && method != Method::GET && !reading {
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
        let response = match routes.oneshot(request).await {
            Ok(response) => response,
            Err(never) => match never {},
        };
        let status = response.status();
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
