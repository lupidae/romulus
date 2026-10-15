//! `GET /api/health`: the API is up and can reach its database, and whether
//! the SQL compiler can run.

use std::time::Duration;

use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    AppState,
    error::{ApiError, ApiResult, Chain, ErrorBody},
    extract::Json,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(health))
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct Health {
    /// Always `ok` in a 200.
    pub status: String,
    /// `ok` when a query round-trips.
    pub database: String,
    /// The SQL compiler (uploads, SQL pushes): `ok`, `unavailable` (starting,
    /// or the sandbox is down) or `off` (no sandbox configured). Not part of
    /// the status: the rest of the API works without it.
    pub sandbox: String,
    /// The API's crate version.
    pub version: String,
}

/// Liveness and database reachability, for compose healthchecks and CI.
#[utoipa::path(
    get,
    path = "/health",
    tag = "health",
    responses(
        (status = 200, description = "The API and its database are up", body = Health),
        (status = 503, description = "The database is unreachable", body = ErrorBody),
    )
)]
async fn health(State(state): State<AppState>) -> ApiResult<Json<Health>> {
    // Shorter than the pool's acquire timeout: a health check that hangs is
    // worse than one that says no.
    let probe = sqlx::query_scalar!(r#"SELECT 1 AS "one!""#).fetch_one(&state.db);
    match tokio::time::timeout(Duration::from_secs(2), probe).await {
        Ok(Ok(_)) => Ok(Json(Health {
            status: "ok".to_owned(),
            database: "ok".to_owned(),
            sandbox: sandbox_status(&state).await.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        })),
        Ok(Err(err)) => {
            tracing::warn!(error = %Chain(&err), "health: database unreachable");
            Err(ApiError::Unavailable(
                "The database is unreachable.".to_owned(),
            ))
        }
        Err(_) => {
            tracing::warn!("health: database did not answer within 2s");
            Err(ApiError::Unavailable(
                "The database did not answer in time.".to_owned(),
            ))
        }
    }
}

async fn sandbox_status(state: &AppState) -> &'static str {
    match tokio::time::timeout(Duration::from_secs(2), state.compiler.status()).await {
        Ok(status) => status.as_str(),
        Err(_) => "unavailable",
    }
}
