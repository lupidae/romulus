//! Cross-site request forgery guard for cookie-authenticated mutations.
//!
//! The session cookie is `SameSite=Lax`, which still lets another site's
//! top-level form POST carry it in older browsers and some edge cases. So
//! every unsafe request must also prove it comes from our own page: a custom
//! header (which a cross-site form cannot set and `fetch` cannot set without a
//! CORS preflight we never grant) and an `Origin` equal to the public URL.
//!
//! Requests with `Authorization: Bearer …` (remus, CI) carry no ambient
//! credential and are exempt. **Whatever authenticates them must then use the
//! bearer token alone and never fall back to the cookie**, or this exemption
//! becomes a bypass.
//!
//! Websocket upgrades are GETs and pass through here; the collab module checks
//! their `Origin` itself (Cross-Site WebSocket Hijacking).

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{HeaderMap, header},
    middleware::Next,
    response::Response,
};

use crate::{ApiError, Config};

/// The header the web app sends on every request (`web/src/api/client.ts`).
pub const HEADER: &str = "x-romulus-csrf";

pub async fn guard(
    State(config): State<Arc<Config>>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if request.method().is_safe() || has_bearer(request.headers()) {
        return Ok(next.run(request).await);
    }
    let headers = request.headers();
    let marked = headers.get(HEADER).is_some_and(|value| value == "1");
    let same_origin = headers
        .get(header::ORIGIN)
        .is_some_and(|origin| origin.as_bytes() == config.public_origin().as_bytes());
    if marked && same_origin {
        Ok(next.run(request).await)
    } else {
        tracing::info!(
            method = %request.method(),
            origin = ?headers.get(header::ORIGIN),
            marked,
            "csrf: refused"
        );
        Err(ApiError::Csrf)
    }
}

fn has_bearer(headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.get(..7))
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("bearer "))
}
