//! The extractors handlers use instead of axum's: same behaviour, but a
//! rejected request (malformed JSON, a missing field, a bad path or query
//! parameter, a body over the limit) answers in the API's error format with a
//! precise code, like every other error. `clippy.toml` disallows the axum
//! originals everywhere else, so a module cannot forget.
//!
//! ```ignore
//! use crate::extract::{Json, Path, Query};
//!
//! async fn rename(Path(slug): Path<String>, Json(body): Json<Rename>) -> ApiResult<Json<Org>> { … }
//! ```
//!
//! Anything that still produces a non-JSON error (another extractor, a 405,
//! the request timeout) is rewritten by `layers::errors_as_json`.
#![expect(
    clippy::disallowed_types,
    reason = "this module is where axum's extractors are wrapped"
)]

use axum::{
    extract::{
        FromRequest, FromRequestParts, OptionalFromRequest, Request,
        rejection::{BytesRejection, JsonRejection, PathRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::ApiError;

/// A JSON request body (`Content-Type: application/json`) or response.
#[derive(Debug, Clone, Copy, Default, FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct Json<T>(pub T);

/// An optional JSON body (`Option<Json<T>>`): `None` without a
/// `Content-Type`, the same rejections as [`Json`] otherwise.
impl<T, S> OptionalFromRequest<S> for Json<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Option<Self>, Self::Rejection> {
        let json = <axum::Json<T> as OptionalFromRequest<S>>::from_request(req, state).await?;
        Ok(json.map(|axum::Json(value)| Self(value)))
    }
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// Path parameters, e.g. `Path(slug): Path<String>` for `/orgs/{slug}`.
#[derive(Debug, Clone, Copy, Default, FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(ApiError))]
pub struct Path<T>(pub T);

/// Query string parameters, deserialized into `T`.
#[derive(Debug, Clone, Copy, Default, FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(ApiError))]
pub struct Query<T>(pub T);

/// A raw body (an SQL upload), with the body limit answered as a 413 in the
/// API's format.
#[derive(Debug, Clone, Default)]
pub struct Bytes(pub axum::body::Bytes);

impl<S: Send + Sync> FromRequest<S> for Bytes {
    type Rejection = ApiError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, ApiError> {
        axum::body::Bytes::from_request(req, state)
            .await
            .map(Self)
            .map_err(ApiError::from)
    }
}

/// Reads a raw body (`axum::body::Body`, taken last by a handler that must
/// decide something before reading it, such as a rate limit), refusing more
/// than `limit` bytes with 413 in the API's format: at once when
/// `Content-Length` says so, otherwise as soon as the stream goes past it.
pub async fn read_body(
    headers: &axum::http::HeaderMap,
    body: axum::body::Body,
    limit: usize,
) -> Result<axum::body::Bytes, ApiError> {
    let declared = headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > limit as u64) {
        return Err(ApiError::PayloadTooLarge);
    }
    axum::body::to_bytes(body, limit).await.map_err(|err| {
        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&err);
        while let Some(cause) = source {
            if cause.is::<http_body_util::LengthLimitError>() {
                return ApiError::PayloadTooLarge;
            }
            source = cause.source();
        }
        ApiError::BadRequest(format!("The body could not be read: {err}"))
    })
}

/// Maps an axum rejection by its status; the text says what was wrong
/// (serde's message names the field and position).
fn rejected(status: StatusCode, text: String) -> ApiError {
    match status {
        StatusCode::PAYLOAD_TOO_LARGE => ApiError::PayloadTooLarge,
        StatusCode::UNSUPPORTED_MEDIA_TYPE => ApiError::UnsupportedMediaType(text),
        StatusCode::UNPROCESSABLE_ENTITY => ApiError::Validation {
            message: text,
            details: None,
        },
        _ => ApiError::BadRequest(text),
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        rejected(rejection.status(), rejection.body_text())
    }
}

impl From<PathRejection> for ApiError {
    fn from(rejection: PathRejection) -> Self {
        // A path that does not parse names a resource that cannot exist.
        if rejection.status() == StatusCode::INTERNAL_SERVER_ERROR {
            return ApiError::internal(rejection);
        }
        ApiError::BadRequest(rejection.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(rejection: QueryRejection) -> Self {
        ApiError::BadRequest(rejection.body_text())
    }
}

impl From<BytesRejection> for ApiError {
    fn from(rejection: BytesRejection) -> Self {
        rejected(rejection.status(), rejection.body_text())
    }
}
