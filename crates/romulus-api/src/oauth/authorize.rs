//! The authorization endpoint and the consent page's two calls.
//!
//! `GET /oauth/authorize` is a browser navigation, so it answers redirects,
//! never JSON:
//! - a request whose client or redirect URI cannot be trusted goes to the
//!   consent page with `?error=` (`unknown_client`, `invalid_redirect_uri`,
//!   `invalid_request`), never to the redirect URI (RFC 6749, 4.1.2.1);
//! - any other refusal ([`Refusal`]: the response type, PKCE, the scope, the
//!   resource, the rate limit) goes to the consent page too, with
//!   `?refused=` and what names the client and its redirect URI: the page
//!   asks `GET /oauth/refusals` for the way back, which checks those again
//!   against the client's registration, and shows it as a "Return to
//!   <host>" link. romulus never sends a browser to a redirect URI on its
//!   own before the person decided: clients register themselves, so an
//!   automatic redirect there would make romulus an open redirect;
//! - a good one is kept in `oauth_requests` (at most [`REQUEST_TTL`]), and
//!   the browser goes to `/app/oauth/authorize?request=<id>`, with a cookie
//!   that ties the request to it: the page reads it and posts the decision
//!   from this browser only, so a link to someone else's request shows
//!   nothing and approves nothing. One cookie holds the browser's last
//!   [`MAX_BINDINGS`] requests. Signing in first is the page's business (it
//!   comes back). Only the person's decision (a code, or `access_denied`)
//!   sends the browser back to the client by itself.
//!
//! A request must carry a PKCE S256 `code_challenge` (`plain` is refused)
//! and, when it names one, the MCP endpoint as its `resource`. `state` is
//! the client's own and optional (OAuth 2.1 with PKCE; some MCP clients
//! omit it): romulus echoes it back unchanged when it is sent, and never
//! needs it, since PKCE is what binds the code to the client that asked.
//! Approving makes a code ([`super::CODE_TTL`], single use) for the person, with
//! the scope they allowed: what was asked, or read only.

use std::net::SocketAddr;

use axum::{
    Extension,
    extract::{ConnectInfo, RawQuery, State},
    response::{IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    Params, REQUEST_TTL,
    clients::{self, redirect_matches},
    is_our_resource, issuer, parse_scope, resource,
};
use crate::{
    ApiError, ApiResult, AppState, Config,
    auth::{CurrentUser, password::throttle},
    domain::AccessTokenScope,
    error::ErrorBody,
    extract::{Json, Path},
    token,
};

/// The consent page.
const PAGE: &str = "/app/oauth/authorize";

/// Where a refusal that cannot go back to the client lands: the consent
/// page, which explains `code`.
fn dead_end(code: &str) -> Response {
    Redirect::to(&format!("{PAGE}?error={code}")).into_response()
}

/// The way back to the client: its redirect URI, `state` (if the request
/// had one) and `iss` added to whatever the URI carries.
struct Back {
    redirect_uri: String,
    state: Option<String>,
    issuer: String,
}

impl Back {
    fn with(&self, pairs: &[(&str, &str)]) -> String {
        let mut url = Url::parse(&self.redirect_uri).expect("a registered redirect URI parses");
        {
            let mut query = url.query_pairs_mut();
            for (name, value) in pairs {
                query.append_pair(name, value);
            }
            if let Some(state) = &self.state {
                query.append_pair("state", state);
            }
            query.append_pair("iss", &self.issuer);
        }
        url.into()
    }
}

/// Requests one browser keeps bound at once: a person connecting two
/// agents together still can, and the cookie stays small whatever a page
/// keeps asking.
pub const MAX_BINDINGS: usize = 3;

/// The cookie that ties requests to the browser that made them: up to
/// [`MAX_BINDINGS`] `<request id>.<secret>` entries, newest first, joined by
/// `~`.
fn binding_cookie_name(config: &Config) -> String {
    format!("{}_oauth", config.session_cookie)
}

fn binding_cookie(config: &Config, entries: &[(Uuid, String)]) -> Cookie<'static> {
    let value = entries
        .iter()
        .map(|(id, secret)| format!("{}.{secret}", id.simple()))
        .collect::<Vec<_>>()
        .join("~");
    let mut cookie = Cookie::build((binding_cookie_name(config), value))
        .http_only(true)
        .secure(config.public_url.scheme() == "https")
        .same_site(SameSite::Lax)
        .path("/api/oauth")
        .max_age(cookie::time::Duration::seconds(
            i64::try_from(REQUEST_TTL.as_secs()).unwrap_or(i64::MAX),
        ))
        .build();
    if entries.is_empty() {
        cookie.make_removal();
    }
    cookie
}

/// The requests this browser holds bindings for, newest first (malformed
/// entries skipped, at most [`MAX_BINDINGS`]).
fn bindings(jar: &CookieJar, config: &Config) -> Vec<(Uuid, String)> {
    jar.get(&binding_cookie_name(config))
        .map_or("", Cookie::value)
        .split('~')
        .filter_map(|entry| {
            let (id, secret) = entry.split_once('.')?;
            let id = Uuid::try_parse(id).ok()?;
            token::is_well_formed(secret).then(|| (id, secret.to_owned()))
        })
        .take(MAX_BINDINGS)
        .collect()
}

/// The binding secret this browser holds for request `id`, hashed as
/// stored.
fn binding_hash(jar: &CookieJar, config: &Config, id: Uuid) -> Option<Vec<u8>> {
    bindings(jar, config)
        .into_iter()
        .find(|(entry, _)| *entry == id)
        .map(|(_, secret)| token::hash(&secret))
}

/// Whether `value` is a PKCE S256 challenge: base64url of 32 bytes, no
/// padding.
fn is_challenge(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Starts an authorization (OAuth 2.1, 4.1.1): checks it, then sends the
/// browser to the consent page.
#[utoipa::path(
    get,
    path = "/oauth/authorize",
    operation_id = "oauth_authorize",
    tag = "oauth",
    params(
        ("response_type" = String, Query, description = "`code`"),
        ("client_id" = String, Query, description = "From registration"),
        ("redirect_uri" = Option<String>, Query, description = "One the client registered (any port on a loopback one); optional when it registered only one"),
        ("state" = Option<String>, Query, description = "Optional (PKCE protects the flow): echoed back unchanged, 1024 characters at most"),
        ("code_challenge" = String, Query, description = "PKCE: base64url(sha256(verifier))"),
        ("code_challenge_method" = String, Query, description = "`S256` only"),
        ("scope" = Option<String>, Query, description = "`romulus.read`, `romulus.write` (both when absent)"),
        ("resource" = Option<String>, Query, description = "The MCP endpoint, `<public URL>/api/mcp` (RFC 8707)"),
    ),
    responses(
        (status = 303, description = "Always to the consent page, never to the redirect URI: `/app/oauth/authorize?request=…` (with the cookie that binds the request to this browser); `?refused=…&client_id=…&redirect_uri=…[&state=…]` for a bad request (the page offers the way back); `?error=…` when the client or its redirect URI is unknown"),
    )
)]
pub(crate) async fn authorize(
    State(state): State<AppState>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    jar: CookieJar,
    RawQuery(query): RawQuery,
) -> Response {
    let Ok(params) = Params::parse(query.as_deref().unwrap_or("").as_bytes()) else {
        return dead_end("invalid_request");
    };
    // Who is asking, and where the answer goes: until both are known, a
    // refusal never leaves romulus.
    let Some(client_id) = params
        .get("client_id")
        .and_then(|id| Uuid::parse_str(id).ok())
    else {
        return dead_end("unknown_client");
    };
    let registered = match sqlx::query!(
        "SELECT redirect_uris FROM oauth_clients WHERE id = $1",
        client_id
    )
    .fetch_optional(&state.db)
    .await
    {
        Ok(Some(row)) => row.redirect_uris,
        Ok(None) => return dead_end("unknown_client"),
        Err(err) => {
            tracing::error!(error = %err, "oauth: cannot read a client");
            return dead_end("unavailable");
        }
    };
    let (redirect_uri, given) = match params.get("redirect_uri") {
        Some(uri) if registered.iter().any(|known| redirect_matches(known, uri)) => {
            (uri.to_owned(), true)
        }
        None if registered.len() == 1 => (registered[0].clone(), false),
        _ => return dead_end("invalid_redirect_uri"),
    };
    let back = Back {
        redirect_uri,
        state: params.get("state").map(str::to_owned),
        issuer: issuer(&state.config),
    };

    let ip = throttle::client_key(client.map(|Extension(ConnectInfo(addr))| addr.ip()));
    if state.limits.oauth_authorizations.check(ip).is_err() {
        return refused(Refusal::Busy, client_id, &back);
    }
    let (challenge, scope) = match checked(&state.config, &params, back.state.as_deref()) {
        Ok(checked) => checked,
        Err(refusal) => return refused(refusal, client_id, &back),
    };

    let binding = token::generate();
    let stored = sqlx::query_scalar!(
        "INSERT INTO oauth_requests
             (binding_hash, client_id, redirect_uri, redirect_uri_given, state, code_challenge,
              scope, resource)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         RETURNING id",
        token::hash(&binding),
        client_id,
        back.redirect_uri,
        given,
        back.state.as_deref(),
        challenge,
        scope as AccessTokenScope,
        resource(&state.config),
    )
    .fetch_one(&state.db)
    .await;
    let id = match stored {
        Ok(id) => id,
        Err(err) => {
            tracing::error!(error = %err, "oauth: cannot keep an authorization request");
            return refused(Refusal::ServerError, client_id, &back);
        }
    };
    let mut entries = bindings(&jar, &state.config);
    entries.insert(0, (id, binding));
    entries.truncate(MAX_BINDINGS);
    let jar = jar.add(binding_cookie(&state.config, &entries));
    (jar, Redirect::to(&format!("{PAGE}?request={id}"))).into_response()
}

/// Why an authorization request was refused once its client and redirect
/// URI were known: what the client is told (`error`, `error_description`)
/// when the person takes the way back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Busy,
    ResponseType,
    StateTooLong,
    PkceMethod,
    CodeChallenge,
    Scope,
    Resource,
    ServerError,
}

impl Refusal {
    const ALL: [Self; 8] = [
        Self::Busy,
        Self::ResponseType,
        Self::StateTooLong,
        Self::PkceMethod,
        Self::CodeChallenge,
        Self::Scope,
        Self::Resource,
        Self::ServerError,
    ];

    /// Its name in the consent page's URL (`?refused=`).
    pub fn key(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::ResponseType => "response_type",
            Self::StateTooLong => "state",
            Self::PkceMethod => "pkce",
            Self::CodeChallenge => "code_challenge",
            Self::Scope => "scope",
            Self::Resource => "resource",
            Self::ServerError => "server_error",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|refusal| refusal.key() == key)
    }

    /// The OAuth error (RFC 6749, 4.1.2.1; RFC 8707).
    pub fn error(self) -> &'static str {
        match self {
            Self::Busy => "temporarily_unavailable",
            Self::ResponseType => "unsupported_response_type",
            Self::StateTooLong | Self::PkceMethod | Self::CodeChallenge => "invalid_request",
            Self::Scope => "invalid_scope",
            Self::Resource => "invalid_target",
            Self::ServerError => "server_error",
        }
    }

    /// What went wrong, for the client and the person: fixed sentences,
    /// never text from the request.
    pub fn description(self, config: &Config) -> String {
        match self {
            Self::Busy => {
                "Too many authorizations from this address: try again in a minute.".to_owned()
            }
            Self::ResponseType => {
                "romulus answers the authorization code flow only (`response_type=code`)."
                    .to_owned()
            }
            Self::StateTooLong => "`state` is too long (1024 characters at most).".to_owned(),
            Self::PkceMethod => {
                "PKCE is required, with `code_challenge_method=S256` (`plain` is refused)."
                    .to_owned()
            }
            Self::CodeChallenge => {
                "`code_challenge` must be base64url(sha256(code_verifier)), 43 characters."
                    .to_owned()
            }
            Self::Scope => format!(
                "Unknown scope: romulus offers {} and {}.",
                super::SCOPE_READ,
                super::SCOPE_WRITE
            ),
            Self::Resource => format!(
                "romulus issues tokens for its MCP endpoint only: {}.",
                resource(config)
            ),
            Self::ServerError => "romulus could not take the request just now.".to_owned(),
        }
    }
}

/// The consent page for a refusal: what it was, and what names the way back
/// (checked again by `GET /oauth/refusals` before it is shown).
fn refused(refusal: Refusal, client_id: Uuid, back: &Back) -> Response {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query
        .append_pair("refused", refusal.key())
        .append_pair("client_id", &client_id.to_string())
        .append_pair("redirect_uri", &back.redirect_uri);
    if let Some(state) = &back.state {
        query.append_pair("state", state);
    }
    Redirect::to(&format!("{PAGE}?{}", query.finish())).into_response()
}

/// The checks after the client and its redirect URI are known: the
/// response type, `state`'s length when sent, PKCE S256, the scope, the
/// resource.
fn checked<'a>(
    config: &Config,
    params: &'a Params,
    state: Option<&str>,
) -> Result<(&'a str, AccessTokenScope), Refusal> {
    if params.get("response_type") != Some("code") {
        return Err(Refusal::ResponseType);
    }
    if state.is_some_and(|state| state.chars().count() > 1024) {
        return Err(Refusal::StateTooLong);
    }
    if params.get("code_challenge_method") != Some("S256") {
        return Err(Refusal::PkceMethod);
    }
    let challenge = params
        .get("code_challenge")
        .filter(|c| is_challenge(c))
        .ok_or(Refusal::CodeChallenge)?;
    let scope = parse_scope(params.get("scope")).map_err(|_| Refusal::Scope)?;
    if params
        .get("resource")
        .is_some_and(|asked| !is_our_resource(config, asked))
    {
        return Err(Refusal::Resource);
    }
    Ok((challenge, scope))
}

/// A request for the person to decide on.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AuthorizationRequest {
    pub id: Uuid,
    pub client: RequestingClient,
    /// What it asks for: `write` is everything the person's roles allow.
    pub scope: AccessTokenScope,
    /// Decide before then.
    pub expires_at: DateTime<Utc>,
}

/// The client asking, as far as romulus can tell.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RequestingClient {
    pub id: Uuid,
    /// The name it registered under, unverified; `null` if it gave none.
    pub name: Option<String>,
    /// Where the browser goes back to: the part that can be trusted.
    pub redirect_host: String,
    /// The redirect URI is a loopback address: a program on this computer.
    pub loopback: bool,
    pub registered_at: DateTime<Utc>,
}

fn request_gone() -> ApiError {
    ApiError::NotFound(
        "This request to connect an app is over: it expired, was already answered, or was \
         started in another browser. Connect again from your agent."
            .into(),
    )
}

/// Requests are decided by the person, signed in: never with a token.
fn by_the_person(user: &CurrentUser) -> ApiResult<()> {
    if user.via.is_some() {
        return Err(ApiError::Denied {
            code: "token_not_allowed",
            message: "Apps are connected in romulus, signed in: not with a token.".into(),
        });
    }
    Ok(())
}

/// The authorization request this browser started, for the consent page.
#[utoipa::path(
    get,
    path = "/oauth/requests/{id}",
    operation_id = "get_authorization_request",
    tag = "oauth",
    params(("id" = Uuid, Path, description = "From the consent page's `?request=`")),
    responses(
        (status = 200, body = AuthorizationRequest),
        (status = 401, body = ErrorBody),
        (status = 404, description = "Expired, answered, or started in another browser", body = ErrorBody),
    )
)]
pub(crate) async fn request(
    user: CurrentUser,
    State(state): State<AppState>,
    jar: CookieJar,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AuthorizationRequest>> {
    by_the_person(&user)?;
    let binding = binding_hash(&jar, &state.config, id).ok_or_else(request_gone)?;
    let row = sqlx::query!(
        r#"SELECT r.scope AS "scope: AccessTokenScope", r.redirect_uri,
                  r.created_at + make_interval(secs => $3) AS "expires_at!",
                  c.id AS client_id, c.name, c.created_at AS registered_at
           FROM oauth_requests r JOIN oauth_clients c ON c.id = r.client_id
           WHERE r.id = $1 AND r.binding_hash = $2
             AND r.created_at > now() - make_interval(secs => $3)"#,
        id,
        binding,
        REQUEST_TTL.as_secs_f64(),
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(request_gone)?;
    Ok(Json(AuthorizationRequest {
        id,
        client: RequestingClient {
            id: row.client_id,
            name: row.name,
            redirect_host: clients::host_of(&row.redirect_uri),
            loopback: clients::is_loopback_uri(&row.redirect_uri),
            registered_at: row.registered_at,
        },
        scope: row.scope,
        expires_at: row.expires_at,
    }))
}

/// The person's answer.
#[derive(Debug, Deserialize, ToSchema)]
pub struct Decision {
    pub allow: bool,
    /// Less than asked: `read` when the app asked for `write`. Absent: what
    /// it asked.
    #[serde(default)]
    pub scope: Option<AccessTokenScope>,
}

/// Where the browser goes now.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct Decided {
    /// The app's redirect URI with a code (allowed) or `error=access_denied`,
    /// `state` and `iss`.
    pub redirect_url: String,
}

/// Allows or denies an authorization request: answers where to send the
/// browser (back to the app).
#[utoipa::path(
    post,
    path = "/oauth/requests/{id}",
    operation_id = "decide_authorization_request",
    tag = "oauth",
    params(("id" = Uuid, Path, description = "From the consent page's `?request=`")),
    request_body = Decision,
    responses(
        (status = 200, body = Decided),
        (status = 401, body = ErrorBody),
        (status = 404, description = "Expired, answered, or started in another browser", body = ErrorBody),
        (status = 422, description = "A scope wider than the app asked for", body = ErrorBody),
    )
)]
pub(crate) async fn decide(
    user: CurrentUser,
    State(state): State<AppState>,
    jar: CookieJar,
    Path(id): Path<Uuid>,
    Json(decision): Json<Decision>,
) -> ApiResult<(CookieJar, Json<Decided>)> {
    by_the_person(&user)?;
    let binding = binding_hash(&jar, &state.config, id).ok_or_else(request_gone)?;
    let mut tx = state.db.begin().await?;
    // Consumed by the first decision, whatever it is.
    let request = sqlx::query!(
        r#"DELETE FROM oauth_requests
           WHERE id = $1 AND binding_hash = $2
             AND created_at > now() - make_interval(secs => $3)
           RETURNING client_id, redirect_uri, redirect_uri_given, state, code_challenge,
                     scope AS "scope: AccessTokenScope", resource"#,
        id,
        binding,
        REQUEST_TTL.as_secs_f64(),
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(request_gone)?;
    let back = Back {
        redirect_uri: request.redirect_uri.clone(),
        state: request.state.clone(),
        issuer: issuer(&state.config),
    };
    let redirect_url = if decision.allow {
        let scope = match (request.scope, decision.scope) {
            (AccessTokenScope::Read, Some(AccessTokenScope::Write)) => {
                return Err(crate::orgs::invalid(
                    "scope",
                    "The app asked for read access only.",
                ));
            }
            (_, Some(narrower)) => narrower,
            (asked, None) => asked,
        };
        let code = token::generate();
        sqlx::query!(
            "INSERT INTO oauth_codes
                 (code_hash, client_id, user_id, redirect_uri, redirect_uri_given,
                  code_challenge, scope, resource)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            token::hash(&code),
            request.client_id,
            user.id,
            request.redirect_uri,
            request.redirect_uri_given,
            request.code_challenge,
            scope as AccessTokenScope,
            request.resource,
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "UPDATE oauth_clients SET last_used_at = now() WHERE id = $1",
            request.client_id
        )
        .execute(&mut *tx)
        .await?;
        tracing::info!(user = %user.id, client = %request.client_id, ?scope, "oauth: approved");
        back.with(&[("code", &code)])
    } else {
        tracing::info!(user = %user.id, client = %request.client_id, "oauth: denied");
        back.with(&[
            ("error", "access_denied"),
            (
                "error_description",
                "The person did not allow this app to use romulus.",
            ),
        ])
    };
    tx.commit().await?;
    let left: Vec<_> = bindings(&jar, &state.config)
        .into_iter()
        .filter(|(entry, _)| *entry != id)
        .collect();
    Ok((
        jar.add(binding_cookie(&state.config, &left)),
        Json(Decided { redirect_url }),
    ))
}

/// A refused authorization request, as the consent page shows it.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RefusedRequest {
    /// The OAuth error the client is told (`invalid_request`…).
    pub error: String,
    /// What went wrong, in a sentence.
    pub description: String,
    /// The client's redirect URI with `error`, `error_description`, `state`
    /// (when it sent one) and `iss`: one of its registered URIs, checked
    /// again here.
    pub return_url: String,
    /// The return URL's host, as the link names it.
    pub redirect_host: String,
    /// A program on this computer.
    pub loopback: bool,
    /// The name the client registered under, unverified.
    pub client_name: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RefusalQuery {
    /// The consent page's `?refused=`.
    pub reason: String,
    pub client_id: String,
    pub redirect_uri: String,
    /// The request's own, when it had one.
    #[serde(default)]
    pub state: Option<String>,
}

/// The way back to a client after its request was refused, for the
/// consent page's "Return to" link. Signed out too: refusals come before
/// signing in. Answers only for a registered client and one of its
/// redirect URIs, so the link can lead nowhere else.
#[utoipa::path(
    get,
    path = "/oauth/refusals",
    operation_id = "get_authorization_refusal",
    tag = "oauth",
    params(RefusalQuery),
    responses(
        (status = 200, body = RefusedRequest),
        (status = 404, description = "Not a refusal romulus makes, an unknown client, or a redirect URI it did not register", body = ErrorBody),
    )
)]
pub(crate) async fn refusal(
    State(state): State<AppState>,
    crate::extract::Query(query): crate::extract::Query<RefusalQuery>,
) -> ApiResult<Json<RefusedRequest>> {
    let not_one = || ApiError::not_found("This is not a request romulus refused.");
    let refusal = Refusal::from_key(&query.reason).ok_or_else(not_one)?;
    let client_id = Uuid::try_parse(&query.client_id).map_err(|_| not_one())?;
    let client = sqlx::query!(
        "SELECT name, redirect_uris FROM oauth_clients WHERE id = $1",
        client_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(not_one)?;
    if !client
        .redirect_uris
        .iter()
        .any(|known| redirect_matches(known, &query.redirect_uri))
    {
        return Err(not_one());
    }
    let state_param = query.state.filter(|value| !value.is_empty());
    let description = refusal.description(&state.config);
    let back = Back {
        redirect_uri: query.redirect_uri,
        state: state_param,
        issuer: issuer(&state.config),
    };
    let return_url = back.with(&[
        ("error", refusal.error()),
        ("error_description", &description),
    ]);
    Ok(Json(RefusedRequest {
        error: refusal.error().to_owned(),
        description,
        redirect_host: clients::host_of(&back.redirect_uri),
        loopback: clients::is_loopback_uri(&back.redirect_uri),
        client_name: client.name,
        return_url,
    }))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;

    #[test]
    fn the_way_back_keeps_the_uri_query_and_adds_state_and_iss() {
        let back = Back {
            redirect_uri: "http://localhost:3118/callback?x=1".into(),
            state: Some("s t".into()),
            issuer: "https://romulus.example".into(),
        };
        assert_eq!(
            back.with(&[("code", "abc")]),
            "http://localhost:3118/callback?x=1&code=abc&state=s+t&iss=https%3A%2F%2Fromulus.example"
        );
    }

    #[test]
    fn without_a_state_none_is_added() {
        let back = Back {
            redirect_uri: "https://claude.ai/api/mcp/auth_callback".into(),
            state: None,
            issuer: "https://romulus.example".into(),
        };
        assert_eq!(
            back.with(&[("code", "abc")]),
            "https://claude.ai/api/mcp/auth_callback?code=abc&iss=https%3A%2F%2Fromulus.example"
        );
    }

    #[test]
    fn challenges_are_s256_shaped() {
        assert!(is_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(!is_challenge("short"));
        assert!(!is_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-c="));
    }

    #[test]
    fn one_binding_cookie_keeps_the_latest_requests() {
        let config = Config::for_tests();
        let entries: Vec<_> = (0..MAX_BINDINGS)
            .map(|n| (Uuid::from_u128(n as u128 + 1), token::generate()))
            .collect();
        let cookie = binding_cookie(&config, &entries);
        assert_eq!(cookie.path(), Some("/api/oauth"));
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.same_site(), Some(SameSite::Lax));
        assert_eq!(cookie.name(), format!("{}_oauth", config.session_cookie));
        let jar = CookieJar::new().add(cookie);
        assert_eq!(bindings(&jar, &config), entries);
        assert_eq!(
            binding_hash(&jar, &config, entries[1].0),
            Some(token::hash(&entries[1].1))
        );
        assert_eq!(binding_hash(&jar, &config, Uuid::nil()), None);
        // Garbage in the cookie is skipped; nothing left removes it.
        let jar = CookieJar::new().add(Cookie::new(
            binding_cookie_name(&config),
            format!("junk~{}.{}", Uuid::nil().simple(), entries[0].1),
        ));
        assert_eq!(
            bindings(&jar, &config),
            [(Uuid::nil(), entries[0].1.clone())]
        );
        assert_eq!(binding_cookie(&config, &[]).value(), "");
    }

    #[test]
    fn refusals_round_trip_their_key_and_never_echo_the_request() {
        let config = Config::for_tests();
        for refusal in Refusal::ALL {
            assert_eq!(Refusal::from_key(refusal.key()), Some(refusal));
            assert_ne!(refusal.description(&config), "");
        }
        assert_eq!(Refusal::from_key("anything"), None);
        let back = Back {
            redirect_uri: "http://localhost:9/cb".into(),
            state: Some("s".into()),
            issuer: "http://localhost:41000".into(),
        };
        let response = refused(Refusal::PkceMethod, Uuid::nil(), &back);
        let location = response.headers()["location"].to_str().unwrap();
        assert!(
            location.starts_with("/app/oauth/authorize?refused=pkce&client_id="),
            "{location}"
        );
        assert!(location.contains("&state=s"), "{location}");
    }

    #[test]
    fn dead_ends_land_on_the_consent_page() {
        let response = dead_end("unknown_client");
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()["location"],
            "/app/oauth/authorize?error=unknown_client"
        );
    }
}
