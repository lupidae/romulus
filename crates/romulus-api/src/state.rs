use std::sync::Arc;

use axum::extract::FromRef;
use sqlx::PgPool;

use crate::Config;

/// Shared by every handler. Cheap to clone: a pool handle and an `Arc`.
///
/// Handlers can extract the parts directly (`State(db): State<PgPool>`,
/// `State(config): State<Arc<Config>>`) thanks to the `FromRef` impls.
#[derive(Clone, FromRef)]
pub struct AppState {
    pub db: PgPool,
    pub config: Arc<Config>,
}

impl AppState {
    pub fn new(db: PgPool, config: Config) -> Self {
        Self {
            db,
            config: Arc::new(config),
        }
    }
}
