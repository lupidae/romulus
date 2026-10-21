//! Connected apps: what a person approved, as their account settings show
//! it, and what an OAuth access token resolves to at the MCP endpoint.
//!
//! - `GET /me/connected-apps`: one entry per client with a live grant
//!   (approved, not revoked, not expired), newest use first.
//! - `DELETE /me/connected-apps/{client_id}`: Disconnect, every grant of
//!   that client for the person, at once.
//!
//! Signed in with the session cookie: a token never manages apps. A
//! password reset or change calls [`revoke_all`] (as it revokes personal
//! tokens); deleting an account deletes its grants with it. [`agent`] is
//! `access_tokens::Agent::lookup` for `rmls_oat_` tokens: the person, the
//! grant's scope, refused when the token expired, was revoked or was issued
//! for another resource.

use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{CLIENT_UNUSED, REQUEST_TTL, clients};
use crate::{
    ApiError, ApiResult, AppState,
    auth::{
        CurrentUser,
        access_tokens::{Agent, TokenKind, TokenRef},
    },
    domain::AccessTokenScope,
    error::{Chain, ErrorBody},
    extract::{Json, Path},
    mail::{Email, Letter},
    token,
};

/// An app the person connected to romulus.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConnectedApp {
    /// The client's id: what Disconnect takes.
    pub client_id: Uuid,
    /// The name it registered under, unverified; `null` if it gave none.
    pub name: Option<String>,
    /// Where its sign-ins go back to (`claude.ai`, `localhost`).
    pub redirect_host: String,
    /// Its redirect is a loopback address: a program on the person's
    /// computer.
    pub loopback: bool,
    /// The widest access among its grants.
    pub scope: AccessTokenScope,
    /// When the person first approved it.
    pub connected_at: DateTime<Utc>,
    /// Its last request (written at most once a minute), or refresh;
    /// `null` if never used.
    pub last_used_at: Option<DateTime<Utc>>,
    /// The MCP client that last said who it is (`claude-code 2.1.4`).
    pub last_client: Option<String>,
    /// When it stops working unless it is used before.
    pub expires_at: DateTime<Utc>,
}

/// The person's connected apps.
#[utoipa::path(
    get,
    path = "/me/connected-apps",
    operation_id = "list_connected_apps",
    tag = "oauth",
    responses(
        (status = 200, body = Vec<ConnectedApp>),
        (status = 401, body = ErrorBody),
    )
)]
pub(crate) async fn list(
    user: CurrentUser,
    State(db): State<PgPool>,
) -> ApiResult<Json<Vec<ConnectedApp>>> {
    by_the_person(&user)?;
    let rows = sqlx::query!(
        r#"SELECT c.id AS client_id, c.name, c.redirect_uris,
                  max(g.scope) AS "scope!: AccessTokenScope",
                  min(g.created_at) AS "connected_at!",
                  max(g.last_used_at) AS last_used_at,
                  (array_agg(g.last_client ORDER BY g.last_used_at DESC NULLS LAST))[1] AS last_client,
                  max(g.idle_expires_at) AS "expires_at!"
           FROM oauth_grants g JOIN oauth_clients c ON c.id = g.client_id
           WHERE g.user_id = $1 AND g.revoked_at IS NULL
             AND g.idle_expires_at > now() AND g.expires_at > now()
           GROUP BY c.id
           ORDER BY max(coalesce(g.last_used_at, g.created_at)) DESC, c.id"#,
        user.id,
    )
    .fetch_all(&db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|row| {
                let uri = row.redirect_uris.first().map_or("", String::as_str);
                ConnectedApp {
                    client_id: row.client_id,
                    name: row.name,
                    redirect_host: clients::host_of(uri),
                    loopback: clients::is_loopback_uri(uri),
                    scope: row.scope,
                    connected_at: row.connected_at,
                    last_used_at: row.last_used_at,
                    last_client: row.last_client,
                    expires_at: row.expires_at,
                }
            })
            .collect(),
    ))
}

/// Disconnects an app: every token it holds for the person stops working
/// at once.
#[utoipa::path(
    delete,
    path = "/me/connected-apps/{client_id}",
    operation_id = "disconnect_app",
    tag = "oauth",
    params(("client_id" = Uuid, Path, description = "The app's client id")),
    responses(
        (status = 204, description = "Disconnected"),
        (status = 401, body = ErrorBody),
        (status = 404, description = "No such app connected for the caller", body = ErrorBody),
    )
)]
pub(crate) async fn disconnect(
    user: CurrentUser,
    State(db): State<PgPool>,
    Path(client_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    by_the_person(&user)?;
    let revoked = sqlx::query!(
        "UPDATE oauth_grants SET revoked_at = now()
         WHERE user_id = $1 AND client_id = $2 AND revoked_at IS NULL",
        user.id,
        client_id,
    )
    .execute(&db)
    .await?
    .rows_affected();
    if revoked == 0 {
        return Err(ApiError::not_found(
            "No such app is connected. It may already be disconnected.",
        ));
    }
    tracing::info!(user = %user.id, client = %client_id, "oauth: app disconnected");
    Ok(StatusCode::NO_CONTENT)
}

/// Apps are managed by the person, signed in: not by an agent.
fn by_the_person(user: &CurrentUser) -> ApiResult<()> {
    if user.via.is_some() {
        return Err(ApiError::Denied {
            code: "token_not_allowed",
            message: "Connected apps are managed in romulus, signed in: not with a token.".into(),
        });
    }
    Ok(())
}

/// Revokes `grant` (and so every token of it); whether it was live.
pub async fn revoke_grant(db: &mut PgConnection, grant: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query!(
        "UPDATE oauth_grants SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
        grant
    )
    .execute(db)
    .await
    .map(|done| done.rows_affected() > 0)
}

/// Disconnects every app of `user_id` (a password reset or change signs
/// them out everywhere); answers how many grants it revoked.
pub async fn revoke_all(db: &mut PgConnection, user_id: Uuid) -> Result<u64, sqlx::Error> {
    sqlx::query!(
        "UPDATE oauth_grants SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
        user_id,
    )
    .execute(db)
    .await
    .map(|done| done.rows_affected())
}

const NOT_VALID: &str = "This access token is not valid: the app may have been disconnected. \
                         Connect it again from your agent.";

/// The live OAuth access token `secret` is, as the agent it lets in:
/// 401 `invalid_token` when it is unknown, revoked, expired, its grant
/// over, or issued for another resource. Its use is not written
/// ([`touch`]).
pub async fn agent(state: &AppState, secret: &str) -> Result<Agent, ApiError> {
    let row = sqlx::query!(
        r#"SELECT a.expires_at <= now() AS "expired!",
                  g.id AS grant_id, g.scope AS "scope: AccessTokenScope", g.resource,
                  c.name AS client_name,
                  u.id AS user_id, u.email, u.email_verified, u.name AS user_name,
                  u.oidc_subject LIKE 'password|%' AS "password_account!"
           FROM oauth_access_tokens a
           JOIN oauth_grants g ON g.id = a.grant_id
           JOIN oauth_clients c ON c.id = g.client_id
           JOIN users u ON u.id = g.user_id
           WHERE a.token_hash = $1 AND a.revoked_at IS NULL AND g.revoked_at IS NULL"#,
        token::hash(secret),
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::InvalidToken(NOT_VALID.into()))?;
    if row.expired {
        return Err(ApiError::InvalidToken(
            "This access token expired: refresh it.".into(),
        ));
    }
    if row.resource != super::resource(&state.config) {
        return Err(ApiError::InvalidToken(
            "This access token was issued for another resource: connect again.".into(),
        ));
    }
    let token = TokenRef {
        id: row.grant_id,
        name: row.client_name.unwrap_or_else(|| UNNAMED.to_owned()),
        scope: row.scope,
        kind: TokenKind::App,
    };
    Ok(Agent {
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

/// What an app that gave no name is called.
pub const UNNAMED: &str = "An unnamed app";

/// Records a grant's use, at most once a minute: whether it wrote (the
/// caller then counts the person's account as used).
pub async fn touch(db: &PgPool, grant: Uuid) -> Result<bool, sqlx::Error> {
    let touched = sqlx::query!(
        "UPDATE oauth_grants SET last_used_at = now()
         WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
        grant
    )
    .execute(db)
    .await?
    .rows_affected();
    Ok(touched > 0)
}

/// Notes which client uses a grant, as it says it. Best effort.
pub async fn note_client(db: &PgPool, grant: Uuid, client: &str) {
    let noted = sqlx::query!(
        "UPDATE oauth_grants SET last_client = $2
         WHERE id = $1 AND last_client IS DISTINCT FROM $2",
        grant,
        client,
    )
    .execute(db)
    .await;
    if let Err(err) = noted {
        tracing::warn!(%grant, error = %err, "cannot note a connected app's client");
    }
}

/// The person's notice of a newly connected app: which, what it may do,
/// where it sends sign-ins, and where to disconnect it.
pub fn connected_letter(
    to: &str,
    name: Option<&str>,
    redirect_uri: &str,
    scope: AccessTokenScope,
    public_url: &url::Url,
) -> Email {
    let apps = public_url
        .join("/app/settings/apps")
        .map_or_else(|_| public_url.to_string(), String::from);
    let name = name.unwrap_or(UNNAMED);
    let access = match scope {
        AccessTokenScope::Read => "read only",
        AccessTokenScope::Write => "read and write, as you",
    };
    let host = clients::host_of(redirect_uri);
    let place = if clients::is_loopback_uri(redirect_uri) {
        format!("a program on the computer it was approved from ({host})")
    } else {
        host
    };
    Letter {
        kind: "app_connected",
        to,
        subject: "A new app is connected to your romulus account".to_owned(),
        paragraphs: vec![
            format!(
                "\"{name}\" was just connected to your romulus account ({access}). It is an AI agent's access: it works in romulus as you until you disconnect it."
            ),
            format!("It was approved in your browser, for {place}."),
            "If it was not you, disconnect it now from your connected apps, and change your password.".to_owned(),
        ],
        action: Some(("Your connected apps", &apps)),
        footer: format!("Sent to {to} because an app was connected to its romulus account."),
    }
    .email()
}

/// What a sweep deleted.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Swept {
    pub requests: u64,
    pub codes: u64,
    pub tokens: u64,
    pub grants: u64,
    pub clients: u64,
}

/// Deletes what can no longer be used: requests nobody decided, codes
/// after a day, tokens a day past their expiry, grants over for 30 days,
/// and clients unused for [`CLIENT_UNUSED`] with no grant left.
pub async fn sweep(db: &PgPool) -> Result<Swept, sqlx::Error> {
    let requests = sqlx::query!(
        "DELETE FROM oauth_requests WHERE created_at < now() - make_interval(secs => $1)",
        REQUEST_TTL.as_secs_f64(),
    )
    .execute(db)
    .await?
    .rows_affected();
    let codes = sqlx::query!("DELETE FROM oauth_codes WHERE created_at < now() - interval '1 day'")
        .execute(db)
        .await?
        .rows_affected();
    let access =
        sqlx::query!("DELETE FROM oauth_access_tokens WHERE expires_at < now() - interval '1 day'")
            .execute(db)
            .await?
            .rows_affected();
    let refresh = sqlx::query!(
        "DELETE FROM oauth_refresh_tokens WHERE expires_at < now() - interval '1 day'"
    )
    .execute(db)
    .await?
    .rows_affected();
    let grants = sqlx::query!(
        "DELETE FROM oauth_grants
         WHERE coalesce(revoked_at, least(expires_at, idle_expires_at)) < now() - interval '30 days'"
    )
    .execute(db)
    .await?
    .rows_affected();
    let clients = sqlx::query!(
        "DELETE FROM oauth_clients c
         WHERE coalesce(c.last_used_at, c.created_at) < now() - make_interval(secs => $1)
           AND NOT EXISTS (SELECT 1 FROM oauth_grants g WHERE g.client_id = c.id)",
        CLIENT_UNUSED.as_secs_f64(),
    )
    .execute(db)
    .await?
    .rows_affected();
    Ok(Swept {
        requests,
        codes,
        tokens: access + refresh,
        grants,
        clients,
    })
}

/// [`sweep`] now, then every `every`.
pub fn sweeper(db: PgPool, every: std::time::Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(every);
        loop {
            ticks.tick().await;
            match sweep(&db).await {
                Ok(swept) if swept == Swept::default() => {}
                Ok(swept) => tracing::info!(?swept, "oauth: swept"),
                Err(err) => tracing::warn!(error = %Chain(&err), "oauth: sweep failed"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notice_names_the_app_its_access_and_where_it_signs_in() {
        let url: url::Url = "https://romulus.example".parse().unwrap();
        let email = connected_letter(
            "alice@x.test",
            Some("Claude"),
            "https://claude.ai/api/mcp/auth_callback",
            AccessTokenScope::Write,
            &url,
        );
        assert!(
            email.text.contains("\"Claude\" was just connected"),
            "{}",
            email.text
        );
        assert!(email.text.contains("read and write"));
        assert!(email.text.contains("for claude.ai."));
        assert!(
            email
                .text
                .contains("https://romulus.example/app/settings/apps")
        );
        let local = connected_letter(
            "alice@x.test",
            None,
            "http://localhost:3118/callback",
            AccessTokenScope::Read,
            &url,
        );
        assert!(local.text.contains(UNNAMED));
        assert!(local.text.contains("a program on the computer"));
    }
}
