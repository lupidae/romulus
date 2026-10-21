//! The romulus API: one axum app under `/api`, documented with utoipa.
//!
//! Feature modules (`auth`, `orgs`, `databases`, `versions`, `imports`,
//! `drafts`, `collab`, `compile`, `playgrounds`, `billing`, `mcp`, `oauth`) each expose `router()`, an [`OpenApiRouter`] whose
//! paths are relative to `/api`. [`routes`] merges them; it is the only place
//! that lists every module, so adding endpoints never means editing it.
//! With `ROMULUS_WEB_DIR` set, [`app`] also serves the web app at `/app`
//! ([`web`]). The one other path outside `/api` is OAuth's discovery,
//! `/.well-known/oauth-*` (`oauth::metadata`), where MCP clients look.

pub mod admin;
pub mod audit;
pub mod auth;
pub mod billing;
pub mod collab;
pub mod compile;
pub mod config;
pub mod csrf;
pub mod databases;
pub mod db;
pub mod domain;
pub mod drafts;
pub mod edge;
pub mod email;
pub mod error;
pub mod export;
pub mod extract;
pub mod forwarded;
pub mod health;
pub mod imports;
pub mod instance;
mod layers;
pub mod mail;
pub mod mcp;
pub mod migration_tests;
pub mod oauth;
pub mod orgs;
pub mod playgrounds;
pub mod rate;
pub mod retention;
pub mod state;
pub mod token;
pub mod versions;
pub mod web;

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
    components(schemas(
        error::ErrorBody,
        billing::PlanLimitDetails,
        imports::ImportFailure,
        playgrounds::ConsoleNotice,
        romulus_playground::protocol::ConsoleRequest,
        romulus_playground::protocol::ConsoleEvent,
    ))
)]
struct ApiDoc;

/// Every endpoint, relative to `/api`. One line per feature module.
fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .merge(health::router())
        .merge(instance::router())
        .merge(auth::router())
        .merge(audit::router())
        .merge(orgs::router())
        .merge(databases::router())
        .merge(versions::router())
        .merge(imports::router())
        .merge(drafts::router())
        .merge(collab::router())
        .merge(compile::router())
        .merge(playgrounds::router())
        .merge(migration_tests::router())
        .merge(billing::router())
        .merge(export::router())
        .merge(mcp::router())
        .merge(oauth::router())
}

pub(crate) fn router_and_spec() -> (Router<AppState>, utoipa::openapi::OpenApi) {
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
    let mut router = router
        .route("/api/openapi.json", get(move || async move { Json(spec) }))
        .merge(oauth::metadata::well_known());
    if let Some(dir) = &state.config.web_dir {
        router = router.merge(web::router(dir));
    }
    let router = router
        .fallback(error::not_found)
        .layer(middleware::from_fn_with_state(state.clone(), csrf::guard))
        // The client behind a trusted reverse proxy (after the edge, which
        // names it itself).
        .layer(middleware::from_fn_with_state(
            state.clone(),
            forwarded::client,
        ))
        // Outside the CSRF guard: a request that did not come through the
        // edge is refused before anything else looks at it.
        .layer(middleware::from_fn_with_state(state.clone(), edge::guard))
        .with_state(state);
    layers::around(router)
}
