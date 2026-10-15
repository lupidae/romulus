//! Immutable schema versions: upload SQL (compiled through `compile`), the
//! remus push endpoint (bearer token, remus JSON, `200 unchanged` when the
//! canonical model equals the latest), list and read, and diffs between two
//! versions (`romulus-diff`).
//!
//! Planned: `GET/POST …/databases/{db}/versions`, `GET …/versions/{n}`,
//! `GET …/diff?from=&to=`. Table: `versions` (immutable by trigger; numbers
//! assigned under a lock on the `databases` row).

use utoipa_axum::router::OpenApiRouter;

use crate::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
