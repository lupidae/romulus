//! API integration tests: one binary, one module per feature.
//!
//! Each `#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]` gets a fresh,
//! migrated database on the server `DATABASE_URL` points at (`.env` in dev, a
//! service container in CI), dropped when the test passes.

mod auth;
mod collab;
mod csrf;
mod databases;
mod diff;
mod drafts;
mod harness;
mod health;
mod idp;
mod imports;
mod invitations;
mod live;
mod members;
mod migration;
mod oidc;
mod orgs;
mod push;
mod schema;
mod tidy;
mod tokens;
mod versions;
mod web;
