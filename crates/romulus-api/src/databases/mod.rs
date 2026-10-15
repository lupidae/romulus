//! Databases inside an organization, and their API tokens.
//!
//! Planned: `GET/POST /orgs/{org}/databases`, `GET/PATCH/DELETE
//! /orgs/{org}/databases/{db}` (soft delete: `deleted_at`), and
//! `GET/POST/DELETE …/tokens` (scopes `push` | `full`, shown once, stored as
//! sha256). Tables: `databases`, `api_tokens`.

use utoipa_axum::router::OpenApiRouter;

use crate::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
