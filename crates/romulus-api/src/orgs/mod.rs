//! Organizations, memberships, roles and copy-link invitations.
//!
//! Planned: `GET/POST /orgs`, `GET/PATCH /orgs/{org}`, `GET /orgs/{org}/members`,
//! `PATCH/DELETE /orgs/{org}/members/{user}`, `POST /orgs/{org}/invitations`,
//! `POST /invitations/{token}/accept`. Tables: `organizations`, `memberships`,
//! `invitations`. The last-admin rule is a database trigger: a refused change
//! surfaces as `ApiError::Conflict { code: "last_admin" }` through `?`.
//! A role change or removal must close that member's websockets (`collab`).

use utoipa_axum::router::OpenApiRouter;

use crate::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
