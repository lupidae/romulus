//! The edge in front of a deployed API (the preview's Pages Function,
//! `site/functions/_middleware.js`), when `ROMULUS_EDGE_KEY` is set.
//!
//! The origin hostname the edge forwards to is public (a tunnel route), so
//! the edge proves itself with a shared secret, [`KEY_HEADER`], on every
//! request it forwards. Without it the API answers 403 `edge_only`: the only
//! way in is the public URL, the one `Origin` checks and redirects are built
//! for. Loopback peers are exempt (the container's own health check).
//!
//! A request that carries the key also carries the visitor's address in
//! [`CLIENT_IP_HEADER`]. It replaces the peer address (the tunnel connector,
//! the same for everyone) that per-client limits (sign-ins) key by. Unset, the
//! API trusts no header and nothing here runs.

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use axum::{
    extract::{ConnectInfo, Request, State},
    middleware::Next,
    response::Response,
};
use sha2::{Digest, Sha256};

use crate::{ApiError, Config};

/// The shared secret, as the edge sends it.
pub const KEY_HEADER: &str = "x-romulus-edge-key";
/// The visitor's address, as the edge saw it (`CF-Connecting-IP`).
pub const CLIENT_IP_HEADER: &str = "x-romulus-client-ip";

pub async fn guard(
    State(config): State<Arc<Config>>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let Some(key) = &config.edge_key else {
        return Ok(next.run(request).await);
    };
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| *addr);
    let presented = request
        .headers()
        .get(KEY_HEADER)
        .is_some_and(|value| same_secret(value.as_bytes(), key.expose().as_bytes()));
    if presented {
        let client = request
            .headers()
            .get(CLIENT_IP_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<IpAddr>().ok());
        if let Some(ip) = client {
            request
                .extensions_mut()
                .insert(ConnectInfo(SocketAddr::new(ip, 0)));
        }
        return Ok(next.run(request).await);
    }
    if peer.is_some_and(|addr| addr.ip().is_loopback()) {
        return Ok(next.run(request).await);
    }
    tracing::info!(peer = ?peer, path = %request.uri().path(), "edge: request without the edge key refused");
    Err(ApiError::Denied {
        code: "edge_only",
        message: format!("Use {}.", config.public_origin()),
    })
}

/// Compares digests, so the time taken says nothing about the key.
fn same_secret(presented: &[u8], key: &[u8]) -> bool {
    Sha256::digest(presented) == Sha256::digest(key)
}

#[cfg(test)]
mod tests {
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
        middleware,
        routing::get,
    };
    use tower::ServiceExt;

    use super::*;
    use crate::config::Secret;

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    /// Answers the client address the handlers would see.
    fn app(edge_key: Option<&str>) -> Router {
        let mut config = Config::for_tests();
        config.edge_key = edge_key.map(Secret::new);
        Router::new()
            .route(
                "/",
                get(|request: Request<Body>| async move {
                    request
                        .extensions()
                        .get::<ConnectInfo<SocketAddr>>()
                        .map_or_else(String::new, |ConnectInfo(addr)| addr.ip().to_string())
                }),
            )
            .layer(middleware::from_fn_with_state(Arc::new(config), guard))
    }

    async fn send(app: Router, peer: &str, headers: &[(&str, &str)]) -> (StatusCode, String) {
        let mut request = Request::get("/");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut request = request.body(Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn without_a_key_configured_everything_passes_and_no_header_is_trusted() {
        let (status, client) = send(
            app(None),
            "172.18.0.5:4000",
            &[(CLIENT_IP_HEADER, "203.0.113.9")],
        )
        .await;
        assert_eq!((status, client.as_str()), (StatusCode::OK, "172.18.0.5"));
    }

    #[tokio::test]
    async fn the_key_lets_the_edge_in_with_the_visitor_address() {
        let headers = [(KEY_HEADER, KEY), (CLIENT_IP_HEADER, "2001:db8::7")];
        let (status, client) = send(app(Some(KEY)), "172.18.0.5:4000", &headers).await;
        assert_eq!((status, client.as_str()), (StatusCode::OK, "2001:db8::7"));
    }

    #[tokio::test]
    async fn the_key_without_an_address_keeps_the_peer() {
        let (status, client) = send(
            app(Some(KEY)),
            "172.18.0.5:4000",
            &[(KEY_HEADER, KEY), (CLIENT_IP_HEADER, "nope")],
        )
        .await;
        assert_eq!((status, client.as_str()), (StatusCode::OK, "172.18.0.5"));
    }

    #[tokio::test]
    async fn a_missing_or_wrong_key_is_refused() {
        for headers in [
            vec![],
            vec![(KEY_HEADER, "0123456789abcdef0123456789abcdeX")],
        ] {
            let mut headers = headers;
            headers.push((CLIENT_IP_HEADER, "203.0.113.9"));
            let (status, body) = send(app(Some(KEY)), "172.18.0.5:4000", &headers).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert!(body.contains("edge_only"), "{body}");
        }
    }

    #[tokio::test]
    async fn loopback_peers_need_no_key() {
        let (status, _) = send(app(Some(KEY)), "127.0.0.1:4000", &[]).await;
        assert_eq!(status, StatusCode::OK);
    }
}
