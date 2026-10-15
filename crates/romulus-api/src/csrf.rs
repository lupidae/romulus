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
//! becomes a bypass (`auth::CurrentUser` refuses bearer requests).
//!
//! Websocket upgrades are GETs and pass through here, yet the browser sends
//! the cookie with them from any site (Cross-Site WebSocket Hijacking). The
//! upgrade handler takes [`SameOrigin`] as an extractor, which checks `Origin`
//! the same way:
//!
//! ```ignore
//! async fn ws(_: SameOrigin, member: Member, upgrade: WebSocketUpgrade) -> Response { … }
//! ```

use std::sync::Arc;

use axum::{
    extract::{FromRef, FromRequestParts, Request, State},
    http::{HeaderMap, header, request::Parts},
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
    if marked && is_same_origin(headers, &config) {
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

/// Whether the request's `Origin` is exactly the public URL's origin. A
/// missing `Origin` is not ours: browsers always send one on unsafe requests
/// and websocket upgrades.
pub fn is_same_origin(headers: &HeaderMap, config: &Config) -> bool {
    headers
        .get(header::ORIGIN)
        .is_some_and(|origin| origin.as_bytes() == config.public_origin().as_bytes())
}

/// Whether the request authenticates with an API token rather than a cookie.
pub fn has_bearer(headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.get(..7))
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("bearer "))
}

/// Extractor for requests the middleware cannot guard, websocket upgrades
/// first: rejects with 403 `csrf` unless `Origin` is the public URL's.
#[derive(Debug, Clone, Copy)]
pub struct SameOrigin;

impl<S> FromRequestParts<S> for SameOrigin
where
    Arc<Config>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> impl Future<Output = Result<Self, ApiError>> + Send {
        let config = Arc::<Config>::from_ref(state);
        let result = if is_same_origin(&parts.headers, &config) {
            Ok(Self)
        } else {
            tracing::info!(origin = ?parts.headers.get(header::ORIGIN), "csrf: cross-origin upgrade refused");
            Err(ApiError::Csrf)
        };
        std::future::ready(result)
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(origin: Option<&'static str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(origin) = origin {
            headers.insert(header::ORIGIN, HeaderValue::from_static(origin));
        }
        headers
    }

    #[test]
    fn only_the_exact_public_origin_is_ours() {
        let config = Config::for_tests();
        assert!(is_same_origin(
            &headers(Some("http://localhost:41000")),
            &config
        ));
        for other in [
            Some("http://localhost:41001"),
            Some("https://localhost:41000"),
            Some("http://localhost:41000.evil.test"),
            Some("null"),
            None,
        ] {
            assert!(!is_same_origin(&headers(other), &config), "{other:?}");
        }
    }

    #[tokio::test]
    async fn the_extractor_refuses_another_origin() {
        let state = Arc::new(Config::for_tests());
        let request = |origin| {
            let mut builder = axum::http::Request::get("/ws");
            if let Some(origin) = origin {
                builder = builder.header(header::ORIGIN, origin);
            }
            builder.body(()).unwrap().into_parts().0
        };
        let mut ours = request(Some("http://localhost:41000"));
        assert!(
            SameOrigin::from_request_parts(&mut ours, &state)
                .await
                .is_ok()
        );
        let mut theirs = request(Some("http://evil.test"));
        let err = SameOrigin::from_request_parts(&mut theirs, &state)
            .await
            .unwrap_err();
        assert_eq!(err.code(), "csrf");
    }

    #[test]
    fn bearer_is_case_insensitive_and_needs_a_token() {
        let with = |value: &'static str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::AUTHORIZATION, HeaderValue::from_static(value));
            headers
        };
        assert!(has_bearer(&with("Bearer rom_x")));
        assert!(has_bearer(&with("bearer rom_x")));
        assert!(!has_bearer(&with("Basic abc")));
        assert!(!has_bearer(&HeaderMap::new()));
    }
}
