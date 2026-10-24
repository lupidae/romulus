//! API integration tests: one binary, one module per feature.
//!
//! Each `#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]` gets a fresh,
//! migrated database on the server `DATABASE_URL` points at (`.env` in dev, a
//! service container in CI), dropped when the test passes.

mod access_tokens;
mod approvals;
mod auth;
mod billing;
mod cli;
mod collab;
mod comment_attacks;
mod comments;
mod csrf;
mod databases;
mod diff;
mod draft_modes;
mod draft_text;
mod drafts;
mod export;
mod harness;
mod health;
mod idp;
mod imports;
mod invitations;
mod legal;
mod live;
mod majors;
mod mcp;
mod mcp_files;
mod members;
mod migration;
mod migration_tests;
mod oauth;
mod oidc;
mod operator;
mod orgs;
mod passwords;
mod playgrounds;
mod push;
mod removal;
mod renames;
mod retention;
mod schema;
mod signups;
mod tidy;
mod timeline;
mod tokens;
mod versions;
mod web;
mod withdrawal;
