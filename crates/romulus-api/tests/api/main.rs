//! API integration tests: one binary, one module per feature.
//!
//! Each `#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]` gets a fresh,
//! migrated database on the server `DATABASE_URL` points at (`.env` in dev, a
//! service container in CI), dropped when the test passes.

mod approvals;
mod auth;
mod collab;
mod comment_attacks;
mod comments;
mod csrf;
mod databases;
mod diff;
mod draft_modes;
mod drafts;
mod harness;
mod health;
mod idp;
mod imports;
mod invitations;
mod live;
mod majors;
mod members;
mod migration;
mod oidc;
mod orgs;
mod passwords;
mod playgrounds;
mod push;
mod removal;
mod schema;
mod tidy;
mod timeline;
mod tokens;
mod versions;
mod web;
