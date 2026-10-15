//! Compiling draft SQL into a `remus_core::Schema` on the sandbox Postgres
//! (`Config::sandbox_url`, Postgres 17 to match `pg_query`'s `libpg_query`):
//! DDL allowlist with `pg_query`, auto-stubs for roles/functions/extensions,
//! a pool of pre-created databases, apply in a transaction, run remus's
//! introspection query, roll back. Debounced and coalesced per draft; the
//! last result is cached in `draft_compiles`.
//!
//! Mostly internal (called by `versions` and `collab`); may expose
//! `POST /compile` for uploads' dry runs.

use utoipa_axum::router::OpenApiRouter;

use crate::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
