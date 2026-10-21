//! The authorization endpoint and the consent page's two calls.
//!
//! `GET /oauth/authorize` is a browser navigation, so it answers redirects,
//! never JSON:
//! - a request whose client or redirect URI cannot be trusted goes to the
//!   consent page with `?error=` (`unknown_client`, `invalid_redirect_uri`,
//!   `invalid_request`), never to the redirect URI (RFC 6749, 4.1.2.1);
//! - any other refusal goes back to the redirect URI with `error`,
//!   `error_description`, `state` and `iss`;
//! - a good one is kept in `oauth_requests` (at most [`REQUEST_TTL`]), and
//!   the browser goes to `/app/oauth/authorize?request=<id>`, with a cookie
//!   that ties the request to it: the page reads it and posts the decision
//!   from this browser only, so a link to someone else's request shows
//!   nothing and approves nothing. Signing in first is the page's business
//!   (it comes back).
//!
//! A request must carry `state`, a PKCE S256 `code_challenge` (`plain` is
//! refused) and, when it names one, the MCP endpoint as its `resource`.
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

/// The way back to the client: its redirect URI, `state` and `iss` added to
/// whatever the URI carries.
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

    fn error(&self, error: &str, description: &str) -> Response {
        Redirect::to(&self.with(&[("error", error), ("error_description", description)]))
            .into_response()
    }
}

/// The cookie that ties request `id` to the browser that made it.
fn binding_cookie_name(config: &Config, id: Uuid) -> String {
    format!("{}_oauth_{}", config.session_cookie, id.simple())
}

fn binding_cookie(config: &Config, id: Uuid, value: String) -> Cookie<'static> {
    Cookie::build((binding_cookie_name(config, id), value))
        .http_only(true)
        .secure(config.public_url.scheme() == "https")
        .same_site(SameSite::Lax)
        .path("/api/oauth")
        .max_age(cookie::time::Duration::seconds(
            i64::try_from(REQUEST_TTL.as_secs()).unwrap_or(i64::MAX),
        ))
        .build()
}

fn binding_removal(config: &Config, id: Uuid) -> Cookie<'static> {
    let mut cookie = binding_cookie(config, id, String::new());
    cookie.make_removal();
    cookie
}

/// The binding secret this browser holds for request `id`, hashed as
/// stored.
fn binding_hash(jar: &CookieJar, config: &Config, id: Uuid) -> Option<Vec<u8>> {
    jar.get(&binding_cookie_name(config, id))
        .map(Cookie::value)
        .filter(|value| token::is_well_formed(value))
        .map(token::hash)
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
        ("state" = String, Query, description = "Required"),
        ("code_challenge" = String, Query, description = "PKCE: base64url(sha256(verifier))"),
        ("code_challenge_method" = String, Query, description = "`S256` only"),
        ("scope" = Option<String>, Query, description = "`romulus.read`, `romulus.write` (both when absent)"),
        ("resource" = Option<String>, Query, description = "The MCP endpoint, `<public URL>/api/mcp` (RFC 8707)"),
    ),
    responses(
        (status = 303, description = "To the consent page (`/app/oauth/authorize?request=…`, with the cookie that binds the request to this browser); to the redirect URI with `error` for a bad request; to `/app/oauth/authorize?error=…` when the client or its redirect URI is unknown"),
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
        return back.error(
            "temporarily_unavailable",
            "Too many authorizations from this address: try again in a minute.",
        );
    }
    let (client_state, challenge, scope) =
        match checked(&state.config, &params, back.state.as_deref()) {
            Ok(checked) => checked,
            Err((error, description)) => return back.error(error, &description),
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
        client_state,
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
            return back.error(
                "server_error",
                "romulus could not take the request just now.",
            );
        }
    };
    let jar = jar.add(binding_cookie(&state.config, id, binding));
    (jar, Redirect::to(&format!("{PAGE}?request={id}"))).into_response()
}

/// The checks after the client and its redirect URI are known: `state`, the
/// response type, PKCE S256, the scope, the resource. Refusals go back to
/// the client.
fn checked<'a>(
    config: &Config,
    params: &'a Params,
    state: Option<&'a str>,
) -> Result<(&'a str, &'a str, AccessTokenScope), (&'static str, String)> {
    if params.get("response_type") != Some("code") {
        return Err((
            "unsupported_response_type",
            "romulus answers the authorization code flow only (`response_type=code`).".to_owned(),
        ));
    }
    let Some(client_state) = state else {
        return Err(("invalid_request", "`state` is required.".to_owned()));
    };
    if client_state.chars().count() > 1024 {
        return Err((
            "invalid_request",
            "`state` is too long (1024 characters at most).".to_owned(),
        ));
    }
    if params.get("code_challenge_method") != Some("S256") {
        return Err((
            "invalid_request",
            "PKCE is required, with `code_challenge_method=S256` (`plain` is refused).".to_owned(),
        ));
    }
    let Some(challenge) = params.get("code_challenge").filter(|c| is_challenge(c)) else {
        return Err((
            "invalid_request",
            "`code_challenge` must be base64url(sha256(code_verifier)), 43 characters.".to_owned(),
        ));
    };
    let scope = match parse_scope(params.get("scope")) {
        Ok(scope) => scope,
        Err(unknown) => {
            return Err((
                "invalid_scope",
                format!(
                    "Unknown scope {unknown:?}: romulus offers {} and {}.",
                    super::SCOPE_READ,
                    super::SCOPE_WRITE
                ),
            ));
        }
    };
    if let Some(asked) = params.get("resource")
        && !is_our_resource(config, asked)
    {
        return Err((
            "invalid_target",
            format!(
                "romulus issues tokens for its MCP endpoint only: {}.",
                resource(config)
            ),
        ));
    }
    Ok((client_state, challenge, scope))
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
        state: Some(request.state.clone()),
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
    Ok((
        jar.add(binding_removal(&state.config, id)),
        Json(Decided { redirect_url }),
    ))
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
    fn challenges_are_s256_shaped() {
        assert!(is_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(!is_challenge("short"));
        assert!(!is_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-c="));
    }

    #[test]
    fn the_binding_cookie_is_scoped_to_the_oauth_endpoints() {
        let config = Config::for_tests();
        let id = Uuid::nil();
        let cookie = binding_cookie(&config, id, "v".into());
        assert_eq!(cookie.path(), Some("/api/oauth"));
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.same_site(), Some(SameSite::Lax));
        assert!(cookie.name().ends_with(&id.simple().to_string()));
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
