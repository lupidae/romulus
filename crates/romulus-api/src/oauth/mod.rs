//! OAuth for the MCP endpoint (docs/mcp.md, "Connect"): a person connects
//! their agent by signing in to romulus in a browser and approving it,
//! instead of pasting a token. romulus's API is the authorization server of
//! its own MCP resource (`/api/mcp`); Zitadel stays the identity provider,
//! and signing in is the usual sign-in (single sign-on or a password).
//!
//! What MCP's authorization spec asks of a server, as clients (Claude Code,
//! claude.ai and Claude Desktop, Cursor, VS Code, `mcp-remote`) read it:
//! - **Discovery** ([`metadata`], outside `/api`): the protected resource
//!   metadata (RFC 9728) at `/.well-known/oauth-protected-resource` and its
//!   path-suffixed form `/.well-known/oauth-protected-resource/api/mcp`, and
//!   the authorization server metadata (RFC 8414) at
//!   `/.well-known/oauth-authorization-server`. The issuer is the public
//!   URL's origin. `/api/mcp` answers 401 with `WWW-Authenticate: Bearer
//!   resource_metadata="…"` (`mcp::unauthorized`).
//! - **Registration** ([`clients`], `POST /oauth/register`): dynamic client
//!   registration (RFC 7591), open, limited per client IP. Clients are
//!   public (no secret: PKCE protects the code). Redirect URIs are https,
//!   or http on a loopback host with any port (RFC 8252).
//! - **Authorization** ([`authorize`], `GET /oauth/authorize`): checks the
//!   request (`state`, PKCE S256, the scope, `resource` = the MCP endpoint),
//!   keeps it, ties it to this browser with a cookie, and sends the browser
//!   to the consent page, `/app/oauth/authorize` (signing in first when
//!   needed). The page reads it (`GET /oauth/requests/{id}`) and posts the
//!   person's decision (`POST /oauth/requests/{id}`), which answers where
//!   to send the browser: the client's redirect URI, with a code (60 s,
//!   single use) or `access_denied`, and `iss` (RFC 9207).
//! - **Tokens** ([`token`], `POST /oauth/token`, `POST /oauth/revoke`):
//!   the code with its PKCE verifier, then refresh tokens, which rotate on
//!   every use; a refresh token or a code presented twice revokes the whole
//!   grant. Access tokens ([`ACCESS_PREFIX`], an hour) act as the person
//!   exactly as a personal access token does (`access_tokens::Agent`), for
//!   the MCP endpoint only: tools, roles, caps, rate limits and the audit
//!   log are unchanged. Scopes [`SCOPE_READ`] and [`SCOPE_WRITE`] are the
//!   personal tokens' `read` and `write`.
//! - **Connected apps** ([`apps`], `GET /me/connected-apps`, `DELETE
//!   /me/connected-apps/{client_id}`): what the person approved, and
//!   Disconnect. A password reset or change disconnects them all
//!   ([`apps::revoke_all`]); deleting an account deletes its grants.
//!
//! Errors at the registration, token and revocation endpoints are OAuth's
//! (`{"error", "error_description"}`, [`OAuthError`]), not the API's:
//! that is what clients read. They are exempt from the CSRF guard (no
//! cookie is read there) and answer cross-origin requests (`cors`), for
//! clients that run in a browser. Tables: `oauth_clients`,
//! `oauth_requests`, `oauth_codes`, `oauth_grants`, `oauth_access_tokens`,
//! `oauth_refresh_tokens`.

pub mod apps;
pub mod authorize;
pub mod clients;
pub mod metadata;
pub mod token;

use std::{collections::HashMap, time::Duration};

use axum::{
    extract::Request,
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use serde_json::json;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{AppState, Config, domain::AccessTokenScope};

/// Every OAuth access token starts with it (personal ones with
/// `rmls_pat_`): recognisable by secret scanners.
pub const ACCESS_PREFIX: &str = "rmls_oat_";
/// Every refresh token starts with it.
pub const REFRESH_PREFIX: &str = "rmls_ort_";

/// The scope of reading tools only: a personal token's `read`.
pub const SCOPE_READ: &str = "romulus.read";
/// Everything the person's roles allow (and so reading too): `write`.
pub const SCOPE_WRITE: &str = "romulus.write";

/// An access token's life.
pub const ACCESS_TTL: Duration = Duration::from_hours(1);
/// A refresh token's life: a grant unused this long is over.
pub const REFRESH_IDLE: Duration = Duration::from_hours(30 * 24);
/// A grant's life, however often it is refreshed.
pub const GRANT_TTL: Duration = Duration::from_hours(90 * 24);
/// An authorization code's life.
pub const CODE_TTL: Duration = Duration::from_secs(60);
/// How long a person may take to sign in and decide.
pub const REQUEST_TTL: Duration = Duration::from_mins(20);
/// A client nobody used this long, with no grant left, is deleted.
pub const CLIENT_UNUSED: Duration = Duration::from_hours(30 * 24);
/// The sweeper's period ([`apps::sweeper`]).
pub const SWEEP_EVERY: Duration = Duration::from_hours(1);

/// The endpoints MCP clients call without a cookie, which the CSRF guard
/// lets through.
pub const OPEN_PATHS: [&str; 3] = [
    "/api/oauth/register",
    "/api/oauth/token",
    "/api/oauth/revoke",
];

/// The endpoints under `/api`.
pub fn router() -> OpenApiRouter<AppState> {
    let open = OpenApiRouter::new()
        .routes(routes!(clients::register))
        .routes(routes!(token::token))
        .routes(routes!(token::revoke))
        .layer(middleware::from_fn(cors));
    OpenApiRouter::new()
        .merge(open)
        .routes(routes!(authorize::authorize))
        .routes(routes!(authorize::request, authorize::decide))
        .routes(routes!(apps::list))
        .routes(routes!(apps::disconnect))
}

/// The resource the MCP endpoint is (RFC 8707, RFC 9728): what tokens are
/// issued for and checked against.
pub fn resource(config: &Config) -> String {
    format!("{}{}", config.public_origin(), crate::mcp::PATH)
}

/// The authorization server's issuer: the public URL's origin, so its
/// metadata lives at `/.well-known/oauth-authorization-server`.
pub fn issuer(config: &Config) -> String {
    config.public_origin()
}

/// The protected resource metadata's URL, as `WWW-Authenticate` names it.
pub fn resource_metadata_url(config: &Config) -> String {
    format!(
        "{}{}{}",
        config.public_origin(),
        metadata::PROTECTED_RESOURCE,
        crate::mcp::PATH
    )
}

/// The scope a grant of `scope` holds, as OAuth spells it (write implies
/// read).
pub fn scope_text(scope: AccessTokenScope) -> &'static str {
    match scope {
        AccessTokenScope::Read => SCOPE_READ,
        AccessTokenScope::Write => "romulus.read romulus.write",
    }
}

/// What a requested `scope` asks for: write if it names `romulus.write`,
/// read if only `romulus.read`, write when absent (all of
/// `scopes_supported`, as clients ask by default). `offline_access` is
/// accepted and changes nothing (refresh tokens are always issued).
/// `Err` names the first scope romulus does not know.
pub fn parse_scope(scope: Option<&str>) -> Result<AccessTokenScope, String> {
    let mut asked = None;
    for item in scope
        .unwrap_or("")
        .split(' ')
        .filter(|item| !item.is_empty())
    {
        match item {
            SCOPE_WRITE => asked = Some(AccessTokenScope::Write),
            SCOPE_READ => {
                asked.get_or_insert(AccessTokenScope::Read);
            }
            "offline_access" => {}
            other => return Err(other.to_owned()),
        }
    }
    Ok(asked.unwrap_or(AccessTokenScope::Write))
}

/// Whether `value` names the MCP endpoint (`resource`): the same URL, its
/// scheme and host in any case, with or without a trailing slash.
pub fn is_our_resource(config: &Config, value: &str) -> bool {
    let Ok(mut presented) = url::Url::parse(value) else {
        return false;
    };
    if presented.fragment().is_some() || presented.query().is_some() {
        return false;
    }
    if presented.path().len() > 1 && presented.path().ends_with('/') {
        let trimmed = presented.path().trim_end_matches('/').to_owned();
        presented.set_path(&trimmed);
    }
    presented.as_str() == resource(config)
}

/// The parameters of a form body or a query string. OAuth refuses a
/// parameter sent twice (RFC 6749, 3.1 and 3.2): `Err` names it.
pub(crate) struct Params(HashMap<String, String>);

impl Params {
    pub(crate) fn parse(input: &[u8]) -> Result<Self, String> {
        let mut params = HashMap::new();
        for (name, value) in url::form_urlencoded::parse(input) {
            if params
                .insert(name.clone().into_owned(), value.into_owned())
                .is_some()
            {
                return Err(name.into_owned());
            }
        }
        Ok(Self(params))
    }

    /// A parameter, absent when empty (RFC 6749: an empty value is an
    /// omitted one).
    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        self.0
            .get(name)
            .map(String::as_str)
            .filter(|value| !value.is_empty())
    }
}

/// An error at an OAuth endpoint, in OAuth's shape (RFC 6749, 5.2; RFC
/// 7591, 3.2.2): `{"error": "invalid_grant", "error_description": "…"}`.
#[derive(Debug)]
pub struct OAuthError {
    pub status: StatusCode,
    pub error: &'static str,
    pub description: String,
    pub retry_after: Option<Duration>,
}

impl OAuthError {
    pub fn new(status: StatusCode, error: &'static str, description: impl Into<String>) -> Self {
        Self {
            status,
            error,
            description: description.into(),
            retry_after: None,
        }
    }

    pub fn invalid_request(description: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", description)
    }

    pub fn invalid_grant(description: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_grant", description)
    }

    /// 401: the client is unknown (expired, or never registered here).
    /// Clients register again on it.
    pub fn invalid_client() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "This client is not registered with romulus (any more): register it again.",
        )
    }

    /// 429 with `Retry-After`.
    pub fn too_many(retry_after: Duration) -> Self {
        let seconds = retry_after.as_secs().max(1);
        Self {
            retry_after: Some(Duration::from_secs(seconds)),
            ..Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                format!("Too many requests: wait {seconds} s."),
            )
        }
    }

    /// A database failure: logged, answered as `server_error`.
    pub fn internal(err: &sqlx::Error) -> Self {
        tracing::error!(error = %err, "oauth: database error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "romulus could not answer just now: try again in a moment.",
        )
    }
}

impl From<sqlx::Error> for OAuthError {
    fn from(err: sqlx::Error) -> Self {
        Self::internal(&err)
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            no_store(),
            crate::Json(json!({ "error": self.error, "error_description": self.description })),
        )
            .into_response();
        if let Some(wait) = self.retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(wait.as_secs()));
        }
        response
    }
}

/// Tokens and errors are never cached (RFC 6749, 5.1).
pub(crate) fn no_store() -> [(header::HeaderName, HeaderValue); 2] {
    [
        (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        (header::PRAGMA, HeaderValue::from_static("no-cache")),
    ]
}

/// Cross-origin access for the metadata and the endpoints clients call
/// without a cookie: any origin, no credentials, so a browser-based client
/// can use them and no page can use someone's session through them (none
/// of them reads it).
pub(crate) async fn cors(request: Request, next: Next) -> Response {
    let mut response = if request.method() == Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("authorization, content-type, mcp-protocol-version"),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("86400"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_default_to_everything_and_write_implies_read() {
        assert_eq!(parse_scope(None), Ok(AccessTokenScope::Write));
        assert_eq!(parse_scope(Some("")), Ok(AccessTokenScope::Write));
        assert_eq!(
            parse_scope(Some("romulus.read")),
            Ok(AccessTokenScope::Read)
        );
        assert_eq!(
            parse_scope(Some("romulus.read romulus.write")),
            Ok(AccessTokenScope::Write)
        );
        assert_eq!(
            parse_scope(Some("romulus.write")),
            Ok(AccessTokenScope::Write)
        );
        assert_eq!(
            parse_scope(Some("romulus.read offline_access")),
            Ok(AccessTokenScope::Read)
        );
        assert_eq!(parse_scope(Some("romulus.read admin")), Err("admin".into()));
        assert_eq!(
            scope_text(AccessTokenScope::Write),
            "romulus.read romulus.write"
        );
    }

    #[test]
    fn the_resource_is_the_mcp_endpoint_in_any_case() {
        let config = Config::for_tests();
        let ours = resource(&config);
        assert_eq!(ours, "http://localhost:41000/api/mcp");
        assert!(is_our_resource(&config, &ours));
        assert!(is_our_resource(&config, "HTTP://LOCALHOST:41000/api/mcp"));
        assert!(is_our_resource(&config, "http://localhost:41000/api/mcp/"));
        for other in [
            "http://localhost:41000",
            "http://localhost:41000/",
            "http://localhost:41000/api/mcp#x",
            "http://localhost:41000/api/mcp?x=1",
            "http://localhost:41001/api/mcp",
            "https://localhost:41000/api/mcp",
            "http://evil.test/api/mcp",
            "http://localhost:41000/api/MCP",
            "not a url",
        ] {
            assert!(!is_our_resource(&config, other), "{other}");
        }
        assert_eq!(
            resource_metadata_url(&config),
            "http://localhost:41000/.well-known/oauth-protected-resource/api/mcp"
        );
    }

    #[test]
    fn a_parameter_sent_twice_is_refused() {
        let params = Params::parse(b"a=1&b=&c=x%20y").unwrap();
        assert_eq!(params.get("a"), Some("1"));
        assert_eq!(params.get("b"), None, "empty is omitted");
        assert_eq!(params.get("c"), Some("x y"));
        assert_eq!(Params::parse(b"a=1&a=2").err().as_deref(), Some("a"));
    }
}
