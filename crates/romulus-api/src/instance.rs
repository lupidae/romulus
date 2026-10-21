//! `GET /api/instance`: how this server is set up, for the app to show only
//! what works here. Public: it says nothing about anyone.

use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{AppState, extract::Json};

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
    })
}
