//! The romulus API: one axum app under `/api`, documented with utoipa.
//!
//! Feature modules (`auth`, `orgs`, `databases`, `versions`, `drafts`,
//! `collab`, `compile`) each expose `router()`, an [`OpenApiRouter`] whose
//! paths are relative to `/api`. [`routes`] merges them; it is the only place
//! that lists every module, so adding endpoints never means editing it.

pub mod auth;
pub mod collab;
pub mod compile;
pub mod config;
pub mod csrf;
pub mod databases;
pub mod db;
pub mod domain;
pub mod drafts;
pub mod error;
pub mod extract;
pub mod health;
mod layers;
pub mod orgs;
pub mod state;
pub mod token;
pub mod versions;

use std::sync::Arc;

use axum::{Router, middleware, routing::get};
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;

pub use crate::{
    config::Config,
    error::{ApiError, ApiResult},
    extract::Json,
    state::AppState,
};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "romulus",
        description = "DBA as a service: versioned Postgres schemas, diffs and collaborative drafts."
    ),
    components(schemas(error::ErrorBody))
)]
struct ApiDoc;

/// Every endpoint, relative to `/api`. One line per feature module.
fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .merge(health::router())
        .merge(auth::router())
        .merge(orgs::router())
        .merge(databases::router())
        .merge(versions::router())
        .merge(drafts::router())
        .merge(collab::router())
        .merge(compile::router())
}

fn router_and_spec() -> (Router<AppState>, utoipa::openapi::OpenApi) {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .nest("/api", routes())
        .split_for_parts()
}

/// The OpenAPI document, built without a database or configuration
/// (`romulus-api openapi`, which `just gen` feeds to the web app).
pub fn openapi() -> utoipa::openapi::OpenApi {
    router_and_spec().1
}

/// The whole HTTP app, ready to serve.
pub fn app(state: AppState) -> Router {
    let (router, spec) = router_and_spec();
    let spec = Arc::new(spec);
    let router = router
        .route("/api/openapi.json", get(move || async move { Json(spec) }))
        .fallback(error::not_found)
        .layer(middleware::from_fn_with_state(state.clone(), csrf::guard))
        .with_state(state);
    layers::around(router)
}
