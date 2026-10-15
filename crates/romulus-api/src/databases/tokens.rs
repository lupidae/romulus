//! API tokens: how remus and CI push versions without a person signed in.
//!
//! A token belongs to one database and has a scope: `push` creates versions,
//! `full` also reads them back over the machine API. Developers and admins
//! create, list and revoke them (`/orgs/{org}/databases/{db}/tokens`). A
//! token is `rom_` + [`crate::token::generate`]; it is shown once, at
//! creation, and only its sha256 is stored. Revoking keeps the row (versions
//! keep their author); a deleted database's tokens are revoked with it.
//!
//! The machine API (`/api/databases/{database_id}/…`) authenticates with
//! [`Bearer`]: `Authorization: Bearer rom_…`, never a cookie.

use std::marker::PhantomData;

use axum::{
    extract::{FromRequestParts, RawPathParams, State},
    http::{StatusCode, header, request::Parts},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::find;
use crate::{
    ApiError, ApiResult, AppState,
    auth::{Member, min},
    domain::TokenScope,
    error::ErrorBody,
    extract::{Json, Path},
    orgs::invalid,
    token,
};

/// Every token starts with it: recognisable in a CI secret, and by secret
/// scanners.
pub const PREFIX: &str = "rom_";

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(revoke))
}

/// An API token, without its secret.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiToken {
    pub id: Uuid,
    /// What it is for: "GitHub Actions", "remus on prod".
    pub name: String,
    pub scope: TokenScope,
    pub created_at: DateTime<Utc>,
    /// Who created it; `null` once their account is gone.
    pub created_by: Option<String>,
    /// The last request it authenticated; `null` if never used.
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateToken {
    /// 1 to 100 characters.
    pub name: String,
    pub scope: TokenScope,
}

/// A token just created: the only time its secret is shown.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreatedToken {
    pub token: ApiToken,
    /// `rom_…`, for `Authorization: Bearer`. Not stored: copy it now.
    pub secret: String,
}

/// Whether `value` has the shape of a token this module hands out.
pub fn is_well_formed(value: &str) -> bool {
    value.strip_prefix(PREFIX).is_some_and(token::is_well_formed)
}

/// The database's live tokens, newest first.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/tokens",
    operation_id = "list_api_tokens",
    tag = "databases",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
    ),
    responses(
        (status = 200, body = Vec<ApiToken>),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
    )
)]
async fn list(
    member: Member<min::Developer>,
    State(db): State<PgPool>,
    Path((_, slug)): Path<(String, String)>,
) -> ApiResult<Json<Vec<ApiToken>>> {
    let database = find(&db, member.org.id, &slug).await?;
    let tokens = sqlx::query_as!(
        ApiToken,
        r#"SELECT t.id, t.name, t.scope AS "scope: TokenScope", t.created_at,
                  u.name AS "created_by?", t.last_used_at
           FROM api_tokens t LEFT JOIN users u ON u.id = t.created_by
           WHERE t.database_id = $1 AND t.revoked_at IS NULL
           ORDER BY t.created_at DESC"#,
        database.id,
    )
    .fetch_all(&db)
    .await?;
    Ok(Json(tokens))
}

/// Creates a token and answers its secret, once.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/tokens",
    operation_id = "create_api_token",
    tag = "databases",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
    ),
    request_body = CreateToken,
    responses(
        (status = 201, body = CreatedToken),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 422, body = ErrorBody),
    )
)]
async fn create(
    member: Member<min::Developer>,
    State(db): State<PgPool>,
    Path((_, slug)): Path<(String, String)>,
    Json(body): Json<CreateToken>,
) -> ApiResult<(StatusCode, Json<CreatedToken>)> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(invalid("name", "Name the token after where it is used."));
    }
    if name.chars().count() > 100 {
        return Err(invalid("name", "Keep the name to 100 characters or fewer."));
    }
    let database = find(&db, member.org.id, &slug).await?;
    let secret = format!("{PREFIX}{}", token::generate());
    let row = sqlx::query!(
        "INSERT INTO api_tokens (database_id, name, token_hash, scope, created_by)
         VALUES ($1, $2, $3, $4, $5) RETURNING id, created_at",
        database.id,
        name,
        token::hash(&secret),
        body.scope as TokenScope,
        member.user.id,
    )
    .fetch_one(&db)
    .await?;
    tracing::info!(org = %member.org.slug, database = %slug, scope = ?body.scope, "API token created");
    Ok((
        StatusCode::CREATED,
        Json(CreatedToken {
            token: ApiToken {
                id: row.id,
                name: name.to_owned(),
                scope: body.scope,
                created_at: row.created_at,
                created_by: Some(member.user.name),
                last_used_at: None,
            },
            secret,
        }),
    ))
}

/// Revokes a token: it stops working at once.
#[utoipa::path(
    delete,
    path = "/orgs/{org}/databases/{db}/tokens/{id}",
    operation_id = "revoke_api_token",
    tag = "databases",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        ("id" = Uuid, Path, description = "The token's id"),
    ),
    responses(
        (status = 204, description = "Revoked"),
        (status = 403, body = ErrorBody),
        (status = 404, description = "No such live token", body = ErrorBody),
    )
)]
async fn revoke(
    member: Member<min::Developer>,
    State(db): State<PgPool>,
    Path((_, slug, id)): Path<(String, String, Uuid)>,
) -> ApiResult<StatusCode> {
    let database = find(&db, member.org.id, &slug).await?;
    let revoked = sqlx::query!(
        "UPDATE api_tokens SET revoked_at = now()
         WHERE id = $1 AND database_id = $2 AND revoked_at IS NULL",
        id,
        database.id,
    )
    .execute(&db)
    .await?
    .rows_affected();
    if revoked == 0 {
        return Err(ApiError::not_found("No such token. It may already be revoked."));
    }
    tracing::info!(org = %member.org.slug, database = %slug, token = %id, "API token revoked");
    Ok(StatusCode::NO_CONTENT)
}

/// The scopes, as types: `Bearer<scope::Full>`.
pub mod scope {
    use crate::domain::TokenScope;

    /// What an endpoint of the machine API requires of a token.
    pub trait Required: Send + Sync + 'static {
        fn allows(scope: TokenScope) -> bool;
        /// The 403's message.
        const DENIED: &'static str;
    }

    /// Any token of the database: pushing versions.
    #[derive(Debug, Clone, Copy)]
    pub struct Push;
    /// Tokens with the `full` scope: reading versions back.
    #[derive(Debug, Clone, Copy)]
    pub struct Full;

    impl Required for Push {
        fn allows(_: TokenScope) -> bool {
            true
        }
        const DENIED: &'static str = "";
    }

    impl Required for Full {
        fn allows(scope: TokenScope) -> bool {
            scope == TokenScope::Full
        }
        const DENIED: &'static str = "This token can only push versions. Create a token with the full scope to read them.";
    }
}

/// A machine API request, authenticated by an API token of the
/// `{database_id}` in its path (the segment must be named so) with at least
/// scope `S`. Answers 401 `invalid_token` without a valid token, 403
/// `wrong_database` for another database's token, and 403
/// `insufficient_scope` below `S`. Records the token's `last_used_at`.
#[derive(Debug, Clone)]
pub struct Bearer<S: scope::Required = scope::Push> {
    pub token_id: Uuid,
    pub token_name: String,
    pub scope: TokenScope,
    pub database_id: Uuid,
    pub database_slug: String,
    required: PhantomData<S>,
}

const NO_TOKEN: &str = "Authenticate with an API token: `Authorization: Bearer rom_…`. \
                        Create one in the database's settings.";
const BAD_TOKEN: &str = "This API token is not valid: it may have been revoked, or its database \
                         deleted. Create a new one in the database's settings.";

impl<S: scope::Required> FromRequestParts<AppState> for Bearer<S> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
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
        let token = sqlx::query!(
            r#"SELECT t.id, t.name, t.scope AS "scope: TokenScope", d.id AS database_id, d.slug
               FROM api_tokens t JOIN databases d ON d.id = t.database_id
               WHERE t.token_hash = $1 AND t.revoked_at IS NULL AND d.deleted_at IS NULL"#,
            token::hash(secret),
        )
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::InvalidToken(BAD_TOKEN.into()))?;

        let params = RawPathParams::from_request_parts(parts, state)
            .await
            .map_err(|rejection| ApiError::BadRequest(rejection.body_text()))?;
        let path_id = params
            .iter()
            .find_map(|(name, value)| (name == "database_id").then_some(value))
            .ok_or_else(|| {
                ApiError::internal(std::io::Error::other(
                    "Bearer is extracted on a route without a {database_id} segment",
                ))
            })?;
        if Uuid::parse_str(path_id).ok() != Some(token.database_id) {
            return Err(ApiError::Denied {
                code: "wrong_database",
                message: format!(
                    "This token belongs to another database: push to /api/databases/{}/versions.",
                    token.database_id
                ),
            });
        }
        if !S::allows(token.scope) {
            return Err(ApiError::Denied {
                code: "insufficient_scope",
                message: S::DENIED.to_owned(),
            });
        }

        sqlx::query!(
            "UPDATE api_tokens SET last_used_at = now() WHERE id = $1",
            token.id
        )
        .execute(&state.db)
        .await?;
        Ok(Self {
            token_id: token.id,
            token_name: token.name,
            scope: token.scope,
            database_id: token.database_id,
            database_slug: token.slug,
            required: PhantomData,
        })
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
        assert!(!is_well_formed("rom_short"));
    }
}
