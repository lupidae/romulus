//! Server-side sessions and the [`CurrentUser`] extractor.
//!
//! The cookie holds a random token ([`crate::token`]); the `sessions` row
//! holds its sha256. Expiry slides: every use pushes `expires_at` to
//! [`IDLE`] from now, never past [`ABSOLUTE`] after sign-in, so an unused
//! session dies within a week and none outlives a month (after which the IdP
//! vouches for the user again).

use std::time::Duration;

use axum::{
    extract::{FromRequestParts, OptionalFromRequestParts},
    http::request::Parts,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use super::access_tokens::{self, Delegated, TokenRef};
use crate::{ApiError, AppState, Config, csrf, token};

/// A session nobody uses for this long is over.
pub const IDLE: Duration = Duration::from_hours(7 * 24);
/// No session lasts longer than this after sign-in, used or not.
pub const ABSOLUTE: Duration = Duration::from_hours(30 * 24);
/// `last_seen_at` (and the sliding expiry) is written at most this often, so
/// a busy page does not turn every read into a write.
const TOUCH_EVERY: Duration = Duration::from_mins(5);

/// The signed-in user, from the session cookie. Every endpoint that needs a
/// person takes it (or [`super::Member`], which includes it).
///
/// Rejects with 401 `unauthorized` without a cookie, 401 `session_expired`
/// with a cookie whose session is gone, and 401 `unauthorized` for any
/// request with `Authorization: Bearer`: API tokens are not people, and a
/// bearer request never falls back to the cookie (see `crate::csrf`).
///
/// The one other way to be someone: a request the MCP endpoint makes for a
/// personal access token's person (`access_tokens::Delegated`, a request
/// extension only server code sets). Then `via` is the token, and a `read`
/// token's identity refuses every unsafe method (403 `read_only_token`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CurrentUser {
    pub id: Uuid,
    pub email: String,
    /// Accepting an invitation requires it.
    pub email_verified: bool,
    pub name: String,
    /// Signs in with a password (and may change it), not single sign-on.
    pub password_account: bool,
    /// The personal access token this request came with (an agent's,
    /// through the MCP endpoint); `None` for the person's own session.
    #[serde(skip)]
    #[schema(ignore)]
    pub via: Option<TokenRef>,
}

/// Cached in the request's extensions: [`super::Member`] and a handler may
/// both ask, the database is asked once.
#[derive(Clone)]
struct Resolved(Option<CurrentUser>, bool);

impl CurrentUser {
    async fn resolve(parts: &mut Parts, state: &AppState) -> Result<Resolved, ApiError> {
        if let Some(resolved) = parts.extensions.get::<Resolved>() {
            return Ok(resolved.clone());
        }
        let resolved = if let Some(Delegated(agent)) = parts.extensions.get::<Delegated>() {
            Resolved(Some(agent.user.clone()), false)
        } else if csrf::has_bearer(&parts.headers) {
            Resolved(None, false)
        } else {
            match CookieJar::from_headers(&parts.headers).get(&state.config.session_cookie) {
                None => Resolved(None, false),
                Some(cookie) => Resolved(lookup(&state.db, cookie.value()).await?, true),
            }
        };
        parts.extensions.insert(resolved.clone());
        Ok(resolved)
    }
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        match Self::resolve(parts, state).await? {
            Resolved(Some(user), _) => within_scope(user, parts),
            Resolved(None, true) => Err(ApiError::SessionExpired),
            Resolved(None, false) => Err(ApiError::Unauthorized),
        }
    }
}

/// `Option<CurrentUser>`: for pages that work signed out too.
impl OptionalFromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<Self>, ApiError> {
        Self::resolve(parts, state)
            .await?
            .0
            .map(|user| within_scope(user, parts))
            .transpose()
    }
}

/// A read-only token's person never makes an unsafe request.
fn within_scope(user: CurrentUser, parts: &Parts) -> Result<CurrentUser, ApiError> {
    match &user.via {
        Some(token)
            if !token.can_write()
                && !parts.method.is_safe()
                && parts.extensions.get::<access_tokens::Reading>().is_none() =>
        {
            Err(access_tokens::read_only())
        }
        _ => Ok(user),
    }
}

/// The user behind a live session token, sliding its expiry.
async fn lookup(db: &PgPool, token: &str) -> Result<Option<CurrentUser>, ApiError> {
    if !token::is_well_formed(token) {
        return Ok(None);
    }
    let id_hash = token::hash(token);
    let Some(row) = sqlx::query!(
        r#"SELECT s.last_seen_at, u.id, u.email, u.email_verified, u.name,
                  u.oidc_subject LIKE 'password|%' AS "password_account!"
           FROM sessions s JOIN users u ON u.id = s.user_id
           WHERE s.id_hash = $1 AND s.expires_at > now()"#,
        id_hash,
    )
    .fetch_optional(db)
    .await?
    else {
        return Ok(None);
    };
    if is_stale(row.last_seen_at, Utc::now()) {
        sqlx::query!(
            "UPDATE sessions
             SET last_seen_at = now(),
                 expires_at = least(now() + make_interval(secs => $2), created_at + make_interval(secs => $3))
             WHERE id_hash = $1",
            id_hash,
            IDLE.as_secs_f64(),
            ABSOLUTE.as_secs_f64(),
        )
        .execute(db)
        .await?;
    }
    Ok(Some(CurrentUser {
        id: row.id,
        email: row.email,
        email_verified: row.email_verified,
        name: row.name,
        password_account: row.password_account,
        via: None,
    }))
}

fn is_stale(last_seen_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    (now - last_seen_at)
        .to_std()
        .is_ok_and(|age| age >= TOUCH_EVERY)
}

/// Starts a session for `user_id` and returns its token (for the cookie).
/// Also sweeps sessions that already expired, so the table does not grow.
pub async fn create(db: &PgPool, user_id: Uuid) -> Result<String, sqlx::Error> {
    let token = token::generate();
    sqlx::query!(
        "INSERT INTO sessions (id_hash, user_id, expires_at)
         VALUES ($1, $2, now() + make_interval(secs => $3))",
        token::hash(&token),
        user_id,
        IDLE.as_secs_f64(),
    )
    .execute(db)
    .await?;
    sqlx::query!("DELETE FROM sessions WHERE expires_at <= now()")
        .execute(db)
        .await?;
    Ok(token)
}

/// Whether the session with this `id_hash` (`token::hash`) is still live:
/// for connections that outlive the request that opened them.
pub async fn is_live(db: &PgPool, id_hash: &[u8]) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM sessions WHERE id_hash = $1 AND expires_at > now()) AS "live!""#,
        id_hash,
    )
    .fetch_one(db)
    .await
}

/// Ends the session `token` names, if it exists.
pub async fn delete(db: impl PgExecutor<'_>, token: &str) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "DELETE FROM sessions WHERE id_hash = $1",
        token::hash(token)
    )
    .execute(db)
    .await
    .map(drop)
}

/// The session cookie: httpOnly (scripts never see it), `SameSite=Lax`,
/// `Secure` whenever the public URL is https, sent to `/api` only.
pub fn cookie(config: &Config, token: String) -> Cookie<'static> {
    Cookie::build((config.session_cookie.clone(), token))
        .http_only(true)
        .secure(config.public_url.scheme() == "https")
        .same_site(SameSite::Lax)
        .path("/api")
        .max_age(cookie_duration(ABSOLUTE))
        .build()
}

/// Matches [`cookie`] so the browser drops it.
pub fn removal(config: &Config) -> Cookie<'static> {
    let mut cookie = cookie(config, String::new());
    cookie.make_removal();
    cookie
}

pub(super) fn cookie_duration(duration: Duration) -> cookie::time::Duration {
    cookie::time::Duration::seconds(i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_are_touched_at_most_every_five_minutes() {
        let now = Utc::now();
        assert!(!is_stale(now - chrono::Duration::minutes(4), now));
        assert!(is_stale(now - chrono::Duration::minutes(5), now));
        // A clock that moved backwards does not count as stale.
        assert!(!is_stale(now + chrono::Duration::minutes(1), now));
    }

    #[test]
    fn the_cookie_is_http_only_lax_and_scoped_to_the_api() {
        let config = Config::for_tests();
        let cookie = cookie(&config, "t".into());
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.same_site(), Some(SameSite::Lax));
        assert_eq!(cookie.path(), Some("/api"));
        assert_eq!(cookie.secure(), Some(false));

        let mut https = Config::for_tests();
        https.public_url = "https://romulus.example".parse().unwrap();
        assert_eq!(super::cookie(&https, "t".into()).secure(), Some(true));
    }
}
