//! Personal access tokens: how a person's AI agent (an MCP client) calls
//! romulus as them, at the MCP endpoint (`crate::mcp`).
//!
//! Endpoints (signed in with the session cookie; a token never manages
//! tokens):
//! - `GET /me/access-tokens`: the caller's tokens not revoked, newest first,
//!   with when each was last used and by which client.
//! - `POST /me/access-tokens`: a new one (a name, `read` or `write`, an
//!   expiry of 30, 90 (the default) or 365 days or none), its secret
//!   answered once; the person is emailed (when mail is configured), so a
//!   token made behind their back does not go unseen.
//! - `DELETE /me/access-tokens/{id}`: revokes it, at once.
//!
//! A password reset or change (`auth::password`) revokes them all
//! ([`revoke_all`]): it signs the person out everywhere, agents included.
//!
//! A token is [`PREFIX`] + [`crate::token::generate`], recognisable by
//! secret scanners; only its sha256 is stored. It is accepted at `/api/mcp`
//! only ([`Agent`]), and it acts as its person: every MCP tool calls the
//! handlers the web app calls, as that person ([`Delegated`]), so roles,
//! validation, rate limits and the audit log are the web app's. A `read`
//! token never writes: the MCP endpoint refuses its write tools, and the
//! identity it lends refuses every unsafe method.
//!
//! Table: `personal_access_tokens`.

use axum::{
    extract::{FromRequestParts, State},
    http::{StatusCode, header, request::Parts},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::CurrentUser;
use crate::{
    ApiError, ApiResult, AppState,
    domain::AccessTokenScope,
    error::ErrorBody,
    extract::{Json, Path},
    mail::{Email, Letter},
    orgs::invalid,
    token,
};

/// Every personal access token starts with it: recognisable in a config
/// file, and by secret scanners.
pub const PREFIX: &str = "rmls_pat_";

/// Live tokens a person may have at once.
pub const MAX_LIVE: i64 = 50;

/// The expiries a token may be given, in days (`null`: none).
pub const EXPIRIES: [u16; 3] = [30, 90, 365];

/// A token's expiry when none is said.
pub const DEFAULT_EXPIRY: u16 = 90;

#[expect(
    clippy::unnecessary_wraps,
    reason = "serde's default for an optional field"
)]
fn default_expiry() -> Option<u16> {
    Some(DEFAULT_EXPIRY)
}

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(revoke))
}

/// A personal access token, without its secret.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccessToken {
    pub id: Uuid,
    /// Where it is used: "Claude Code on my laptop".
    pub name: String,
    pub scope: AccessTokenScope,
    pub created_at: DateTime<Utc>,
    /// `null`: it never expires (until revoked).
    pub expires_at: Option<DateTime<Utc>>,
    /// Past `expires_at`: refused, kept in the list until revoked.
    pub expired: bool,
    /// The last request it authenticated (written at most once a minute);
    /// `null` if never used.
    pub last_used_at: Option<DateTime<Utc>>,
    /// The MCP client that last said who it is with it (`claude-code
    /// 2.1.4`), as it said it.
    pub last_client: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateAccessToken {
    /// 1 to 100 characters.
    pub name: String,
    pub scope: AccessTokenScope,
    /// 30, 90 or 365; absent: 90; `null`: it never expires.
    #[serde(default = "default_expiry")]
    pub expires_in_days: Option<u16>,
}

/// A token just created: the only time its secret is shown.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreatedAccessToken {
    pub token: AccessToken,
    /// `rmls_pat_…`, for `Authorization: Bearer`. Not stored: copy it now.
    pub secret: String,
}

/// Whether `value` has the shape of a token this module hands out.
pub fn is_well_formed(value: &str) -> bool {
    value
        .strip_prefix(PREFIX)
        .is_some_and(token::is_well_formed)
}

/// Tokens are managed by the person, signed in: an agent's token cannot
/// mint or revoke tokens, whatever its scope.
fn by_the_person(user: &CurrentUser) -> ApiResult<()> {
    if user.via.is_some() {
        return Err(ApiError::Denied {
            code: "token_not_allowed",
            message: "Access tokens are managed in romulus, signed in: not with a token.".into(),
        });
    }
    Ok(())
}

/// The caller's tokens not revoked, newest first.
#[utoipa::path(
    get,
    path = "/me/access-tokens",
    operation_id = "list_access_tokens",
    tag = "auth",
    responses(
        (status = 200, body = Vec<AccessToken>),
        (status = 401, body = ErrorBody),
    )
)]
async fn list(user: CurrentUser, State(db): State<PgPool>) -> ApiResult<Json<Vec<AccessToken>>> {
    by_the_person(&user)?;
    let tokens = sqlx::query_as!(
        AccessToken,
        r#"SELECT id, name, scope AS "scope: AccessTokenScope", created_at, expires_at,
                  coalesce(expires_at <= now(), false) AS "expired!", last_used_at, last_client
           FROM personal_access_tokens
           WHERE user_id = $1 AND revoked_at IS NULL
           ORDER BY created_at DESC, id DESC"#,
        user.id,
    )
    .fetch_all(&db)
    .await?;
    Ok(Json(tokens))
}

/// Creates a token and answers its secret, once.
#[utoipa::path(
    post,
    path = "/me/access-tokens",
    operation_id = "create_access_token",
    tag = "auth",
    request_body = CreateAccessToken,
    responses(
        (status = 201, body = CreatedAccessToken),
        (status = 401, body = ErrorBody),
        (status = 409, description = "`too_many_tokens`: revoke one first", body = ErrorBody),
        (status = 422, body = ErrorBody),
    )
)]
async fn create(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(body): Json<CreateAccessToken>,
) -> ApiResult<(StatusCode, Json<CreatedAccessToken>)> {
    let db = &state.db;
    by_the_person(&user)?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(invalid(
            "name",
            "Name the token after where it is used: \"Claude Code on my laptop\".",
        ));
    }
    if name.chars().count() > 100 {
        return Err(invalid("name", "Keep the name to 100 characters or fewer."));
    }
    if body
        .expires_in_days
        .is_some_and(|days| !EXPIRIES.contains(&days))
    {
        return Err(invalid(
            "expires_in_days",
            "A token expires after 30, 90 or 365 days, or never.",
        ));
    }
    let mut tx = db.begin().await?;
    // One person's tokens are created one at a time: the count holds.
    sqlx::query!("SELECT id FROM users WHERE id = $1 FOR UPDATE", user.id)
        .fetch_one(&mut *tx)
        .await?;
    let live = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM personal_access_tokens
           WHERE user_id = $1 AND revoked_at IS NULL"#,
        user.id,
    )
    .fetch_one(&mut *tx)
    .await?;
    if live >= MAX_LIVE {
        return Err(ApiError::Conflict {
            code: "too_many_tokens",
            message: format!(
                "You have {MAX_LIVE} access tokens: revoke one you no longer use first."
            ),
        });
    }
    let secret = format!("{PREFIX}{}", token::generate());
    let row = sqlx::query!(
        "INSERT INTO personal_access_tokens (user_id, name, token_hash, scope, expires_at)
         VALUES ($1, $2, $3, $4, now() + make_interval(days => $5))
         RETURNING id, created_at, expires_at",
        user.id,
        name,
        token::hash(&secret),
        body.scope as AccessTokenScope,
        body.expires_in_days.map(i32::from),
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, token = %row.id, scope = ?body.scope, "access token created");
    state.mailer.send(created_letter(
        &user.email,
        name,
        body.scope,
        row.expires_at,
        &state.config.public_url,
    ));
    Ok((
        StatusCode::CREATED,
        Json(CreatedAccessToken {
            token: AccessToken {
                id: row.id,
                name: name.to_owned(),
                scope: body.scope,
                created_at: row.created_at,
                expires_at: row.expires_at,
                expired: false,
                last_used_at: None,
                last_client: None,
            },
            secret,
        }),
    ))
}

/// The person's notice of a new token: which, what it may do, until when,
/// and where to revoke it.
fn created_letter(
    to: &str,
    name: &str,
    scope: AccessTokenScope,
    expires_at: Option<DateTime<Utc>>,
    public_url: &url::Url,
) -> Email {
    let tokens = public_url
        .join("/app/settings/tokens")
        .map_or_else(|_| public_url.to_string(), String::from);
    let access = match scope {
        AccessTokenScope::Read => "read only",
        AccessTokenScope::Write => "read and write, as you",
    };
    let until = expires_at.map_or_else(
        || "It never expires: revoke it when you no longer use it.".to_owned(),
        |at| format!("It expires on {}.", at.format("%B %-d, %Y")),
    );
    Letter {
        kind: "access_token_created",
        to,
        subject: "A new access token for your romulus account".to_owned(),
        paragraphs: vec![
            format!(
                "An access token named \"{name}\" was just created for your account ({access}). An AI agent can use it to work in romulus as you."
            ),
            until,
            "If you did not create it, revoke it now from your access tokens, and change your password.".to_owned(),
        ],
        action: Some(("Your access tokens", &tokens)),
        footer: format!("Sent to {to} because an access token was created for its romulus account."),
    }
    .email()
}

/// Revokes every live token of `user_id` (a password reset or change signs
/// them out everywhere); answers how many.
pub async fn revoke_all(db: &mut sqlx::PgConnection, user_id: Uuid) -> Result<u64, sqlx::Error> {
    sqlx::query!(
        "UPDATE personal_access_tokens SET revoked_at = now()
         WHERE user_id = $1 AND revoked_at IS NULL",
        user_id,
    )
    .execute(db)
    .await
    .map(|done| done.rows_affected())
}

/// Revokes a token: it stops working at once.
#[utoipa::path(
    delete,
    path = "/me/access-tokens/{id}",
    operation_id = "revoke_access_token",
    tag = "auth",
    params(("id" = Uuid, Path, description = "The token's id")),
    responses(
        (status = 204, description = "Revoked"),
        (status = 401, body = ErrorBody),
        (status = 404, description = "No such live token of the caller's", body = ErrorBody),
    )
)]
async fn revoke(
    user: CurrentUser,
    State(db): State<PgPool>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    by_the_person(&user)?;
    let revoked = sqlx::query!(
        "UPDATE personal_access_tokens SET revoked_at = now()
         WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
        id,
        user.id,
    )
    .execute(&db)
    .await?
    .rows_affected();
    if revoked == 0 {
        return Err(ApiError::not_found(
            "No such token. It may already be revoked.",
        ));
    }
    tracing::info!(user = %user.id, token = %id, "access token revoked");
    Ok(StatusCode::NO_CONTENT)
}

/// The token a request came with, as the handlers it reaches see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRef {
    pub id: Uuid,
    pub name: String,
    pub scope: AccessTokenScope,
}

impl TokenRef {
    pub fn can_write(&self) -> bool {
        self.scope == AccessTokenScope::Write
    }
}

/// A request to the MCP endpoint, authenticated by a personal access token
/// (`Authorization: Bearer rmls_pat_…`), never by a cookie. Answers 401
/// `invalid_token` without a live token (unknown, revoked, expired, or its
/// person gone). Records `last_used_at` (at most once a minute).
#[derive(Debug, Clone)]
pub struct Agent {
    pub token: TokenRef,
    /// The person it acts as (`via` set to the token).
    pub user: CurrentUser,
}

pub const NO_TOKEN: &str = "Authenticate with a personal access token: \
                            `Authorization: Bearer rmls_pat_…`. Create one in romulus: \
                            your account menu, Access tokens.";
const BAD_TOKEN: &str = "This access token is not valid: it may have been revoked. \
                         Create a new one in romulus: your account menu, Access tokens.";

impl FromRequestParts<AppState> for Agent {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let secret = Self::bearer(parts)?;
        let agent = Self::lookup(state, secret).await?;
        agent.touch(&state.db).await?;
        Ok(agent)
    }
}

impl Agent {
    /// The secret of `Authorization: Bearer rmls_pat_…`, if it has a
    /// personal token's shape (nothing looked up).
    pub fn bearer(parts: &Parts) -> Result<&str, ApiError> {
        let secret = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| {
                let (scheme, rest) = value.split_at_checked(7)?;
                scheme.eq_ignore_ascii_case("bearer ").then(|| rest.trim())
            })
            .ok_or_else(|| ApiError::InvalidToken(NO_TOKEN.into()))?;
        if !is_well_formed(secret) {
            return Err(ApiError::InvalidToken(BAD_TOKEN.into()));
        }
        Ok(secret)
    }

    /// The live token `secret` is, and its person: 401 `invalid_token`
    /// when there is none (unknown, revoked, expired, its person gone).
    /// Its last use is not written ([`Self::touch`]).
    pub async fn lookup(state: &AppState, secret: &str) -> Result<Self, ApiError> {
        let row = sqlx::query!(
            r#"SELECT t.id, t.name, t.scope AS "scope: AccessTokenScope", t.expires_at,
                      coalesce(t.expires_at <= now(), false) AS "expired!",
                      u.id AS user_id, u.email, u.email_verified, u.name AS user_name,
                      u.oidc_subject LIKE 'password|%' AS "password_account!"
               FROM personal_access_tokens t JOIN users u ON u.id = t.user_id
               WHERE t.token_hash = $1 AND t.revoked_at IS NULL"#,
            token::hash(secret),
        )
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::InvalidToken(BAD_TOKEN.into()))?;
        if row.expired {
            let on = row
                .expires_at
                .map_or_else(String::new, |at| format!(" on {}", at.format("%Y-%m-%d")));
            return Err(ApiError::InvalidToken(format!(
                "This access token expired{on}. Create a new one in romulus: your account \
                 menu, Access tokens."
            )));
        }
        let token = TokenRef {
            id: row.id,
            name: row.name,
            scope: row.scope,
        };
        Ok(Self {
            user: CurrentUser {
                id: row.user_id,
                email: row.email,
                email_verified: row.email_verified,
                name: row.user_name,
                password_account: row.password_account,
                via: Some(token.clone()),
            },
            token,
        })
    }

    /// Records the token's use, at most once a minute: an agent calling in
    /// a loop writes no faster.
    pub async fn touch(&self, db: &PgPool) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE personal_access_tokens SET last_used_at = now()
             WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
            self.token.id
        )
        .execute(db)
        .await
        .map(drop)
    }
}

/// Notes which client uses the token, as it says it (`initialize`'s
/// `clientInfo`): shown in the token list. Best effort.
pub async fn note_client(db: &PgPool, token: Uuid, client: &str) {
    let client: String = client
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    let noted = sqlx::query!(
        "UPDATE personal_access_tokens SET last_client = $2
         WHERE id = $1 AND last_client IS DISTINCT FROM $2",
        token,
        client,
    )
    .execute(db)
    .await;
    if let Err(err) = noted {
        tracing::warn!(%token, error = %err, "cannot note an access token's client");
    }
}

/// The identity the MCP endpoint lends the handlers it calls for a token's
/// person: [`CurrentUser`] (and so `Member`) resolves to them, `via` the
/// token. A request extension: only server code sets it, never a client.
#[derive(Debug, Clone)]
pub struct Delegated(pub Agent);

/// Marks a request the MCP endpoint makes for a token's person as a read,
/// though its method is not safe (a playground query posts its SQL): a
/// `read` token's identity lets it through. A request extension, like
/// [`Delegated`].
#[derive(Debug, Clone, Copy)]
pub struct Reading;

/// The 403 an unsafe request gets with a `read` token's identity.
pub fn read_only() -> ApiError {
    ApiError::Denied {
        code: "read_only_token",
        message: "This access token is read-only. Create a token with write access to make \
                  changes (your account menu, Access tokens)."
            .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_carry_the_prefix() {
        let secret = format!("{PREFIX}{}", token::generate());
        assert!(is_well_formed(&secret));
        assert!(!is_well_formed(&token::generate()));
        assert!(!is_well_formed("rmls_pat_short"));
        // An API token is not a personal one.
        assert!(!is_well_formed(&format!("rom_{}", token::generate())));
    }
}
