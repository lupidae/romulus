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

/// Raised by the `versions_are_immutable` / `versions_are_permanent` triggers:
/// a version is never updated, and only deleted with its database.
pub const VERSIONS_ARE_IMMUTABLE: &str = "versions_are_immutable";
pub const VERSIONS_ARE_PERMANENT: &str = "versions_are_permanent";

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

/// Whether `err` is a serialization failure (SQLSTATE 40001): under REPEATABLE
/// READ or SERIALIZABLE, Postgres aborted this transaction because a concurrent
/// one changed what it read (the last-admin trigger relies on it). Retrying the
/// whole transaction is the fix; [`crate::ApiError`] answers 409
/// `concurrent_change` otherwise.
pub fn is_serialization_failure(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "40001")
}

/// Whether `err` is a unique violation, e.g. to answer 409 "slug taken".
pub fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
}
