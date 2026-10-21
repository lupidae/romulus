//! The one error type handlers return, and the JSON body every error has.

use axum::{
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{db, extract::Json};

/// The body of every error response.
///
/// `code` is stable and machine-readable (the web app switches on it);
/// `message` is for people and may change.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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
    /// 400 with a specific code (`bad_signature`: a webhook not signed by
    /// its sender).
    #[error("{message}")]
    Rejected { code: &'static str, message: String },
    /// 422: well-formed but invalid input; `details` can carry per-field messages.
    #[error("{message}")]
    Validation {
        message: String,
        details: Option<serde_json::Value>,
    },
    /// 422 with a specific code (`compile_failed`, `invalid_schema`) and
    /// structured `details`, where `Validation`'s per-field shape does not fit.
    #[error("{message}")]
    Unprocessable {
        code: &'static str,
        message: String,
        details: Option<serde_json::Value>,
    },
    /// 401: no valid session or token.
    #[error("sign in to continue")]
    Unauthorized,
    /// 401 on the machine API (`Authorization: Bearer`): no token, or one that
    /// is unknown or revoked. The message says how to get a working one.
    #[error("{0}")]
    InvalidToken(String),
    /// 401 from a password sign-in: the address has no account or the
    /// password is wrong (never which).
    #[error("Email or password is incorrect.")]
    InvalidCredentials,
    /// 401: the request carried a session cookie, but that session expired
    /// (or was signed out elsewhere). The web app says so on the sign-in page.
    #[error("Your session expired. Sign in again to continue.")]
    SessionExpired,
    /// 403: signed in, but the role does not allow it.
    #[error("{0}")]
    Forbidden(String),
    /// 403 with a specific code the web app branches on (`wrong_account`,
    /// `email_unverified`…), where `Forbidden`'s generic code is not enough.
    #[error("{message}")]
    Denied { code: &'static str, message: String },
    /// 403: a cookie-authenticated mutation without the CSRF header or the right `Origin`.
    #[error("cross-site request refused")]
    Csrf,
    /// 404.
    #[error("{0}")]
    NotFound(String),
    /// 405: the path exists, the method does not.
    #[error("method not allowed")]
    MethodNotAllowed,
    /// 409, with a specific code (`slug_taken`, `last_admin`, `stale_base`…).
    #[error("{message}")]
    Conflict { code: &'static str, message: String },
    /// 402 `plan_limit`: the organization's plan does not allow it (hosted
    /// billing). `details` is a `billing::PlanLimitDetails`: the cap, the
    /// usage, the plan, the plan that lifts it. Nothing was changed.
    #[error("{message}")]
    PlanLimit {
        message: String,
        details: serde_json::Value,
    },
    /// 413.
    #[error("request body too large")]
    PayloadTooLarge,
    /// 415: e.g. a JSON endpoint called without `Content-Type: application/json`.
    #[error("{0}")]
    UnsupportedMediaType(String),
    /// 429: too many requests from this token or person; `Retry-After` says
    /// when the next one is allowed.
    #[error("Too many requests. Try again in {} s.", whole_seconds(*retry_after))]
    RateLimited { retry_after: std::time::Duration },
    /// 503: a dependency (the database, the sandbox) is unreachable.
    #[error("{0}")]
    Unavailable(String),
    /// 503 with a specific code (`host_unavailable`): what one feature needs
    /// (the playground host) is down or not configured.
    #[error("{message}")]
    Down { code: &'static str, message: String },
    /// 503: the request did not finish within the server's time limit.
    #[error("the request took too long")]
    Timeout,
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
            Self::BadRequest(_) | Self::Rejected { .. } => StatusCode::BAD_REQUEST,
            Self::Validation { .. } | Self::Unprocessable { .. } => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            Self::Unauthorized
            | Self::SessionExpired
            | Self::InvalidToken(_)
            | Self::InvalidCredentials => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) | Self::Denied { .. } | Self::Csrf => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::Conflict { .. } => StatusCode::CONFLICT,
            Self::PlanLimit { .. } => StatusCode::PAYMENT_REQUIRED,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnsupportedMediaType(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::Unavailable(_) | Self::Down { .. } | Self::Timeout => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::Validation { .. } => "validation",
            Self::Unauthorized => "unauthorized",
            Self::SessionExpired => "session_expired",
            Self::InvalidCredentials => "invalid_credentials",
            Self::InvalidToken(_) => "invalid_token",
            Self::Forbidden(_) => "forbidden",
            Self::Csrf => "csrf",
            Self::NotFound(_) => "not_found",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::Denied { code, .. }
            | Self::Rejected { code, .. }
            | Self::Conflict { code, .. }
            | Self::Unprocessable { code, .. }
            | Self::Down { code, .. } => code,
            Self::PlanLimit { .. } => "plan_limit",
            Self::PayloadTooLarge => "payload_too_large",
            Self::UnsupportedMediaType(_) => "unsupported_media_type",
            Self::RateLimited { .. } => "rate_limited",
            Self::Unavailable(_) => "unavailable",
            Self::Timeout => "timeout",
            Self::Internal(_) => "internal",
        }
    }
}

impl ApiError {
    /// An error response produced outside our handlers (a 405 from routing, a
    /// raw extractor's rejection, the timeout), for `layers::errors_as_json`.
    /// `text` is the original body, kept for 4xx; a 5xx never echoes it.
    pub fn from_status(status: StatusCode, text: Option<String>) -> Self {
        let text = text.unwrap_or_else(|| status.to_string());
        match status {
            StatusCode::UNAUTHORIZED => Self::Unauthorized,
            StatusCode::FORBIDDEN => Self::Forbidden(text),
            StatusCode::NOT_FOUND => Self::NotFound(text),
            StatusCode::METHOD_NOT_ALLOWED => Self::MethodNotAllowed,
            StatusCode::PAYLOAD_TOO_LARGE => Self::PayloadTooLarge,
            StatusCode::UNSUPPORTED_MEDIA_TYPE => Self::UnsupportedMediaType(text),
            StatusCode::UNPROCESSABLE_ENTITY => Self::Validation {
                message: text,
                details: None,
            },
            StatusCode::REQUEST_TIMEOUT
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT => Self::Timeout,
            s if s.is_client_error() => Self::BadRequest(text),
            s => Self::Internal(format!("{s}: {text}").into()),
        }
    }
}

/// `?` on a query: invariants the database enforces become their 409, the
/// rest is a 500. Map other constraints (a taken slug…) where the query runs,
/// with [`db::constraint`] or [`db::is_unique_violation`].
impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        let conflict = |code, message: &str| Self::Conflict {
            code,
            message: message.to_owned(),
        };
        match db::constraint(&err) {
            Some(db::ORGANIZATION_NEEDS_AN_ADMIN) => conflict(
                "last_admin",
                "An organization needs at least one admin. Make another member an admin first.",
            ),
            Some(db::VERSIONS_ARE_IMMUTABLE | db::VERSIONS_ARE_PERMANENT) => conflict(
                "version_immutable",
                "Versions cannot be changed or deleted.",
            ),
            _ if db::is_serialization_failure(&err) => conflict(
                "concurrent_change",
                "Someone changed this at the same time. Try again.",
            ),
            _ => Self::internal(err),
        }
    }
}

impl ApiError {
    /// The JSON body this error answers with, for an error carried inside
    /// another answer (an import's progress stream, a preview's problem).
    pub fn body(&self) -> ErrorBody {
        let details = match self {
            Self::Validation { details, .. } | Self::Unprocessable { details, .. } => {
                details.clone()
            }
            Self::PlanLimit { details, .. } => Some(details.clone()),
            // Also in `Retry-After`, which a page's fetch wrapper may not read.
            Self::RateLimited { retry_after } => {
                Some(serde_json::json!({ "retry_after": whole_seconds(*retry_after) }))
            }
            _ => None,
        };
        let message = if let Self::Internal(cause) = self {
            tracing::error!(error = %Chain(cause.as_ref()), "internal error");
            "Something went wrong on our side.".to_owned()
        } else {
            self.to_string()
        };
        ErrorBody {
            code: self.code().to_owned(),
            message,
            details,
        }
    }
}

/// A 402 `plan_limit`'s details (a `billing::PlanLimitDetails`), in the
/// extensions of its response: the operators hear of it from there
/// (`operator::limits`).
#[derive(Debug, Clone)]
pub struct PlanLimitReached(pub serde_json::Value);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = self.body();
        let mut response = (status, Json(body)).into_response();
        if let Self::PlanLimit { details, .. } = &self {
            response
                .extensions_mut()
                .insert(PlanLimitReached(details.clone()));
        }
        if let Self::RateLimited { retry_after } = &self {
            let seconds = whole_seconds(*retry_after);
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        }
        response
    }
}

/// A wait in whole seconds, rounded up and at least 1: `Retry-After: 0`
/// would have a client retry at once.
fn whole_seconds(duration: std::time::Duration) -> u64 {
    (duration.as_secs() + u64::from(duration.subsec_nanos() > 0)).max(1)
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
    fn denials_carry_their_own_code() {
        let err = ApiError::Denied {
            code: "wrong_account",
            message: "no".into(),
        };
        assert_eq!(
            (err.status(), err.code()),
            (StatusCode::FORBIDDEN, "wrong_account")
        );
        assert_eq!(
            ApiError::SessionExpired.status(),
            ApiError::Unauthorized.status()
        );
    }

    #[test]
    fn a_rate_limit_says_when_to_come_back() {
        let response = ApiError::RateLimited {
            retry_after: std::time::Duration::from_millis(2500),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[header::RETRY_AFTER], "3");
        // Never "0": a client would retry at once.
        let soon = ApiError::RateLimited {
            retry_after: std::time::Duration::from_millis(10),
        }
        .into_response();
        assert_eq!(soon.headers()[header::RETRY_AFTER], "1");
    }

    #[test]
    fn foreign_error_responses_keep_their_meaning() {
        let cases = [
            (StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed"),
            (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"),
            (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type"),
            (StatusCode::REQUEST_TIMEOUT, "timeout"),
            (StatusCode::SERVICE_UNAVAILABLE, "timeout"),
            (StatusCode::BAD_REQUEST, "bad_request"),
            (StatusCode::IM_A_TEAPOT, "bad_request"),
            (StatusCode::BAD_GATEWAY, "internal"),
        ];
        for (status, code) in cases {
            assert_eq!(ApiError::from_status(status, None).code(), code, "{status}");
        }
        let err = ApiError::from_status(StatusCode::BAD_GATEWAY, Some("secret detail".into()));
        assert!(!err.to_string().contains("secret"));
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
