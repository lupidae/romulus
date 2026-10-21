//! The token endpoint (`POST /oauth/token`, OAuth 2.1, 3.2) and revocation
//! (`POST /oauth/revoke`, RFC 7009). Form bodies, OAuth's errors, no cookie.
//!
//! - `authorization_code`: the code (once, within [`CODE_TTL`], by the
//!   client it was made for), its PKCE verifier (S256), the redirect URI
//!   when the authorization named one, the `resource` when given (the code's
//!   own). The first exchange makes the grant and its first tokens; a code
//!   presented again revokes that grant (OAuth 2.1, 4.1.3). A code that
//!   fails a check is spent all the same.
//! - `refresh_token`: rotates. The refresh token is replaced by a new one
//!   with a new access token; a used one presented again is a replay (the
//!   token leaked, or two copies of the client run), which revokes the
//!   grant: every token of it stops working, and the person connects
//!   again. A refresh token lives [`REFRESH_IDLE`] from its last use, never
//!   past the grant's [`GRANT_TTL`].
//!
//! Clients are public: `client_id` in the body (or as the user of HTTP
//! Basic, which some clients send with an empty secret); an unknown one is
//! a 401 `invalid_client`, on which clients register again. Limits: per
//! client (`oauth_tokens`), and per client IP for refusals
//! (`oauth_failures`, checked before anything is read); revocations per
//! client IP (`oauth_revocations`).

use std::net::SocketAddr;

use axum::{
    Extension,
    body::Body,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    ACCESS_PREFIX, ACCESS_TTL, CODE_TTL, GRANT_TTL, OAuthError, Params, REFRESH_IDLE,
    REFRESH_PREFIX, apps, is_our_resource, parse_scope, scope_text,
};
use crate::{AppState, Json, auth::password::throttle, domain::AccessTokenScope, token};

/// A form body's size at most.
const BODY_LIMIT: usize = 16 * 1024;

/// Tokens issued (OAuth 2.1, 3.2.3).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TokenResponse {
    /// `rmls_oat_…`, for `Authorization: Bearer` at the MCP endpoint.
    pub access_token: String,
    /// `Bearer`.
    pub token_type: String,
    /// Seconds.
    pub expires_in: i64,
    /// `rmls_ort_…`, single use: each refresh answers a new one.
    pub refresh_token: String,
    /// `romulus.read`, or `romulus.read romulus.write`.
    pub scope: String,
}

/// Whether `value` has an OAuth access token's shape.
pub fn is_access_token(value: &str) -> bool {
    value
        .strip_prefix(ACCESS_PREFIX)
        .is_some_and(token::is_well_formed)
}

fn is_refresh_token(value: &str) -> bool {
    value
        .strip_prefix(REFRESH_PREFIX)
        .is_some_and(token::is_well_formed)
}

/// A PKCE verifier's shape (RFC 7636, 4.1): 43 to 128 unreserved
/// characters.
fn is_verifier(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// S256: base64url(sha256(verifier)) is the challenge.
fn verifies(verifier: &str, challenge: &str) -> bool {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) == challenge
}

/// The form's parameters: refused unless the body is a form, or when a
/// parameter comes twice.
async fn read_form(headers: &HeaderMap, body: Body) -> Result<Params, OAuthError> {
    let is_form = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|kind| {
            kind.trim()
                .eq_ignore_ascii_case("application/x-www-form-urlencoded")
        });
    if !is_form {
        return Err(OAuthError::invalid_request(
            "Send the parameters as a form: `Content-Type: application/x-www-form-urlencoded`.",
        ));
    }
    let bytes = crate::extract::read_body(headers, body, BODY_LIMIT)
        .await
        .map_err(|_| OAuthError::invalid_request("The body is too large."))?;
    Params::parse(&bytes)
        .map_err(|name| OAuthError::invalid_request(format!("`{name}` is sent more than once.")))
}

/// The client the request is from: `client_id`, or the user of HTTP Basic
/// (its secret is not checked: there is none).
fn client_id_of(params: &Params, headers: &HeaderMap) -> Result<Uuid, OAuthError> {
    let basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let (scheme, credentials) = value.split_once(' ')?;
            scheme.eq_ignore_ascii_case("basic").then_some(credentials)
        })
        .and_then(|credentials| STANDARD.decode(credentials.trim()).ok())
        .and_then(|decoded| String::from_utf8(decoded).ok())
        .and_then(|pair| {
            let user = pair.split(':').next()?;
            // Form-encoded (RFC 6749, 2.3.1).
            url::form_urlencoded::parse(format!("u={user}").as_bytes())
                .next()
                .map(|(_, value)| value.into_owned())
        });
    let id = match (params.get("client_id"), basic.as_deref()) {
        (Some(body), Some(basic)) if body != basic => {
            return Err(OAuthError::invalid_request(
                "`client_id` and the Authorization header name different clients.",
            ));
        }
        (Some(id), _) | (None, Some(id)) => id,
        (None, None) => {
            return Err(OAuthError::new(
                StatusCode::UNAUTHORIZED,
                "invalid_client",
                "Send `client_id`: romulus's clients are public.",
            ));
        }
    };
    Uuid::parse_str(id).map_err(|_| OAuthError::invalid_client())
}

/// Exchanges a code, or a refresh token, for tokens.
#[utoipa::path(
    post,
    path = "/oauth/token",
    operation_id = "oauth_token",
    tag = "oauth",
    request_body(content = String, content_type = "application/x-www-form-urlencoded", description = "`grant_type=authorization_code` with `code`, `code_verifier`, `client_id`, `redirect_uri`, `resource`; or `grant_type=refresh_token` with `refresh_token`, `client_id`, `scope`, `resource`"),
    responses(
        (status = 200, body = TokenResponse),
        (status = 400, description = "`invalid_request`, `invalid_grant`, `invalid_scope`, `invalid_target`, `unsupported_grant_type` (`{error, error_description}`)", body = Object),
        (status = 401, description = "`invalid_client`: register again", body = Object),
        (status = 429, description = "Too many requests (`Retry-After`)", body = Object),
    )
)]
pub(crate) async fn token(
    State(state): State<AppState>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let ip = throttle::client_key(client.map(|Extension(ConnectInfo(addr))| addr.ip()));
    if let Some(wait) = state.limits.oauth_failures.waiting(&ip) {
        return OAuthError::too_many(wait).into_response();
    }
    match exchange(&state, &headers, body).await {
        Ok(tokens) => (StatusCode::OK, super::no_store(), Json(tokens)).into_response(),
        Err(err) => {
            if matches!(err.error, "invalid_grant" | "invalid_client") {
                // Counted; past the burst this client is refused first.
                let _ = state.limits.oauth_failures.check(ip);
            }
            err.into_response()
        }
    }
}

async fn exchange(
    state: &AppState,
    headers: &HeaderMap,
    body: Body,
) -> Result<TokenResponse, OAuthError> {
    let params = read_form(headers, body).await?;
    let client_id = client_id_of(&params, headers)?;
    let known = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM oauth_clients WHERE id = $1) AS "known!""#,
        client_id
    )
    .fetch_one(&state.db)
    .await?;
    if !known {
        return Err(OAuthError::invalid_client());
    }
    if let Err(crate::ApiError::RateLimited { retry_after }) =
        state.limits.oauth_tokens.check(client_id)
    {
        return Err(OAuthError::too_many(retry_after));
    }
    match params.get("grant_type") {
        Some("authorization_code") => authorization_code(state, client_id, &params).await,
        Some("refresh_token") => refresh(state, client_id, &params).await,
        Some(other) => Err(OAuthError::new(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            format!("Grant type {other:?} is not supported: authorization_code or refresh_token."),
        )),
        None => Err(OAuthError::invalid_request("`grant_type` is required.")),
    }
}

fn invalid_target(state: &AppState) -> OAuthError {
    OAuthError::new(
        StatusCode::BAD_REQUEST,
        "invalid_target",
        format!(
            "romulus issues tokens for its MCP endpoint only: {}.",
            super::resource(&state.config)
        ),
    )
}

async fn authorization_code(
    state: &AppState,
    client_id: Uuid,
    params: &Params,
) -> Result<TokenResponse, OAuthError> {
    let code = params
        .get("code")
        .ok_or_else(|| OAuthError::invalid_request("`code` is required."))?;
    let verifier = params.get("code_verifier").ok_or_else(|| {
        OAuthError::invalid_request("`code_verifier` is required: romulus requires PKCE.")
    })?;
    let mut tx = state.db.begin().await?;
    let Some(found) = sqlx::query!(
        r#"SELECT client_id, user_id, redirect_uri, redirect_uri_given, code_challenge,
                  scope AS "scope: AccessTokenScope", resource, used_at, grant_id,
                  created_at > now() - make_interval(secs => $2) AS "fresh!"
           FROM oauth_codes WHERE code_hash = $1 FOR UPDATE"#,
        token::hash(code),
        CODE_TTL.as_secs_f64(),
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Err(OAuthError::invalid_grant("This code is not valid."));
    };
    if found.used_at.is_some() {
        // Presented again: whoever holds it is not to be trusted with what
        // it gave the first time either.
        if let Some(grant) = found.grant_id
            && apps::revoke_grant(&mut tx, grant).await?
        {
            tracing::warn!(%grant, client = %client_id, "oauth: a code was used twice, its grant revoked");
        }
        tx.commit().await?;
        return Err(OAuthError::invalid_grant(
            "This code was already used: the tokens it gave are revoked. Connect again.",
        ));
    }
    sqlx::query!(
        "UPDATE oauth_codes SET used_at = now() WHERE code_hash = $1",
        token::hash(code)
    )
    .execute(&mut *tx)
    .await?;
    let refused = refusal(
        state,
        params,
        &Presented {
            same_client: found.client_id == client_id,
            fresh: found.fresh,
            verifier,
            challenge: &found.code_challenge,
            redirect_uri: &found.redirect_uri,
            redirect_uri_given: found.redirect_uri_given,
        },
    );
    if let Some(refused) = refused {
        // The code is spent all the same.
        tx.commit().await?;
        return Err(refused);
    }
    let grant = sqlx::query_scalar!(
        "INSERT INTO oauth_grants (client_id, user_id, scope, resource, expires_at, idle_expires_at)
         VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5),
                 now() + make_interval(secs => least($5, $6)))
         RETURNING id",
        client_id,
        found.user_id,
        found.scope as AccessTokenScope,
        found.resource,
        GRANT_TTL.as_secs_f64(),
        REFRESH_IDLE.as_secs_f64(),
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE oauth_codes SET grant_id = $2 WHERE code_hash = $1",
        token::hash(code),
        grant
    )
    .execute(&mut *tx)
    .await?;
    let tokens = issue(&mut tx, grant, found.scope).await?;
    let notice = sqlx::query!(
        "UPDATE oauth_clients c SET last_used_at = now() FROM users u
         WHERE c.id = $1 AND u.id = $2
         RETURNING c.name, c.redirect_uris, u.email",
        client_id,
        found.user_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(user = %found.user_id, client = %client_id, %grant, "oauth: app connected");
    state.mailer.send(apps::connected_letter(
        &notice.email,
        notice.name.as_deref(),
        &found.redirect_uri,
        found.scope,
        &state.config.public_url,
    ));
    Ok(tokens)
}

/// What a code exchange is checked against.
struct Presented<'a> {
    same_client: bool,
    fresh: bool,
    verifier: &'a str,
    challenge: &'a str,
    redirect_uri: &'a str,
    redirect_uri_given: bool,
}

/// Why a code cannot be exchanged, if it cannot: another client, too old,
/// a verifier that does not match, another redirect URI, another resource.
fn refusal(state: &AppState, params: &Params, code: &Presented) -> Option<OAuthError> {
    let redirect_differs = match params.get("redirect_uri") {
        Some(uri) => uri != code.redirect_uri,
        None => code.redirect_uri_given,
    };
    if !code.same_client {
        Some(OAuthError::invalid_grant(
            "This code was issued to another client.",
        ))
    } else if !code.fresh {
        Some(OAuthError::invalid_grant(
            "This code expired (it lasts 60 seconds): connect again.",
        ))
    } else if !is_verifier(code.verifier) || !verifies(code.verifier, code.challenge) {
        Some(OAuthError::invalid_grant(
            "`code_verifier` does not match the code's challenge (PKCE S256).",
        ))
    } else if redirect_differs {
        Some(OAuthError::invalid_grant(
            "`redirect_uri` must be the one the authorization request named.",
        ))
    } else if params
        .get("resource")
        .is_some_and(|asked| !is_our_resource(&state.config, asked))
    {
        Some(invalid_target(state))
    } else {
        None
    }
}

async fn refresh(
    state: &AppState,
    client_id: Uuid,
    params: &Params,
) -> Result<TokenResponse, OAuthError> {
    let secret = params
        .get("refresh_token")
        .ok_or_else(|| OAuthError::invalid_request("`refresh_token` is required."))?;
    if !is_refresh_token(secret) {
        return Err(OAuthError::invalid_grant(
            "This refresh token is not valid.",
        ));
    }
    let mut tx = state.db.begin().await?;
    let Some(found) = sqlx::query!(
        r#"SELECT r.id, r.used_at, r.expires_at <= now() AS "expired!",
                  g.id AS grant_id, g.client_id, g.scope AS "scope: AccessTokenScope",
                  g.revoked_at, g.expires_at <= now() AS "grant_expired!"
           FROM oauth_refresh_tokens r JOIN oauth_grants g ON g.id = r.grant_id
           WHERE r.token_hash = $1
           FOR UPDATE OF r, g"#,
        token::hash(secret),
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Err(OAuthError::invalid_grant(
            "This refresh token is not valid.",
        ));
    };
    if found.revoked_at.is_some() {
        return Err(OAuthError::invalid_grant(
            "This app was disconnected from romulus: connect it again.",
        ));
    }
    if found.used_at.is_some() || found.client_id != client_id {
        apps::revoke_grant(&mut tx, found.grant_id).await?;
        tx.commit().await?;
        tracing::warn!(grant = %found.grant_id, client = %client_id, "oauth: a refresh token was replayed, its grant revoked");
        return Err(OAuthError::invalid_grant(
            "This refresh token was already used, so every token of this connection is revoked \
             (it may have leaked). Connect again.",
        ));
    }
    if found.expired || found.grant_expired {
        return Err(OAuthError::invalid_grant(
            "This connection expired: connect again.",
        ));
    }
    // Absent, the scope is the grant's; named, it may not be wider.
    if let Some(asked) = params.get("scope") {
        match parse_scope(Some(asked)) {
            Ok(AccessTokenScope::Write) if found.scope == AccessTokenScope::Read => {
                return Err(OAuthError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_scope",
                    "This connection was allowed to read only: connect again to allow more.",
                ));
            }
            Ok(_) => {}
            Err(unknown) => {
                return Err(OAuthError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_scope",
                    format!("Unknown scope {unknown:?}."),
                ));
            }
        }
    }
    if params
        .get("resource")
        .is_some_and(|asked| !is_our_resource(&state.config, asked))
    {
        return Err(invalid_target(state));
    }
    sqlx::query!(
        "UPDATE oauth_refresh_tokens SET used_at = now() WHERE id = $1",
        found.id
    )
    .execute(&mut *tx)
    .await?;
    let tokens = issue(&mut tx, found.grant_id, found.scope).await?;
    sqlx::query!(
        "UPDATE oauth_clients SET last_used_at = now() WHERE id = $1",
        client_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(tokens)
}

/// A new access token and refresh token for `grant`, neither outliving it;
/// the grant's idle expiry follows the refresh token's.
async fn issue(
    tx: &mut PgConnection,
    grant: Uuid,
    scope: AccessTokenScope,
) -> Result<TokenResponse, sqlx::Error> {
    let access = format!("{ACCESS_PREFIX}{}", token::generate());
    let refresh = format!("{REFRESH_PREFIX}{}", token::generate());
    let expires_in = sqlx::query_scalar!(
        r#"INSERT INTO oauth_access_tokens (grant_id, token_hash, expires_at)
           SELECT id, $2, least(now() + make_interval(secs => $3), expires_at)
           FROM oauth_grants WHERE id = $1
           RETURNING ceil(extract(epoch FROM expires_at - now()))::bigint AS "expires_in!""#,
        grant,
        token::hash(&access),
        ACCESS_TTL.as_secs_f64(),
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        "WITH issued AS (
             INSERT INTO oauth_refresh_tokens (grant_id, token_hash, expires_at)
             SELECT id, $2, least(now() + make_interval(secs => $3), expires_at)
             FROM oauth_grants WHERE id = $1
             RETURNING expires_at
         )
         UPDATE oauth_grants SET idle_expires_at = issued.expires_at, last_used_at = now()
         FROM issued WHERE oauth_grants.id = $1",
        grant,
        token::hash(&refresh),
        REFRESH_IDLE.as_secs_f64(),
    )
    .execute(&mut *tx)
    .await?;
    Ok(TokenResponse {
        access_token: access,
        token_type: "Bearer".to_owned(),
        expires_in,
        refresh_token: refresh,
        scope: scope_text(scope).to_owned(),
    })
}

/// Revokes a token (RFC 7009): a refresh token ends its whole grant, an
/// access token itself. Answers 200 whatever the token was, so it tells
/// nothing about tokens it did not revoke.
#[utoipa::path(
    post,
    path = "/oauth/revoke",
    operation_id = "oauth_revoke",
    tag = "oauth",
    request_body(content = String, content_type = "application/x-www-form-urlencoded", description = "`token`, `token_type_hint`, `client_id`"),
    responses(
        (status = 200, description = "Revoked, or nothing to revoke"),
        (status = 400, description = "`invalid_request`: no token", body = Object),
        (status = 429, description = "Too many requests (`Retry-After`)", body = Object),
    )
)]
pub(crate) async fn revoke(
    State(state): State<AppState>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let ip = throttle::client_key(client.map(|Extension(ConnectInfo(addr))| addr.ip()));
    if let Err(crate::ApiError::RateLimited { retry_after }) =
        state.limits.oauth_revocations.check(ip)
    {
        return OAuthError::too_many(retry_after).into_response();
    }
    match revoke_token(&state, &headers, body).await {
        Ok(()) => (StatusCode::OK, super::no_store()).into_response(),
        Err(err) => err.into_response(),
    }
}

async fn revoke_token(state: &AppState, headers: &HeaderMap, body: Body) -> Result<(), OAuthError> {
    let params = read_form(headers, body).await?;
    let secret = params
        .get("token")
        .ok_or_else(|| OAuthError::invalid_request("`token` is required."))?;
    // Only the client a token was issued to revokes it, when it says who
    // it is.
    let client = client_id_of(&params, headers).ok();
    if is_access_token(secret) {
        sqlx::query!(
            "UPDATE oauth_access_tokens a SET revoked_at = now() FROM oauth_grants g
             WHERE a.token_hash = $1 AND a.revoked_at IS NULL AND g.id = a.grant_id
               AND ($2::uuid IS NULL OR g.client_id = $2)",
            token::hash(secret),
            client,
        )
        .execute(&state.db)
        .await?;
    } else if is_refresh_token(secret) {
        let grant = sqlx::query_scalar!(
            "SELECT g.id FROM oauth_refresh_tokens r JOIN oauth_grants g ON g.id = r.grant_id
             WHERE r.token_hash = $1 AND ($2::uuid IS NULL OR g.client_id = $2)",
            token::hash(secret),
            client,
        )
        .fetch_optional(&state.db)
        .await?;
        if let Some(grant) = grant {
            let mut conn = state.db.acquire().await?;
            if apps::revoke_grant(&mut conn, grant).await? {
                tracing::info!(%grant, "oauth: revoked by its client");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_s256_matches_rfc_7636s_example() {
        // RFC 7636, appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert!(is_verifier(verifier));
        assert!(verifies(
            verifier,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        ));
        assert!(!verifies(verifier, verifier), "plain is not S256");
        assert!(!is_verifier("short"));
        assert!(!is_verifier(&"a".repeat(129)));
        assert!(!is_verifier(&format!("{}+", "a".repeat(43))));
    }

    #[test]
    fn tokens_are_told_apart_by_their_prefix() {
        let access = format!("{ACCESS_PREFIX}{}", token::generate());
        let refresh = format!("{REFRESH_PREFIX}{}", token::generate());
        assert!(is_access_token(&access) && !is_refresh_token(&access));
        assert!(is_refresh_token(&refresh) && !is_access_token(&refresh));
        assert!(!is_access_token(&format!("rmls_pat_{}", token::generate())));
    }

    #[test]
    fn a_client_is_named_in_the_body_or_by_http_basic() {
        let id = Uuid::new_v4();
        let params = Params::parse(format!("client_id={id}").as_bytes()).unwrap();
        assert_eq!(client_id_of(&params, &HeaderMap::new()).unwrap(), id);

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Basic {}", STANDARD.encode(format!("{id}:")))
                .parse()
                .unwrap(),
        );
        let empty = Params::parse(b"").unwrap();
        assert_eq!(client_id_of(&empty, &headers).unwrap(), id);

        let other = Params::parse(format!("client_id={}", Uuid::new_v4()).as_bytes()).unwrap();
        assert_eq!(
            client_id_of(&other, &headers).unwrap_err().error,
            "invalid_request"
        );
        assert_eq!(
            client_id_of(&empty, &HeaderMap::new()).unwrap_err().status,
            StatusCode::UNAUTHORIZED
        );
    }
}
