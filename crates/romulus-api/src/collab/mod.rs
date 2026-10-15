//! The collab server: one in-memory Y.Doc (yrs) per open draft, the y-sync
//! protocol over a websocket, persistence to `draft_updates` /
//! `draft_snapshots`, and awareness stamped with the authenticated user.
//!
//! Planned: `GET …/drafts/{id}/ws` (upgrade). Rules from docs/MVP.md: check
//! `Origin` against `Config::public_origin` on upgrade (the CSRF middleware
//! only guards unsafe methods); drop document updates from reviewers but keep
//! their awareness; close a member's sockets on role change or removal. Graceful
//! shutdown waits for open connections, so close sockets on shutdown.

use utoipa_axum::router::OpenApiRouter;

use crate::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
