//! `GET /api/instance`: how this server is set up, for the app to show only
//! what works here. Public: it says nothing about anyone.

use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{AppState, extract::Json, legal::LegalLinks};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(instance))
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[allow(clippy::struct_excessive_bools, reason = "what is on, one flag each")]
pub struct Instance {
    /// One organization per install (`ROMULUS_SINGLE_ORG`): no way to create
    /// another.
    pub single_org: bool,
    /// romulus sends email (invitations, sign-up and reset links).
    pub mail: bool,
    /// Playgrounds are set up on this server (`ROMULUS_PLAY_URL`).
    pub playgrounds: bool,
    /// Plans and billing (the hosted service, `ROMULUS_BILLING`): the app
    /// shows the billing page and upgrade prompts.
    pub billing: bool,
    /// The version running and where its source is (AGPL-3.0, section 13).
    pub source: InstanceSource,
    /// The hosted service's Terms and Privacy Policy (`ROMULUS_TERMS_VERSION`):
    /// the sign-in pages and the app's footer link them. `null` on a
    /// self-hosted install, which has none.
    pub legal: Option<LegalLinks>,
}

/// Which romulus runs here, and where to read its source.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct InstanceSource {
    /// The API's crate version.
    pub version: String,
    /// The commit it was built from; `null` when the build did not say.
    pub commit: Option<String>,
    /// The source of that commit (`<repository>/tree/<commit>`), or the
    /// repository when the commit is unknown (`ROMULUS_SOURCE_URL`).
    pub url: String,
}

/// How this server is set up.
#[utoipa::path(
    get,
    path = "/instance",
    operation_id = "instance",
    tag = "health",
    responses((status = 200, body = Instance))
)]
async fn instance(State(state): State<AppState>) -> Json<Instance> {
    Json(Instance {
        single_org: state.config.single_org,
        mail: state.mailer.is_on(),
        playgrounds: state.config.playground.is_some(),
        billing: state.billing.is_on(),
        source: InstanceSource {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            commit: crate::config::COMMIT.map(str::to_owned),
            url: state.config.source_at_commit(),
        },
        legal: state.config.legal.as_ref().map(LegalLinks::of),
    })
}
