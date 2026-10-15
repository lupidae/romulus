//! Drafts: one-click creation from the latest version, rename, delete,
//! publish (freeze, compile, store as the next version) and "Update draft"
//! (statement-level three-way merge when someone published first).
//!
//! Planned: `GET/POST …/databases/{db}/drafts`, `PATCH/DELETE …/drafts/{id}`,
//! `POST …/drafts/{id}/publish`, `POST …/drafts/{id}/update`. Tables: `drafts`,
//! `draft_compiles`. The live content is the Y.Doc owned by `collab`.

use utoipa_axum::router::OpenApiRouter;

use crate::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
