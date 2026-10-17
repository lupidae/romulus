//! `GET /api/health`: the API is up and can reach its database, and whether
//! the SQL compiler can run.

use std::{collections::BTreeMap, time::Duration};

use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    AppState,
    compile::CompilerStatus,
    domain::PgMajor,
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
    /// The default major's SQL compiler (17): `ok`, `unavailable` (starting,
    /// or the sandbox is down) or `off` (no sandbox configured). Not part of
    /// the status: the rest of the API works without it.
    pub sandbox: String,
    /// Every supported major's compiler, by major (`"15"`…`"18"`), as
    /// `sandbox` says it: a database compiles on its own major's.
    pub sandboxes: BTreeMap<String, String>,
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
        Ok(Ok(_)) => {
            let sandboxes = sandbox_statuses(&state).await;
            Ok(Json(Health {
                status: "ok".to_owned(),
                database: "ok".to_owned(),
                sandbox: sandboxes
                    .get(&PgMajor::DEFAULT.to_string())
                    .cloned()
                    .unwrap_or_else(|| "unavailable".to_owned()),
                sandboxes,
                version: env!("CARGO_PKG_VERSION").to_owned(),
            }))
        }
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

/// Each major's compiler, asked together: a sandbox that hangs answers
/// `unavailable` after 2 s, the others are not held up by it.
async fn sandbox_statuses(state: &AppState) -> BTreeMap<String, String> {
    let checks = PgMajor::SUPPORTED.map(|major| {
        let compiler = state.compiler.clone();
        async move {
            let status = tokio::time::timeout(Duration::from_secs(2), compiler.status(major))
                .await
                .unwrap_or(CompilerStatus::Unavailable);
            (major.to_string(), status.as_str().to_owned())
        }
    });
    futures_util::future::join_all(checks)
        .await
        .into_iter()
        .collect()
}
