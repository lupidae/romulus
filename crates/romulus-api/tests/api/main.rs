//! API integration tests: one binary, one module per feature.
//!
//! Each `#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]` gets a fresh,
//! migrated database on the server `DATABASE_URL` points at (`.env` in dev, a
//! service container in CI), dropped when the test passes.

mod auth;
mod csrf;
mod databases;
mod harness;
mod health;
mod idp;
mod invitations;
mod members;
mod oidc;
mod orgs;
mod push;
mod schema;
mod tokens;
mod versions;
