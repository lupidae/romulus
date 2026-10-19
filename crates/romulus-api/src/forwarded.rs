//! The client's address behind a reverse proxy (`ROMULUS_TRUSTED_PROXIES`).
//!
//! Behind a proxy every request comes from the proxy, so per-client limits
//! (sign-ins) would count everyone as one client. When the TCP peer is a
//! trusted proxy, the client is the right-most address of
//! `X-Forwarded-For` that is not itself a trusted proxy: what the nearest
//! untrusted hop connected from. Anything left of it is whatever the client
//! claimed, never believed. The address replaces the peer in the request's
//! `ConnectInfo`, which handlers read. With no trusted proxies (the
//! default), nothing here runs and no header is believed.

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use axum::{
    extract::{ConnectInfo, Request, State},
    middleware::Next,
    response::Response,
};

use crate::Config;

const FORWARDED_FOR: &str = "x-forwarded-for";

pub async fn client(
    State(config): State<Arc<Config>>,
    mut request: Request,
    next: Next,
) -> Response {
    if config.trusted_proxies.is_empty() {
        return next.run(request).await;
    }
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip().to_canonical());
    if let Some(peer) = peer
        && is_trusted(&config, peer)
        && let Some(client) = forwarded_client(&config, request.headers())
    {
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(client, 0)));
    }
    next.run(request).await
}

fn is_trusted(config: &Config, ip: IpAddr) -> bool {
    config.trusted_proxies.iter().any(|net| net.contains(&ip))
}

/// The right-most untrusted address of every `X-Forwarded-For` line, read
/// as one list; the left-most if all are trusted. `None` without a usable
/// header (a malformed entry ends the walk: nothing left of it is trusted).
fn forwarded_client(config: &Config, headers: &axum::http::HeaderMap) -> Option<IpAddr> {
    let hops: Vec<&str> = headers
        .get_all(FORWARDED_FOR)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .collect();
    let mut last = None;
    for hop in hops.iter().rev() {
        let ip = parse_hop(hop)?.to_canonical();
        if !is_trusted(config, ip) {
            return Some(ip);
        }
        last = Some(ip);
    }
    last
}

/// `203.0.113.7`, `2001:db8::1`, `[2001:db8::1]:443` or `203.0.113.7:443`.
fn parse_hop(hop: &str) -> Option<IpAddr> {
    hop.parse::<IpAddr>()
        .ok()
        .or_else(|| hop.parse::<SocketAddr>().ok().map(|addr| addr.ip()))
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::*;

    fn config(proxies: &[&str]) -> Config {
        let mut config = Config::for_tests();
        config.trusted_proxies = proxies.iter().map(|p| p.parse().unwrap()).collect();
        config
    }

    fn headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(FORWARDED_FOR, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn the_client_is_the_last_untrusted_hop() {
        let config = config(&["172.16.0.0/12", "10.0.0.1/32"]);
        // A client-claimed address on the left is ignored.
        let client = forwarded_client(&config, &headers(&["6.6.6.6, 203.0.113.7, 10.0.0.1"]));
        assert_eq!(client, Some("203.0.113.7".parse().unwrap()));
        let client = forwarded_client(&config, &headers(&["6.6.6.6", "203.0.113.7"]));
        assert_eq!(client, Some("203.0.113.7".parse().unwrap()));
        let client = forwarded_client(&config, &headers(&["[2001:db8::1]:443"]));
        assert_eq!(client, Some("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn garbage_believes_nothing() {
        let config = config(&["10.0.0.0/8"]);
        assert_eq!(forwarded_client(&config, &headers(&["not-an-ip"])), None);
        assert_eq!(forwarded_client(&config, &HeaderMap::new()), None);
        // Garbage right of a real address stops the walk before it.
        assert_eq!(
            forwarded_client(&config, &headers(&["203.0.113.7, junk"])),
            None
        );
    }

    #[test]
    fn mapped_addresses_count_as_ipv4() {
        let config = config(&["172.16.0.0/12"]);
        assert!(is_trusted(
            &config,
            "::ffff:172.18.0.3"
                .parse::<IpAddr>()
                .unwrap()
                .to_canonical()
        ));
    }
}
