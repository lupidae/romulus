//! The middleware every request goes through, outermost first.

use std::{any::Any, time::Duration};

use axum::{
    Router,
    body::to_bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderName, Method, Request, header},
    middleware::{Next, from_fn_with_state, map_response},
    response::{IntoResponse, Response},
};
use tower::ServiceBuilder;
use tower_http::{
    catch_panic::CatchPanicLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::{Level, Span};

use crate::ApiError;

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Bodies above this are refused unless a route raises its own limit with
/// `DefaultBodyLimit::max` (the remus push endpoint will).
const BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Covers producing the response, not the life of a websocket after its 101
/// (nor a body streamed after its headers).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Migration imports (`POST …/imports`) replay up to hundreds of migrations
/// in the sandbox before they answer.
const IMPORT_TIMEOUT: Duration = Duration::from_secs(15 * 60);

pub fn around(router: Router) -> Router {
    with_limits(router, BODY_LIMIT, REQUEST_TIMEOUT)
}

pub(crate) fn with_limits(router: Router, body_limit: usize, timeout: Duration) -> Router {
    router.layer(DefaultBodyLimit::max(body_limit)).layer(
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
            // Outside the timeout, so its 503 is rewritten too.
            .layer(map_response(errors_as_json))
            .layer(from_fn_with_state(timeout, deadline)),
    )
}

/// The request timeout: a 503 `timeout` past it. Imports get
/// [`IMPORT_TIMEOUT`].
async fn deadline(State(timeout): State<Duration>, request: Request<axum::body::Body>, next: Next) -> Response {
    let import = request.method() == Method::POST && request.uri().path().ends_with("/imports");
    let limit = if import {
        timeout.max(IMPORT_TIMEOUT)
    } else {
        timeout
    };
    match tokio::time::timeout(limit, next.run(request)).await {
        Ok(response) => response,
        Err(_) => ApiError::Timeout.into_response(),
    }
}

/// Every error leaves in the API's JSON format, whoever produced it: our
/// handlers already do, this catches the rest (routing's 405, an extractor
/// that is not ours, the timeout). Headers such as `Allow` are kept.
async fn errors_as_json(response: Response) -> Response {
    let status = response.status();
    let is_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"));
    if !(status.is_client_error() || status.is_server_error()) || is_json {
        return response;
    }
    let (parts, body) = response.into_parts();
    let text = to_bytes(body, 16 * 1024)
        .await
        .ok()
        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty());
    let mut json = ApiError::from_status(status, text).into_response();
    for (name, value) in &parts.headers {
        if name != header::CONTENT_TYPE && name != header::CONTENT_LENGTH {
            json.headers_mut().insert(name.clone(), value.clone());
        }
    }
    json
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
        path = %redacted(request.uri().path()),
        request_id = %id,
    )
}

/// The path as logs may show it: segments shaped like a secret this API
/// hands out (an invitation link's token, `crate::token`) are replaced, so a
/// log line never carries a usable link.
fn redacted(path: &str) -> std::borrow::Cow<'_, str> {
    if !path.split('/').any(crate::token::is_well_formed) {
        return path.into();
    }
    path.split('/')
        .map(|segment| {
            if crate::token::is_well_formed(segment) {
                "<token>"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
        .into()
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

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Method, Request},
        routing::{get, post},
    };
    use serde::Deserialize;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;
    use crate::extract::{Bytes, Json, Path, Query};

    #[derive(Deserialize)]
    struct Named {
        #[expect(dead_code, reason = "only deserialized")]
        name: String,
    }

    #[derive(Deserialize)]
    struct Page {
        #[expect(dead_code, reason = "only deserialized")]
        page: u32,
    }

    fn app() -> Router {
        let router = Router::new()
            .route("/json", post(|Json(_): Json<Named>| async { "ok" }))
            .route("/bytes", post(|Bytes(_): Bytes| async { "ok" }))
            .route("/raw", post(|_: String| async { "ok" }))
            .route("/items/{id}", get(|Path(_): Path<u32>| async { "ok" }))
            .route("/list", get(|Query(_): Query<Page>| async { "ok" }))
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    "late"
                }),
            );
        with_limits(router, 32, Duration::from_millis(50))
    }

    async fn send(request: Request<Body>) -> (StatusCode, Value) {
        let response = app().oneshot(request).await.unwrap();
        let status = response.status();
        let content_type = response.headers()[header::CONTENT_TYPE].clone();
        assert_eq!(content_type, "application/json", "{status} is not JSON");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    fn post_json(path: &str, body: &'static str) -> Request<Body> {
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap()
    }

    fn get_request(path: &str) -> Request<Body> {
        Request::get(path).body(Body::empty()).unwrap()
    }

    #[test]
    fn logged_paths_hide_tokens() {
        let token = crate::token::generate();
        assert_eq!(
            redacted(&format!("/api/invitations/{token}/accept")),
            "/api/invitations/<token>/accept"
        );
        assert_eq!(redacted("/api/orgs/acme/members"), "/api/orgs/acme/members");
    }

    #[tokio::test]
    async fn malformed_json_is_a_400() {
        let (status, body) = send(post_json("/json", "{nope")).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("bad_request"))
        );
    }

    #[tokio::test]
    async fn a_missing_field_is_a_422_naming_it() {
        let (status, body) = send(post_json("/json", "{}")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["code"], "validation");
        assert!(body["message"].as_str().unwrap().contains("name"));
    }

    #[tokio::test]
    async fn json_without_its_content_type_is_a_415() {
        let request = Request::post("/json")
            .body(Body::from(r#"{"name":"a"}"#))
            .unwrap();
        let (status, body) = send(request).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                Some("unsupported_media_type")
            )
        );
    }

    #[tokio::test]
    async fn bodies_over_the_limit_are_a_413() {
        for path in ["/json", "/bytes", "/raw"] {
            let (status, body) = send(post_json(
                path,
                r#"{"name":"far more than thirty-two bytes"}"#,
            ))
            .await;
            assert_eq!(
                (status, body["code"].as_str()),
                (StatusCode::PAYLOAD_TOO_LARGE, Some("payload_too_large")),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn bad_path_and_query_parameters_are_a_400() {
        for path in ["/items/abc", "/list?page=minus-one"] {
            let (status, body) = send(get_request(path)).await;
            assert_eq!(
                (status, body["code"].as_str()),
                (StatusCode::BAD_REQUEST, Some("bad_request")),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn a_wrong_method_is_a_405_that_keeps_allow() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[header::ALLOW], "POST");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["code"], "method_not_allowed");
    }

    #[tokio::test]
    async fn the_timeout_is_a_503_in_the_api_format() {
        let (status, body) = send(get_request("/slow")).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, Some("timeout"))
        );
    }
}
