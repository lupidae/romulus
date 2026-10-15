use std::time::Duration;

use sqlx::{
    PgPool,
    migrate::Migrator,
    postgres::{PgDatabaseError, PgPoolOptions},
};

/// Every migration in `migrations/` at the repository root, embedded at build
/// time. Tests use it as `#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]`.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Raised by the `memberships_keep_an_admin` trigger when a change would leave
/// an organization without an admin. [`crate::ApiError`] turns it into a 409.
pub const ORGANIZATION_NEEDS_AN_ADMIN: &str = "organization_needs_an_admin";

pub async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(url)
        .await
}

/// The constraint a database error names, if any: unique and foreign-key
/// violations, check constraints, and our triggers (which raise with a
/// `CONSTRAINT` so callers can match on a stable name instead of a message).
pub fn constraint(err: &sqlx::Error) -> Option<&str> {
    err.as_database_error()?
        .try_downcast_ref::<PgDatabaseError>()?
        .constraint()
}

/// Whether `err` is a unique violation, e.g. to answer 409 "slug taken".
pub fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
}
