//! Sign-in through Zitadel (OIDC code flow + PKCE, done by the API as a
//! backend-for-frontend) or with an email and a password ([`password`],
//! `ROMULUS_AUTH`), server-side sessions, and the extractors every other
//! module authenticates with. Both methods end in the same session.
//!
//! Endpoints: `GET /auth/methods` (what the sign-in page offers),
//! `/auth/password/…` (email + password sign-in: [`password`]),
//! `GET /auth/login?next=` (redirect to the IdP), `GET /auth/callback` (the
//! redirect URI the Zitadel provisioner registers), `POST /auth/logout`,
//! `GET /me`. Tables: `users`, `sessions`, `oidc_flows`. The session cookie is
//! `Config::session_cookie`; see [`session`] for its lifetime.
//!
//! Extractors: [`CurrentUser`] (any signed-in person) and [`Member`] (a
//! member of the `{org}` in the path with at least a role: see [`member`]).
//! Requests with `Authorization: Bearer` authenticate by API token only and
//! never as a [`CurrentUser`] (see `crate::csrf`).
//!
//! Browser navigations (login, callback, dev sign-in) answer with redirects,
//! never JSON: a failure lands on `/app/signin?error=<code>`, whose page
//! explains the code (`sso_unavailable`, `sign_in_expired`, `access_denied`,
//! `email_unverified`, `sso_failed`).
//!
//! Dev sign-in: when `Config::dev_signin` is true (env `ROMULUS_DEV_SIGNIN`,
//! which config refuses unless the public URL is loopback),
//! `GET /auth/dev-signin?email=…&next=…[&name=…][&verified=false]` signs in
//! as that user (created if needed, verified unless asked otherwise) without
//! Zitadel, for QA and the Playwright suite. Otherwise it answers 404 like any
//! unknown path, and it is not in the OpenAPI document.

pub mod events;
pub mod limit;
pub mod member;
pub mod next;
pub mod oidc;
pub mod password;
pub mod session;

use axum::{
    Extension,
    extract::{ConnectInfo, State},
    http::Method,
    response::{IntoResponse, Redirect, Response},
    routing::any,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

use self::oidc::Identity;
pub use self::{
    member::{Member, OrgRef, min},
    session::CurrentUser,
};
use crate::{
    ApiError, ApiResult, AppState, Config,
    error::{Chain, ErrorBody},
    extract::{Json, Query},
    token,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(methods))
        .routes(routes!(login))
        .routes(routes!(callback))
        .routes(routes!(logout))
        .routes(routes!(me))
        .merge(password::router())
        // Undocumented: it only exists in development (see the module docs).
        .route("/auth/dev-signin", any(dev_signin))
}

/// How long a sign-in may take at the IdP before its callback is refused.
const FLOW_TTL: std::time::Duration = std::time::Duration::from_mins(10);

/// The cookie that ties a callback to the browser that started the sign-in
/// (login CSRF): it holds the `state` the IdP must hand back, and where to
/// land afterwards. `next` lives here rather than in `oidc_flows` because it
/// can carry a secret (an invitation link's token) that the database must
/// not hold in clear; the browser already has it. It is checked again at
/// the callback.
fn flow_cookie_name(config: &Config) -> String {
    format!("{}_signin", config.session_cookie)
}

fn flow_cookie_value(state: &str, next: &str) -> String {
    format!("{state}.{}", URL_SAFE_NO_PAD.encode(next))
}

/// The state and the `next` a flow cookie holds, if it is well formed.
fn read_flow_cookie(value: &str) -> Option<(&str, String)> {
    let (state, next) = value.split_once('.')?;
    let next = String::from_utf8(URL_SAFE_NO_PAD.decode(next).ok()?).ok()?;
    Some((state, next))
}

fn flow_cookie(config: &Config, value: String) -> Cookie<'static> {
    Cookie::build((flow_cookie_name(config), value))
        .http_only(true)
        .secure(config.public_url.scheme() == "https")
        .same_site(SameSite::Lax)
        .path("/api/auth")
        .max_age(session::cookie_duration(FLOW_TTL))
        .build()
}

fn flow_cookie_removal(config: &Config) -> Cookie<'static> {
    let mut cookie = flow_cookie(config, String::new());
    cookie.make_removal();
    cookie
}

/// The sign-in page with an error code (and where to go once it works).
fn to_signin(error: &str, next: &str) -> Redirect {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("error", error);
    if next != next::HOME {
        query.append_pair("next", next);
    }
    Redirect::to(&format!("/app/signin?{}", query.finish()))
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the sign-in page's switches, as JSON"
)]
pub struct AuthMethods {
    /// Single sign-on is on and configured (the OIDC client exists).
    pub sso: bool,
    /// Single sign-on is one of this server's methods (`ROMULUS_AUTH`),
    /// configured or not yet.
    pub sso_enabled: bool,
    /// Email + password sign-in is on (`ROMULUS_AUTH=password`).
    pub password: bool,
    /// Who may create a password account; `None` while password sign-in
    /// is off.
    pub sign_up: Option<crate::config::SignUpPolicy>,
    /// "Forgot password?" sends a link (password sign-in and mail are on).
    pub password_reset: bool,
    /// The dev-only sign-in exists (`GET /api/auth/dev-signin?email=…`).
    pub dev_signin: bool,
}

/// What the sign-in page can offer.
#[utoipa::path(
    get,
    path = "/auth/methods",
    operation_id = "auth_methods",
    tag = "auth",
    responses((status = 200, body = AuthMethods))
)]
async fn methods(State(state): State<AppState>) -> Json<AuthMethods> {
    let password = state.config.auth.password;
    Json(AuthMethods {
        sso: state.config.auth.oidc && state.oidc.is_configured(),
        sso_enabled: state.config.auth.oidc,
        password,
        sign_up: password.then_some(state.config.signup),
        password_reset: password && state.mailer.is_on(),
        dev_signin: state.config.dev_signin,
    })
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LoginQuery {
    /// Where to land afterwards: a path under `/app`. Anything else is
    /// ignored (the app's home instead).
    next: Option<String>,
    /// `select_account` asks the IdP which account to use even when one is
    /// already signed in there ("switch account").
    prompt: Option<String>,
    /// The address to sign in with (an invitation's): the IdP pre-fills it,
    /// or offers to create the account. Anything not shaped like an email
    /// is ignored.
    login_hint: Option<String>,
}

/// Starts signing in: redirects to the IdP's authorization endpoint (code
/// flow, PKCE S256, state, nonce).
#[utoipa::path(
    get,
    path = "/auth/login",
    operation_id = "login",
    tag = "auth",
    params(LoginQuery),
    responses(
        (status = 303, description = "To the IdP; or to `/app/signin?error=sso_unavailable` when it is unreachable"),
    )
)]
async fn login(
    State(state): State<AppState>,
    client: Option<Extension<ConnectInfo<std::net::SocketAddr>>>,
    jar: CookieJar,
    Query(query): Query<LoginQuery>,
) -> ApiResult<Response> {
    let next = next::or_home(query.next.as_deref());
    let client = client.map(|Extension(ConnectInfo(addr))| addr.ip());
    if !state.oidc.sign_ins.allow(client) {
        tracing::warn!(?client, "sign-in: too many started by this client");
        return Ok(to_signin("too_many_sign_ins", &next).into_response());
    }
    let select_account = query.prompt.as_deref() == Some("select_account");
    let login_hint = query
        .login_hint
        .as_deref()
        .map(crate::email::normalize)
        .filter(|hint| is_plausible_email(hint));
    let started = match state
        .oidc
        .start(select_account, login_hint.as_deref())
        .await
    {
        Ok(started) => started,
        Err(err) => {
            tracing::warn!(error = %Chain(&err), "sign-in: cannot start");
            return Ok(to_signin("sso_unavailable", &next).into_response());
        }
    };
    if state.oidc.sign_ins.sweep_due() {
        sqlx::query!(
            "DELETE FROM oidc_flows WHERE created_at < now() - make_interval(secs => $1)",
            FLOW_TTL.as_secs_f64(),
        )
        .execute(&state.db)
        .await?;
    }
    sqlx::query!(
        "INSERT INTO oidc_flows (state_hash, pkce_verifier, nonce) VALUES ($1, $2, $3)",
        token::hash(&started.state),
        started.pkce_verifier,
        started.nonce,
    )
    .execute(&state.db)
    .await?;
    let jar = jar.add(flow_cookie(
        &state.config,
        flow_cookie_value(&started.state, &next),
    ));
    Ok((jar, Redirect::to(started.url.as_str())).into_response())
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    /// Set by the IdP instead of `code` when sign-in did not happen.
    error: Option<String>,
}

/// The IdP's redirect back: exchanges the code, verifies the ID token,
/// upserts the user and starts a session. Requires a verified email.
#[utoipa::path(
    get,
    path = "/auth/callback",
    operation_id = "auth_callback",
    tag = "auth",
    params(CallbackQuery),
    responses(
        (status = 303, description = "Signed in: to the `next` of the sign-in, with the session cookie. Otherwise to `/app/signin?error=…`"),
    )
)]
async fn callback(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<CallbackQuery>,
) -> ApiResult<Response> {
    let jar_without_flow = jar.clone().add(flow_cookie_removal(&state.config));
    let fail = |error: &str, next: &str| {
        Ok((jar_without_flow.clone(), to_signin(error, next)).into_response())
    };

    // The state must be the one this browser was given: a callback link
    // forged elsewhere (login CSRF) has the wrong one, or none.
    let started = jar
        .get(&flow_cookie_name(&state.config))
        .and_then(|cookie| read_flow_cookie(cookie.value()));
    let Some((returned, next)) =
        started.filter(|(expected, _)| query.state.as_deref() == Some(*expected))
    else {
        tracing::info!("sign-in: callback without this browser's state");
        return fail("sign_in_expired", next::HOME);
    };
    // Checked when the sign-in started, and again: a cookie is client input.
    let next = next::or_home(Some(&next));
    let Some(flow) = sqlx::query!(
        "DELETE FROM oidc_flows WHERE state_hash = $1
         RETURNING pkce_verifier, nonce, created_at > now() - make_interval(secs => $2) AS \"fresh!\"",
        token::hash(returned),
        FLOW_TTL.as_secs_f64(),
    )
    .fetch_optional(&state.db)
    .await?
    .filter(|flow| flow.fresh) else {
        return fail("sign_in_expired", next::HOME);
    };

    if let Some(error) = query.error {
        tracing::info!(%error, "sign-in: the IdP refused");
        let code = if error == "access_denied" {
            "access_denied"
        } else {
            "sso_failed"
        };
        return fail(code, &next);
    }
    let Some(code) = query.code else {
        return fail("sso_failed", &next);
    };
    let identity = match state
        .oidc
        .finish(code, flow.pkce_verifier, &flow.nonce)
        .await
    {
        Ok(identity) => identity,
        Err(err) => {
            tracing::warn!(error = %Chain(&err), "sign-in: code exchange failed");
            return fail("sso_failed", &next);
        }
    };
    let Some(user_id) = upsert_user(&state.db, &identity).await? else {
        return fail("email_unverified", &next);
    };

    let jar = start_session(&state, jar_without_flow, user_id).await?;
    Ok((jar, Redirect::to(&next)).into_response())
}

/// The user an IdP identity is, created or refreshed from its claims, or
/// `None` when the IdP does not vouch for an email address: romulus matches
/// invitations by email, so an unverified one could claim someone else's.
pub async fn upsert_user(
    db: &PgPool,
    identity: &Identity,
) -> Result<Option<uuid::Uuid>, sqlx::Error> {
    let Some(email) = identity
        .email
        .as_deref()
        .filter(|_| identity.email_verified)
    else {
        tracing::info!(subject = %identity.subject, "sign-in: refused, email not verified");
        return Ok(None);
    };
    let email = &crate::email::normalize(email);
    let name = identity
        .name
        .clone()
        .unwrap_or_else(|| name_from_email(email));
    sqlx::query_scalar!(
        "INSERT INTO users (oidc_subject, email, email_verified, name) VALUES ($1, $2, true, $3)
         ON CONFLICT (oidc_subject) DO UPDATE
         SET email = EXCLUDED.email, email_verified = true, name = EXCLUDED.name
         RETURNING id",
        identity.subject,
        email,
        name,
    )
    .fetch_one(db)
    .await
    .map(Some)
}

/// A new session for `user_id`, replacing whichever one this browser had.
async fn start_session(
    state: &AppState,
    jar: CookieJar,
    user_id: uuid::Uuid,
) -> ApiResult<CookieJar> {
    if let Some(old) = jar.get(&state.config.session_cookie) {
        session::delete(&state.db, old.value()).await?;
    }
    let token = session::create(&state.db, user_id).await?;
    Ok(jar.add(session::cookie(&state.config, token)))
}

/// Something@something, no spaces or controls, at most 254 bytes: enough to
/// pass on as a hint, never trusted as an identity.
fn is_plausible_email(value: &str) -> bool {
    value.len() <= 254
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
        && value.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && !domain.is_empty() && !domain.contains('@')
        })
}

/// "alice.martin@x" → "Alice Martin": a name for users the IdP gave none.
fn name_from_email(email: &str) -> String {
    let local = email.split('@').next().unwrap_or(email);
    let words: Vec<String> = local
        .split(['.', '_', '-', '+'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect();
    if words.is_empty() {
        email.to_owned()
    } else {
        words.join(" ")
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SignedOut {
    /// Where the browser goes next: the IdP's logout (which comes back to
    /// the sign-in page), or the sign-in page itself.
    pub redirect_url: String,
}

/// Ends the session (idempotent: an expired one is fine) and clears the
/// cookie. The web app then navigates to `redirect_url` to end the IdP
/// session too.
#[utoipa::path(
    post,
    path = "/auth/logout",
    operation_id = "logout",
    tag = "auth",
    responses(
        (status = 200, body = SignedOut),
        (status = 403, description = "Missing CSRF header or foreign Origin", body = ErrorBody),
    )
)]
async fn logout(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    jar: CookieJar,
) -> ApiResult<(CookieJar, Json<SignedOut>)> {
    // A bearer request is exempt from the CSRF check, so it must not touch
    // the cookie's session either (see `crate::csrf`).
    let mut password_account = false;
    if let Some(cookie) = jar
        .get(&state.config.session_cookie)
        .filter(|_| !crate::csrf::has_bearer(&headers))
    {
        password_account = sqlx::query_scalar!(
            r#"SELECT u.oidc_subject LIKE 'password|%' AS "password!"
               FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.id_hash = $1"#,
            token::hash(cookie.value()),
        )
        .fetch_optional(&state.db)
        .await?
        .unwrap_or(false);
        session::delete(&state.db, cookie.value()).await?;
        // Its open sockets (drafts) close too.
        state.sessions.ended(token::hash(cookie.value()));
    }
    // A password account has no session at the IdP to end.
    let idp_logout = if password_account {
        None
    } else {
        state.oidc.logout_url().await
    };
    let redirect_url = match idp_logout {
        Some(url) => url.into(),
        None => state
            .config
            .public_url
            .join("/app/signin")
            .expect("a path joins any base URL")
            .into(),
    };
    Ok((
        jar.add(session::removal(&state.config)),
        Json(SignedOut { redirect_url }),
    ))
}

/// The signed-in user.
#[utoipa::path(
    get,
    path = "/me",
    operation_id = "me",
    tag = "auth",
    responses(
        (status = 200, body = CurrentUser),
        (status = 401, description = "`unauthorized`, or `session_expired` when the cookie's session is gone", body = ErrorBody),
    )
)]
async fn me(user: CurrentUser) -> Json<CurrentUser> {
    Json(user)
}

#[derive(Debug, Deserialize)]
struct DevSigninQuery {
    email: String,
    name: Option<String>,
    next: Option<String>,
    /// `false` signs in with an unverified email (to try the invitation's
    /// "verify your email" state). Absent: new users are verified, existing
    /// ones keep theirs.
    verified: Option<bool>,
}

async fn dev_signin(
    State(state): State<AppState>,
    method: Method,
    jar: CookieJar,
    query: Result<Query<DevSigninQuery>, ApiError>,
) -> ApiResult<Response> {
    if !state.config.dev_signin || method != Method::GET {
        return Ok(crate::error::not_found().await.into_response());
    }
    let Ok(Query(query)) = query else {
        return Err(ApiError::BadRequest(
            "dev sign-in: pass ?email=… (and optionally next, name, verified)".to_owned(),
        ));
    };
    let email = &crate::email::normalize(&query.email);
    if !email.contains('@') || email.len() > 254 {
        return Err(ApiError::BadRequest(format!(
            "dev sign-in: {email:?} is not an email address"
        )));
    }
    let existing = sqlx::query_scalar!(
        "SELECT id FROM users
         WHERE lower(email COLLATE pg_unicode_fast) = lower($1 COLLATE pg_unicode_fast)
         ORDER BY created_at, id LIMIT 1",
        email,
    )
    .fetch_optional(&state.db)
    .await?;
    let user_id = match existing {
        Some(id) => {
            sqlx::query!(
                "UPDATE users SET email_verified = coalesce($2, email_verified), name = coalesce($3, name)
                 WHERE id = $1",
                id,
                query.verified,
                query.name,
            )
            .execute(&state.db)
            .await?;
            id
        }
        None => {
            sqlx::query_scalar!(
            "INSERT INTO users (oidc_subject, email, email_verified, name) VALUES ($1, $2, $3, $4)
                 RETURNING id",
            format!("dev|{}", email.to_lowercase()),
            email,
            query.verified.unwrap_or(true),
            query.name.unwrap_or_else(|| name_from_email(email)),
        )
            .fetch_one(&state.db)
            .await?
        }
    };
    tracing::info!(%email, "dev sign-in");
    let jar = start_session(&state, jar, user_id).await?;
    Ok((jar, Redirect::to(&next::or_home(query.next.as_deref()))).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_the_email_when_the_idp_has_none() {
        assert_eq!(name_from_email("alice@romulus.test"), "Alice");
        assert_eq!(name_from_email("bob.durand@x.io"), "Bob Durand");
        assert_eq!(name_from_email("élodie_r@x.io"), "Élodie R");
    }

    #[test]
    fn sign_in_errors_keep_where_the_user_was_going() {
        let location = |redirect: Redirect| {
            redirect.into_response().headers()["location"]
                .to_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            location(to_signin("sso_failed", "/app/invite/abc")),
            "/app/signin?error=sso_failed&next=%2Fapp%2Finvite%2Fabc"
        );
        assert_eq!(
            location(to_signin("sign_in_expired", next::HOME)),
            "/app/signin?error=sign_in_expired"
        );
    }
}
