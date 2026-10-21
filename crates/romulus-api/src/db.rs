use std::time::Duration;

use sqlx::{
    PgConnection, PgPool, Postgres, Transaction,
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

/// A transaction at READ COMMITTED, whatever the server's default: the one
/// membership changes (roles, removals, joins) run in. There the last-admin
/// trigger makes a concurrent change wait and recount; at a higher level it
/// would abort with 40001 instead (see `memberships_keep_an_admin`).
pub async fn begin_read_committed(
    db: &PgPool,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    db.begin_with("BEGIN ISOLATION LEVEL READ COMMITTED").await
}

/// Takes the session-level advisory lock `name` (`hashtext` of it) on a
/// connection of its own, out of the pool: the lock is the session's and
/// goes with the connection whatever happens (close it to release). `None`
/// when another session holds it. For work one process does at a time
/// (sweeps).
pub async fn try_advisory_lock(
    db: &PgPool,
    name: &str,
) -> Result<Option<PgConnection>, sqlx::Error> {
    let mut conn = db.acquire().await?.detach();
    let locked = sqlx::query_scalar!(
        r#"SELECT pg_try_advisory_lock(hashtext($1)) AS "locked!""#,
        name
    )
    .fetch_one(&mut conn)
    .await?;
    Ok(locked.then_some(conn))
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
