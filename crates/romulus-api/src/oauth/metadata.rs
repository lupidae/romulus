//! The discovery documents, served at the origin's root rather than under
//! `/api` (`well_known`): where MCP clients look for them. A proxy in front
//! of the API sends `/.well-known/oauth-*` to it (the homepage's Pages
//! Function, a self-hosted install's reverse proxy).
//!
//! - Protected resource metadata (RFC 9728) at [`PROTECTED_RESOURCE`] +
//!   `/api/mcp`, the form for a resource with a path: the MCP endpoint, its
//!   one authorization server (romulus itself) and the scopes. Not at
//!   [`PROTECTED_RESOURCE`] alone: a document there would describe the
//!   origin as a resource (RFC 9728, 3.3), which romulus does not protect.
//!   Clients find this one from `/api/mcp`'s `WWW-Authenticate`, or try it
//!   first.
//! - Authorization server metadata (RFC 8414) at [`AUTHORIZATION_SERVER`]:
//!   the endpoints, public clients only (`none`), PKCE S256 only, `iss` in
//!   authorization responses (RFC 9207).

use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::{HeaderValue, header},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};

use super::{SCOPE_READ, SCOPE_WRITE, issuer, resource};
use crate::{AppState, Config, Json};

pub const PROTECTED_RESOURCE: &str = "/.well-known/oauth-protected-resource";
pub const AUTHORIZATION_SERVER: &str = "/.well-known/oauth-authorization-server";

/// Where people read how connecting an agent works.
const DOCUMENTATION: &str = "https://github.com/lupidae/romulus/blob/main/docs/mcp.md";

/// The well-known routes, outside `/api` (`crate::app` merges them).
pub fn well_known() -> Router<AppState> {
    Router::new()
        .route(
            &format!("{PROTECTED_RESOURCE}{}", crate::mcp::PATH),
            get(protected_resource),
        )
        .route(AUTHORIZATION_SERVER, get(authorization_server))
        .layer(middleware::from_fn(super::cors))
}

/// The MCP endpoint as a protected resource (RFC 9728, 2).
pub fn protected_resource_document(config: &Config) -> Value {
    json!({
        "resource": resource(config),
        "authorization_servers": [issuer(config)],
        "scopes_supported": [SCOPE_READ, SCOPE_WRITE],
        "bearer_methods_supported": ["header"],
        "resource_name": "romulus",
        "resource_documentation": DOCUMENTATION,
    })
}

/// romulus as the authorization server of its MCP endpoint (RFC 8414, 2).
pub fn authorization_server_document(config: &Config) -> Value {
    let endpoint = |path: &str| format!("{}/api/oauth/{path}", config.public_origin());
    json!({
        "issuer": issuer(config),
        "authorization_endpoint": endpoint("authorize"),
        "token_endpoint": endpoint("token"),
        "registration_endpoint": endpoint("register"),
        "revocation_endpoint": endpoint("revoke"),
        "scopes_supported": [SCOPE_READ, SCOPE_WRITE],
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["none"],
        "revocation_endpoint_auth_methods_supported": ["none"],
        "code_challenge_methods_supported": ["S256"],
        "authorization_response_iss_parameter_supported": true,
        "service_documentation": DOCUMENTATION,
    })
}

async fn protected_resource(State(config): State<Arc<Config>>) -> Response {
    cached(protected_resource_document(&config))
}

async fn authorization_server(State(config): State<Arc<Config>>) -> Response {
    cached(authorization_server_document(&config))
}

/// Metadata changes with a deploy at most.
fn cached(document: Value) -> Response {
    (
        [(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=300"),
        )],
        Json(document),
    )
        .into_response()
}
