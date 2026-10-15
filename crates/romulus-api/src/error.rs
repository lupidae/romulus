//! The one error type handlers return, and the JSON body every error has.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::db;

/// The body of every error response.
///
/// `code` is stable and machine-readable (the web app switches on it);
/// `message` is for people and may change.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    /// e.g. `not_found`, `validation`, `csrf`, `last_admin`.
    pub code: String,
    pub message: String,
    /// Structured context, e.g. per-field validation messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// 400: the request is malformed.
    #[error("{0}")]
    BadRequest(String),
    /// 422: well-formed but invalid input; `details` can carry per-field messages.
    #[error("{message}")]
    Validation {
        message: String,
        details: Option<serde_json::Value>,
    },
    /// 401: no valid session or token.
    #[error("sign in to continue")]
    Unauthorized,
    /// 403: signed in, but the role does not allow it.
    #[error("{0}")]
    Forbidden(String),
    /// 403: a cookie-authenticated mutation without the CSRF header or the right `Origin`.
    #[error("cross-site request refused")]
    Csrf,
    /// 404.
    #[error("{0}")]
    NotFound(String),
    /// 409, with a specific code (`slug_taken`, `last_admin`, `stale_base`…).
    #[error("{message}")]
    Conflict { code: &'static str, message: String },
    /// 413.
    #[error("request body too large")]
    PayloadTooLarge,
    /// 503: a dependency (the database, the sandbox) is unreachable.
    #[error("{0}")]
    Unavailable(String),
    /// 500. The cause is logged, never sent to the client.
    #[error("internal error")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl ApiError {
    pub fn not_found(what: impl Into<String>) -> Self {
        Self::NotFound(what.into())
    }

    pub fn internal(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Internal(Box::new(err))
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Validation { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) | Self::Csrf => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict { .. } => StatusCode::CONFLICT,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::Validation { .. } => "validation",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden(_) => "forbidden",
            Self::Csrf => "csrf",
            Self::NotFound(_) => "not_found",
            Self::Conflict { code, .. } => code,
            Self::PayloadTooLarge => "payload_too_large",
            Self::Unavailable(_) => "unavailable",
            Self::Internal(_) => "internal",
        }
    }
}

/// `?` on a query: invariants the database enforces become their 409, the
/// rest is a 500. Map other constraints (a taken slug…) where the query runs,
/// with [`db::constraint`] or [`db::is_unique_violation`].
impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        if db::constraint(&err) == Some(db::ORGANIZATION_NEEDS_AN_ADMIN) {
            return Self::Conflict {
                code: "last_admin",
                message:
                    "An organization needs at least one admin. Make another member an admin first."
                        .to_owned(),
            };
        }
        Self::internal(err)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let details = match &self {
            Self::Validation { details, .. } => details.clone(),
            _ => None,
        };
        let message = if let Self::Internal(cause) = &self {
            tracing::error!(error = %Chain(cause.as_ref()), "internal error");
            "Something went wrong on our side.".to_owned()
        } else {
            self.to_string()
        };
        let body = ErrorBody {
            code: self.code().to_owned(),
            message,
            details,
        };
        (status, Json(body)).into_response()
    }
}

/// The fallback for routes that do not exist, in the same JSON shape.
pub async fn not_found() -> ApiError {
    ApiError::not_found("No such endpoint.")
}

/// Formats an error with its sources: `outer: inner: root`.
pub struct Chain<'a>(pub &'a (dyn std::error::Error + 'static));

impl std::fmt::Display for Chain<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)?;
        let mut source = self.0.source();
        while let Some(err) = source {
            write!(f, ": {err}")?;
            source = err.source();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_errors_hide_their_cause() {
        let err = ApiError::internal(std::io::Error::other("disk on fire"));
        assert_eq!(err.code(), "internal");
        assert!(!err.to_string().contains("disk"));
    }

    #[test]
    fn conflicts_carry_their_own_code() {
        let err = ApiError::Conflict {
            code: "slug_taken",
            message: "taken".into(),
        };
        assert_eq!(
            (err.status(), err.code()),
            (StatusCode::CONFLICT, "slug_taken")
        );
    }

    #[test]
    fn chain_prints_every_source() {
        #[derive(Debug, thiserror::Error)]
        #[error("outer")]
        struct Outer(#[source] std::io::Error);
        let err = Outer(std::io::Error::other("root"));
        assert_eq!(Chain(&err).to_string(), "outer: root");
    }
}
