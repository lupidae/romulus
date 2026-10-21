//! Dynamic client registration (RFC 7591): `POST /oauth/register`, open to
//! anyone, as MCP clients need (claude.ai registers a client per
//! connection), limited per client IP (`rate::Limits::oauth_registrations`).
//!
//! Every client is public: `token_endpoint_auth_method` is `none` whatever
//! was asked (RFC 7591 lets the server decide; the answer says so), and PKCE
//! protects its codes. Its name is shown on the consent screen as the client
//! said it, next to its redirect URI's host, which is what can be trusted.
//! Other metadata (logo, URIs, contacts) is not kept: nothing unverified is
//! shown or fetched.
//!
//! Redirect URIs ([`check_redirect_uri`]): https, or http on a loopback host
//! (`127.0.0.1`, `[::1]`, `localhost`), whose port is not compared
//! ([`redirect_matches`], RFC 8252 7.3: a native client listens on whatever
//! port it got). No fragment, no user info. A custom scheme is refused.

use std::net::SocketAddr;

use axum::{
    Extension,
    body::Body,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use url::{Host, Url};
use utoipa::ToSchema;
use uuid::Uuid;

use super::OAuthError;
use crate::{AppState, Json, auth::password::throttle};

/// A registration's body at most.
const BODY_LIMIT: usize = 16 * 1024;
/// Redirect URIs one client may register.
pub const MAX_REDIRECT_URIS: usize = 10;
/// A redirect URI's length at most.
const MAX_URI: usize = 2000;
/// A client name's length kept (longer ones are cut).
const MAX_NAME: usize = 100;

/// A client as registered (RFC 7591, 3.2.1). No secret: it is public.
#[derive(Debug, Serialize, ToSchema)]
pub struct RegisteredClient {
    pub client_id: String,
    /// Seconds since the epoch.
    pub client_id_issued_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<&'static str>,
    pub response_types: Vec<&'static str>,
    /// Always `none`: a public client.
    pub token_endpoint_auth_method: &'static str,
}

/// Registers a client (RFC 7591): any MCP client may, without signing in.
#[utoipa::path(
    post,
    path = "/oauth/register",
    operation_id = "oauth_register",
    tag = "oauth",
    request_body(content = Object, description = "Client metadata (RFC 7591, 2): `redirect_uris` (required), `client_name`, `grant_types`, `response_types`, `token_endpoint_auth_method`"),
    responses(
        (status = 201, body = RegisteredClient),
        (status = 400, description = "`invalid_redirect_uri` or `invalid_client_metadata` (`{error, error_description}`)", body = Object),
        (status = 429, description = "Too many registrations from this client (`Retry-After`)", body = Object),
    )
)]
pub(crate) async fn register(
    State(state): State<AppState>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let ip = throttle::client_key(client.map(|Extension(ConnectInfo(addr))| addr.ip()));
    if let Err(crate::ApiError::RateLimited { retry_after }) =
        state.limits.oauth_registrations.check(ip)
    {
        return OAuthError::too_many(retry_after).into_response();
    }
    let bytes = match crate::extract::read_body(&headers, body, BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(err) => return err.into_response(),
    };
    let metadata = match read_metadata(&bytes) {
        Ok(metadata) => metadata,
        Err(err) => return err.into_response(),
    };
    let row = sqlx::query!(
        "INSERT INTO oauth_clients (name, redirect_uris) VALUES ($1, $2)
         RETURNING id, created_at",
        metadata.name,
        &metadata.redirect_uris,
    )
    .fetch_one(&state.db)
    .await;
    let row = match row {
        Ok(row) => row,
        Err(err) => return OAuthError::internal(&err).into_response(),
    };
    tracing::info!(client = %row.id, name = ?metadata.name, "oauth: client registered");
    (
        StatusCode::CREATED,
        super::no_store(),
        Json(registered(row.id, row.created_at, metadata)),
    )
        .into_response()
}

fn registered(id: Uuid, at: DateTime<Utc>, metadata: Metadata) -> RegisteredClient {
    RegisteredClient {
        client_id: id.to_string(),
        client_id_issued_at: at.timestamp(),
        client_name: metadata.name,
        redirect_uris: metadata.redirect_uris,
        grant_types: vec!["authorization_code", "refresh_token"],
        response_types: vec!["code"],
        token_endpoint_auth_method: "none",
    }
}

/// What romulus keeps of a registration.
#[derive(Debug, PartialEq, Eq)]
struct Metadata {
    name: Option<String>,
    redirect_uris: Vec<String>,
}

fn invalid_metadata(description: impl Into<String>) -> OAuthError {
    OAuthError::new(
        StatusCode::BAD_REQUEST,
        "invalid_client_metadata",
        description,
    )
}

fn read_metadata(bytes: &[u8]) -> Result<Metadata, OAuthError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| invalid_metadata("The body must be a JSON object of client metadata."))?;
    let Value::Object(fields) = value else {
        return Err(invalid_metadata(
            "The body must be a JSON object of client metadata.",
        ));
    };
    let strings = |name: &str| -> Result<Option<Vec<String>>, OAuthError> {
        match fields.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str().map(str::to_owned).ok_or_else(|| {
                        invalid_metadata(format!("`{name}` must be an array of strings."))
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(_) => Err(invalid_metadata(format!(
                "`{name}` must be an array of strings."
            ))),
        }
    };

    let redirect_uris = strings("redirect_uris")?.unwrap_or_default();
    if redirect_uris.is_empty() {
        return Err(OAuthError::new(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            "Register at least one redirect URI (`redirect_uris`).",
        ));
    }
    if redirect_uris.len() > MAX_REDIRECT_URIS {
        return Err(OAuthError::new(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            format!("Register {MAX_REDIRECT_URIS} redirect URIs at most."),
        ));
    }
    for uri in &redirect_uris {
        check_redirect_uri(uri).map_err(|why| {
            OAuthError::new(
                StatusCode::BAD_REQUEST,
                "invalid_redirect_uri",
                format!("{uri:?}: {why}"),
            )
        })?;
    }

    if let Some(grant_types) = strings("grant_types")? {
        if let Some(other) = grant_types
            .iter()
            .find(|grant| !matches!(grant.as_str(), "authorization_code" | "refresh_token"))
        {
            return Err(invalid_metadata(format!(
                "Grant type {other:?} is not supported: authorization_code and refresh_token only."
            )));
        }
        if !grant_types
            .iter()
            .any(|grant| grant == "authorization_code")
        {
            return Err(invalid_metadata(
                "`grant_types` must include authorization_code.",
            ));
        }
    }
    if let Some(response_types) = strings("response_types")?
        && response_types.iter().any(|kind| kind != "code")
    {
        return Err(invalid_metadata(
            "Only the `code` response type is supported.",
        ));
    }
    match fields.get("token_endpoint_auth_method") {
        None | Some(Value::Null | Value::String(_)) => {}
        Some(_) => {
            return Err(invalid_metadata(
                "`token_endpoint_auth_method` must be a string.",
            ));
        }
    }

    let name = match fields.get("client_name") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) => clean_name(name),
        Some(_) => return Err(invalid_metadata("`client_name` must be a string.")),
    };
    Ok(Metadata {
        name,
        redirect_uris,
    })
}

/// A client's name as it can be shown: controls removed, spaces collapsed,
/// cut at [`MAX_NAME`] characters; `None` if nothing is left.
fn clean_name(name: &str) -> Option<String> {
    let name = name
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let name: String = name.chars().take(MAX_NAME).collect();
    let name = name.trim_end().to_owned();
    (!name.is_empty()).then_some(name)
}

/// Whether `uri` may be registered as a redirect URI; `Err` says why not.
pub fn check_redirect_uri(uri: &str) -> Result<(), &'static str> {
    if uri.len() > MAX_URI {
        return Err("too long");
    }
    let url = Url::parse(uri).map_err(|_| "not an absolute URI")?;
    if url.fragment().is_some() {
        return Err("a redirect URI cannot have a fragment");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("a redirect URI cannot carry user info");
    }
    match url.scheme() {
        "https" if url.host().is_some() => Ok(()),
        "http" if is_loopback(&url) => Ok(()),
        "http" => Err(
            "http is accepted on a loopback address only (127.0.0.1, [::1], localhost): use https",
        ),
        _ => Err("use https, or http on a loopback address (127.0.0.1, [::1], localhost)"),
    }
}

/// `127.0.0.1` (any of 127/8), `[::1]` or `localhost`.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain(domain)) => domain == "localhost",
        None => false,
    }
}

/// Whether `presented` (an authorization request's `redirect_uri`) is the
/// registered `registered`: the same URI, except that the port of an http
/// loopback one is not compared.
pub fn redirect_matches(registered: &str, presented: &str) -> bool {
    let (Ok(mut registered), Ok(mut presented)) = (Url::parse(registered), Url::parse(presented))
    else {
        return false;
    };
    if presented.fragment().is_some() {
        return false;
    }
    let loopback = |url: &Url| url.scheme() == "http" && is_loopback(url);
    if loopback(&registered) && loopback(&presented) {
        // Neither can fail on an http URL with a host.
        let _ = registered.set_port(None);
        let _ = presented.set_port(None);
    }
    registered == presented
}

/// Whether every redirect URI of a client is a loopback one: the consent
/// screen warns that any program on the computer could be asking.
pub fn is_loopback_uri(uri: &str) -> bool {
    Url::parse(uri).is_ok_and(|url| url.scheme() == "http" && is_loopback(&url))
}

/// A redirect URI's host, as the consent screen shows it.
pub fn host_of(uri: &str) -> String {
    Url::parse(uri)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_and_loopback_http_are_accepted() {
        for uri in [
            "https://claude.ai/api/mcp/auth_callback",
            "https://vscode.dev/redirect",
            "http://localhost:3118/callback",
            "http://localhost/callback",
            "http://127.0.0.1:33418/",
            "http://[::1]:8080/cb?x=1",
        ] {
            assert_eq!(check_redirect_uri(uri), Ok(()), "{uri}");
        }
    }

    #[test]
    fn other_redirect_uris_are_refused() {
        for uri in [
            "http://claude.ai/api/mcp/auth_callback",
            "http://localhost.evil.test/cb",
            "http://10.0.0.1/cb",
            "cursor://anysphere.cursor-mcp/oauth/callback",
            "javascript:alert(1)",
            "data:text/html,hi",
            "https://claude.ai/cb#frag",
            "https://user:pass@claude.ai/cb",
            "/relative",
            "",
            &format!("https://x.test/{}", "a".repeat(2000)),
        ] {
            assert!(check_redirect_uri(uri).is_err(), "{uri}");
        }
    }

    #[test]
    fn loopback_redirects_match_on_any_port_and_others_exactly() {
        assert!(redirect_matches(
            "http://localhost/callback",
            "http://localhost:3118/callback"
        ));
        assert!(redirect_matches(
            "http://127.0.0.1:1/callback",
            "http://127.0.0.1:50000/callback"
        ));
        assert!(!redirect_matches(
            "http://localhost/callback",
            "http://127.0.0.1:3118/callback"
        ));
        assert!(!redirect_matches(
            "http://localhost/callback",
            "http://localhost:3118/other"
        ));
        assert!(redirect_matches(
            "https://claude.ai/api/mcp/auth_callback",
            "https://claude.ai/api/mcp/auth_callback"
        ));
        assert!(!redirect_matches(
            "https://claude.ai/api/mcp/auth_callback",
            "https://claude.ai:8443/api/mcp/auth_callback"
        ));
        assert!(!redirect_matches(
            "https://claude.ai/api/mcp/auth_callback",
            "https://claude.ai/api/mcp/auth_callback/"
        ));
        assert!(!redirect_matches(
            "https://claude.ai/cb",
            "https://claude.ai/cb?x=1"
        ));
        assert!(!redirect_matches(
            "https://claude.ai/cb",
            "https://claude.ai/cb#x"
        ));
    }

    #[test]
    fn registration_keeps_the_name_and_uris_and_refuses_the_rest() {
        let read = |value: Value| read_metadata(value.to_string().as_bytes());
        let ok = read(serde_json::json!({
            "client_name": "  Claude\nCode  ",
            "redirect_uris": ["http://localhost/callback"],
            "grant_types": ["authorization_code", "refresh_token"],
            "token_endpoint_auth_method": "client_secret_post",
            "logo_uri": "https://evil.test/pixel.png",
        }))
        .unwrap();
        assert_eq!(ok.name.as_deref(), Some("Claude Code"));
        assert_eq!(ok.redirect_uris, ["http://localhost/callback"]);

        for (body, error) in [
            (serde_json::json!({}), "invalid_redirect_uri"),
            (
                serde_json::json!({ "redirect_uris": ["http://evil.test/cb"] }),
                "invalid_redirect_uri",
            ),
            (
                serde_json::json!({ "redirect_uris": "https://x.test/cb" }),
                "invalid_client_metadata",
            ),
            (
                serde_json::json!({ "redirect_uris": ["https://x.test/cb"], "grant_types": ["client_credentials"] }),
                "invalid_client_metadata",
            ),
            (
                serde_json::json!({ "redirect_uris": ["https://x.test/cb"], "grant_types": ["refresh_token"] }),
                "invalid_client_metadata",
            ),
            (
                serde_json::json!({ "redirect_uris": ["https://x.test/cb"], "response_types": ["token"] }),
                "invalid_client_metadata",
            ),
            (
                serde_json::json!({ "redirect_uris": ["https://x.test/cb"], "client_name": 3 }),
                "invalid_client_metadata",
            ),
            (serde_json::json!([]), "invalid_client_metadata"),
        ] {
            assert_eq!(read(body.clone()).unwrap_err().error, error, "{body}");
        }
        let many: Vec<String> = (0..11).map(|i| format!("https://x.test/{i}")).collect();
        assert_eq!(
            read(serde_json::json!({ "redirect_uris": many }))
                .unwrap_err()
                .error,
            "invalid_redirect_uri"
        );
    }

    #[test]
    fn names_are_cleaned_and_cut() {
        assert_eq!(clean_name("\u{7}\t "), None);
        assert_eq!(clean_name(&"x".repeat(150)).unwrap().chars().count(), 100);
        assert_eq!(clean_name("Cursor").as_deref(), Some("Cursor"));
    }
}
