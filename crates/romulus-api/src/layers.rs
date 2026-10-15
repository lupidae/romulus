//! The middleware every request goes through, outermost first.

use std::{any::Any, time::Duration};

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderName, Request, StatusCode},
    response::{IntoResponse, Response},
};
use tower::ServiceBuilder;
use tower_http::{
    catch_panic::CatchPanicLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::{Level, Span};

use crate::ApiError;

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Bodies above this are refused unless a route raises its own limit with
/// `DefaultBodyLimit::max` (the remus push endpoint will).
const BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Covers producing the response, not the life of a websocket after its 101.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub fn around(router: Router) -> Router {
    router.layer(DefaultBodyLimit::max(BODY_LIMIT)).layer(
        ServiceBuilder::new()
            // A client-supplied id is kept, so a trace can span the web app's logs.
            .layer(SetRequestIdLayer::new(REQUEST_ID, MakeRequestUuid))
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(request_span)
                    // One line per request (status, latency) at the default level.
                    .on_response(DefaultOnResponse::new().level(Level::INFO)),
            )
            .layer(PropagateRequestIdLayer::new(REQUEST_ID))
            .layer(CatchPanicLayer::custom(|panic: Box<dyn Any + Send>| {
                panic_response(&*panic)
            }))
            .layer(TimeoutLayer::with_status_code(
                StatusCode::SERVICE_UNAVAILABLE,
                REQUEST_TIMEOUT,
            )),
    )
}

fn request_span<B>(request: &Request<B>) -> Span {
    let id = request
        .headers()
        .get(REQUEST_ID)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    tracing::info_span!(
        "request",
        method = %request.method(),
        path = %request.uri().path(),
        request_id = %id,
    )
}

fn panic_response(panic: &(dyn Any + Send)) -> Response {
    let detail = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic payload");
    tracing::error!(panic = detail, "handler panicked");
    ApiError::Internal(detail.to_owned().into()).into_response()
}
