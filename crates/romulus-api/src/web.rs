//! The web app's production build (`web/dist`), served at `/app` when
//! `ROMULUS_WEB_DIR` points at it (the container image does; in development
//! Vite serves `/app` itself). Vite's base is `/app/`, so the build's own URLs
//! already carry the prefix.
//!
//! - `/app/assets/…` are content-hashed by Vite: cached for a year,
//!   `immutable`. A missing one is a 404, never the page: a tab left open
//!   across a deploy must not receive HTML for the script it asks for.
//! - `index.html`, and every path that is not a file of the build (the
//!   router's routes, `/app/acme/db/history`), answer the page with
//!   `no-cache`, so the next navigation after a deploy picks up the new build.
//! - Other files of the build (`favicon.svg`) are cached for an hour.
//! - `/app` redirects to `/app/`.

use std::path::Path;

use axum::{
    Router,
    body::Body,
    extract::Request,
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

const IMMUTABLE: HeaderValue = HeaderValue::from_static("public, max-age=31536000, immutable");
const AN_HOUR: HeaderValue = HeaderValue::from_static("public, max-age=3600");
const NO_CACHE: HeaderValue = HeaderValue::from_static("no-cache");
/// The app is never framed (clickjacking); the rest of its CSP would need
/// `wasm-unsafe-eval` and inline styles, so it stays at that for now.
const FRAME_ANCESTORS: HeaderValue = HeaderValue::from_static("frame-ancestors 'none'");

/// `/app` and everything under it, from the build in `dir`.
pub fn router<S>(dir: &Path) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    // Directories are not listed nor resolved to an index: they fall back
    // to the page like any other route.
    let files = ServeDir::new(dir).append_index_html_on_directories(false);
    let page = ServeFile::new(dir.join("index.html"));
    let serve = move |request: Request| {
        let (files, page) = (files.clone(), page.clone());
        async move { serve(files, page, request).await }
    };
    Router::new()
        .route("/app", get(|| async { Redirect::permanent("/app/") }))
        .route("/app/", get(serve.clone()))
        .route("/app/{*path}", get(serve))
}

async fn serve(files: ServeDir, page: ServeFile, request: Request) -> Response {
    let path = request
        .uri()
        .path()
        .strip_prefix("/app")
        .unwrap_or_default()
        .to_owned();
    let asset = path.starts_with("/assets/");
    let (method, headers) = (request.method().clone(), request.headers().clone());

    let Ok(found) = files.oneshot(rebuild(&method, &headers, &path)).await;
    let found = found.map(Body::new);
    let (mut response, cache) = if asset {
        (found, IMMUTABLE)
    } else if found.status() == StatusCode::NOT_FOUND {
        let Ok(page) = page.oneshot(rebuild(&method, &headers, "/")).await;
        (page.map(Body::new), NO_CACHE)
    } else if Path::new(&path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("html"))
    {
        (found, NO_CACHE)
    } else {
        (found, AN_HOUR)
    };

    let cacheable = response_is_cacheable(response.status());
    let headers = response.headers_mut();
    if cacheable {
        headers.insert(header::CACHE_CONTROL, cache);
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CONTENT_SECURITY_POLICY, FRAME_ANCESTORS);
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    response.into_response()
}

/// What `ServeDir` resolves: the path inside the build, with the caller's
/// method and headers (conditional and range requests keep working).
fn rebuild(method: &Method, headers: &HeaderMap, path: &str) -> Request {
    let mut request = Request::new(Body::empty());
    *request.method_mut() = method.clone();
    *request.headers_mut() = headers.clone();
    // The path came from a valid URI, minus a prefix: it still is one.
    *request.uri_mut() = Uri::try_from(path).unwrap_or_else(|_| Uri::from_static("/"));
    request
}

/// A 404 for a missing asset must not be cached for a year.
fn response_is_cacheable(status: StatusCode) -> bool {
    status.is_success() || status == StatusCode::NOT_MODIFIED
}
