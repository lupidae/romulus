use std::sync::Arc;

use axum::extract::FromRef;
use sqlx::PgPool;

use crate::{Config, auth::oidc::Oidc, orgs::events::MembershipEvents};

/// Shared by every handler. Cheap to clone: a pool handle and `Arc`s.
///
/// Handlers can extract the parts directly (`State(db): State<PgPool>`,
/// `State(config): State<Arc<Config>>`) thanks to the `FromRef` impls.
#[derive(Clone, FromRef)]
pub struct AppState {
    pub db: PgPool,
    pub config: Arc<Config>,
    /// The OIDC relying party and its discovery cache.
    pub oidc: Arc<Oidc>,
    /// Role changes and removals, for the collab server to close sockets.
    pub membership: MembershipEvents,
}

impl AppState {
    pub fn new(db: PgPool, config: Config) -> Self {
        Self {
            db,
            oidc: Arc::new(Oidc::new(&config)),
            config: Arc::new(config),
            membership: MembershipEvents::new(),
        }
    }
}
