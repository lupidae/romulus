//! The machine API: remus and CI push versions with an API token.
//!
//! ```bash
//! remus -f json | curl -fsS -H "Authorization: Bearer $ROMULUS_TOKEN" \
//!   --data-binary @- "$ROMULUS_URL/api/databases/<id>/versions"
//! ```
//!
//! The body is remus JSON or SQL. `Content-Type: application/json` is JSON;
//! `text/plain`, `application/sql` (and `text/x-sql`, `application/x-sql`)
//! are SQL. Without a meaningful type (curl's default
//! `application/x-www-form-urlencoded` for `--data-binary`, none, or
//! `application/octet-stream`) a body starting with `{` is JSON and anything
//! else SQL, so the one-liner above works as written. `?message=` gives the
//! version a message.
//!
//! JSON is stored as pushed (`source_kind = 'remus_json'`), its model made
//! canonical; SQL is compiled like an upload. Either way a model equal to the
//! latest version's (romulus-diff finds no difference) creates nothing and
//! answers `200 {"status": "unchanged", "version": <the latest>}`, so CI can
//! push on every run; otherwise `201 {"status": "created", "version": …}`.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, header},
};
use remus_core::Schema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

use super::{
    VersionDetail, VersionSummary, checked_message, compiled_model, detail,
    store::{NewVersion, Stored, store},
};
use crate::{
    ApiError, ApiResult, AppState,
    databases::tokens::{Bearer, scope},
    domain::{SourceKind, VersionOrigin},
    error::ErrorBody,
    extract::{Json, Path, Query, read_body},
    orgs::invalid,
};

/// Pushed bodies above this are refused (413). remus JSON for a few
/// thousand tables fits; SQL is held to the compiler's own, smaller limit.
pub const PUSH_BODY_LIMIT: usize = 16 * 1024 * 1024;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(push))
        .routes(routes!(read))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PushStatus {
    /// A new version was stored (201).
    Created,
    /// The model equals the latest version's; nothing was stored (200).
    Unchanged,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PushResult {
    pub status: PushStatus,
    /// The version created, or the latest one when unchanged.
    pub version: VersionSummary,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PushQuery {
    /// What changed, in a line, e.g. the commit subject.
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Json,
    Sql,
}

/// What the body is, from its `Content-Type` (or its first character when
/// the type says nothing). See the module documentation.
fn format(headers: &HeaderMap, body: &[u8]) -> ApiResult<Format> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase());
    match content_type.as_deref() {
        Some("application/json") => Ok(Format::Json),
        Some("text/plain" | "application/sql" | "text/x-sql" | "application/x-sql") => {
            Ok(Format::Sql)
        }
        None | Some("application/x-www-form-urlencoded" | "application/octet-stream") => {
            let first = body.iter().find(|b| !b.is_ascii_whitespace());
            Ok(if first == Some(&b'{') {
                Format::Json
            } else {
                Format::Sql
            })
        }
        Some(other) => Err(ApiError::UnsupportedMediaType(format!(
            "Push remus JSON (application/json) or SQL (text/plain or application/sql), not {other}."
        ))),
    }
}

/// Pushes a version: remus JSON or SQL. See the module documentation.
#[utoipa::path(
    post,
    path = "/databases/{database_id}/versions",
    operation_id = "push_version",
    tag = "push",
    params(
        ("database_id" = uuid::Uuid, Path, description = "The database's id (its settings show it)"),
        PushQuery,
    ),
    request_body(
        description = "remus JSON (`remus -f json`), or SQL DDL",
        content(
            (String = "application/json"),
            (String = "text/plain"),
            (String = "application/sql"),
        ),
    ),
    responses(
        (status = 201, description = "Stored as a new version", body = PushResult),
        (status = 200, description = "Unchanged: nothing stored", body = PushResult),
        (status = 401, description = "`invalid_token`", body = ErrorBody),
        (status = 403, description = "`wrong_database`", body = ErrorBody),
        (status = 413, description = "`payload_too_large`", body = ErrorBody),
        (status = 415, description = "`unsupported_media_type`", body = ErrorBody),
        (status = 422, description = "`invalid_schema`, `compile_failed` (`details.diagnostics`) or `validation`", body = ErrorBody),
        (status = 429, description = "`rate_limited`, with `Retry-After`", body = ErrorBody),
        (status = 503, description = "`unavailable`: SQL could not be compiled now; nothing stored", body = ErrorBody),
    )
)]
async fn push(
    token: Bearer,
    State(state): State<AppState>,
    Query(query): Query<PushQuery>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> ApiResult<(StatusCode, Json<PushResult>)> {
    // Before reading up to 16 MiB of it.
    state.limits.push.check(token.token_id)?;
    let message = checked_message(query.message.as_deref())?;
    let body = read_body(&headers, body, PUSH_BODY_LIMIT).await?;
    let format = format(&headers, &body)?;
    let Ok(mut text) = String::from_utf8(body.to_vec()) else {
        return Err(invalid("body", "The body is not UTF-8 text."));
    };
    if text.trim().is_empty() {
        return Err(invalid(
            "body",
            "The body is empty: push remus JSON (`remus -f json`) or SQL.",
        ));
    }
    let (model, source_kind) = match format {
        Format::Json => {
            // Megabytes of JSON: parsed off the async workers.
            let (model, kept) = tokio::task::spawn_blocking(move || {
                let model = Schema::from_json(&text).map(Schema::canonical);
                (model, text)
            })
            .await
            .map_err(ApiError::internal)?;
            text = kept;
            let model = model.map_err(|err| ApiError::Unprocessable {
                code: "invalid_schema",
                message: format!("This is not a remus schema: {err}."),
                details: Some(json!({ "error": err.to_string() })),
            })?;
            (model, SourceKind::RemusJson)
        }
        Format::Sql => {
            state.limits.database_compiles.check(token.database_id)?;
            (
                compiled_model(&state.compiler, &text, &token.database_slug).await?,
                SourceKind::Sql,
            )
        }
    };
    let new = NewVersion {
        database_id: token.database_id,
        model,
        source: text,
        source_kind,
        origin: VersionOrigin::Push,
        message,
        created_by: None,
        created_by_token: Some(token.token_id),
        layout: None,
        expected_base: None,
    };
    let database_id = new.database_id;
    Ok(match store(&state.db, new, true).await? {
        Stored::Created(version) => {
            // Open drafts of the database learn their base moved.
            state.collab.version_created(database_id, version.number);
            (
                StatusCode::CREATED,
                Json(PushResult {
                    status: PushStatus::Created,
                    version,
                }),
            )
        }
        Stored::Unchanged(version) => (
            StatusCode::OK,
            Json(PushResult {
                status: PushStatus::Unchanged,
                version,
            }),
        ),
    })
}

/// Reads a version back: by number, or the latest. Tokens with the `full`
/// scope only.
#[utoipa::path(
    get,
    path = "/databases/{database_id}/versions/{version}",
    operation_id = "read_version",
    tag = "push",
    params(
        ("database_id" = uuid::Uuid, Path, description = "The database's id"),
        ("version" = String, Path, description = "A version number, or `latest`"),
    ),
    responses(
        (status = 200, body = VersionDetail),
        (status = 401, description = "`invalid_token`", body = ErrorBody),
        (status = 403, description = "`wrong_database`, `insufficient_scope`", body = ErrorBody),
        (status = 404, body = ErrorBody),
    )
)]
async fn read(
    token: Bearer<scope::Full>,
    State(state): State<AppState>,
    Path((_, version)): Path<(uuid::Uuid, String)>,
) -> ApiResult<Json<VersionDetail>> {
    let number = match version.as_str() {
        "latest" => None,
        n => Some(n.parse::<i32>().map_err(|_| {
            ApiError::BadRequest(format!(
                "{version:?} is not a version: use its number, or `latest`."
            ))
        })?),
    };
    Ok(Json(detail(&state.db, token.database_id, number).await?))
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn typed(content_type: Option<&'static str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(value) = content_type {
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(value));
        }
        headers
    }

    #[test]
    fn the_content_type_decides() {
        let format = |t, body: &str| format(&typed(t), body.as_bytes()).ok();
        assert_eq!(format(Some("application/json"), "x"), Some(Format::Json));
        assert_eq!(
            format(Some("application/json; charset=utf-8"), "x"),
            Some(Format::Json)
        );
        assert_eq!(format(Some("text/plain"), "{"), Some(Format::Sql));
        assert_eq!(format(Some("application/sql"), "{"), Some(Format::Sql));
        assert_eq!(format(Some("text/html"), "{"), None);
    }

    #[test]
    fn curl_defaults_are_sniffed() {
        let format = |t, body: &str| format(&typed(t), body.as_bytes()).ok();
        let curl = Some("application/x-www-form-urlencoded");
        assert_eq!(
            format(curl, "\n  {\"remus_version\": 1}"),
            Some(Format::Json)
        );
        assert_eq!(format(curl, "CREATE TABLE t ();"), Some(Format::Sql));
        assert_eq!(format(None, "{}"), Some(Format::Json));
        assert_eq!(
            format(Some("application/octet-stream"), "--"),
            Some(Format::Sql)
        );
    }
}
