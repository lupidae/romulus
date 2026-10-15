//! The narrow seam drafts use into databases and versions.
//!
//! TODO(M2 rebase): the databases and versions modules land with milestone
//! 2. Then [`database`] becomes `databases::find`, [`latest`] a query of
//! the versions module, and [`create_version`] `versions::store::store`
//! with the draft's layout and the expected base (`NewVersion.layout`,
//! `NewVersion.expected_base`, answering 409 `stale_base`). Until then they
//! are the minimal queries below, under the same rules: live databases
//! only, numbers assigned under `FOR NO KEY UPDATE` on the databases row.

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    ApiError, ApiResult,
    domain::{Schema, SourceKind, VersionOrigin},
};

/// The database a request is about.
#[derive(Debug, Clone)]
pub struct DatabaseRef {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
}

/// The live database `slug` of the organization `org_id`, or 404.
pub async fn database(db: &PgPool, org_id: Uuid, slug: &str) -> ApiResult<DatabaseRef> {
    sqlx::query_as!(
        DatabaseRef,
        "SELECT id, slug, name FROM databases WHERE org_id = $1 AND slug = $2 AND deleted_at IS NULL",
        org_id,
        slug,
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| ApiError::not_found("No such database. It may have been deleted or renamed."))
}

/// A version, as a draft starts from it.
pub struct Version {
    pub id: Uuid,
    pub number: i32,
    pub model: Value,
    pub source: String,
    pub source_kind: SourceKind,
    pub layout: Value,
}

/// The database's latest version, if it has one.
pub async fn latest(db: &PgPool, database_id: Uuid) -> ApiResult<Option<Version>> {
    Ok(sqlx::query_as!(
        Version,
        r#"SELECT id, number, model, source, source_kind AS "source_kind: SourceKind", layout
           FROM versions WHERE database_id = $1 ORDER BY number DESC LIMIT 1"#,
        database_id,
    )
    .fetch_optional(db)
    .await?)
}

/// The number of a version.
pub async fn number(db: &PgPool, version_id: Uuid) -> ApiResult<i32> {
    Ok(
        sqlx::query_scalar!("SELECT number FROM versions WHERE id = $1", version_id)
            .fetch_one(db)
            .await?,
    )
}

/// A version a draft becomes.
pub struct DraftVersion<'a> {
    pub database_id: Uuid,
    /// The version the draft is based on: creating fails unless it is
    /// still the latest (`None`: the database has none).
    pub expected_base: Option<Uuid>,
    /// Canonical, its `database` set.
    pub model: &'a Schema,
    pub source: &'a str,
    pub layout: Value,
    pub message: Option<&'a str>,
    pub created_by: Uuid,
}

pub struct Created {
    pub id: Uuid,
    pub number: i32,
}

/// Stores the next version of the database inside `tx`, unless someone
/// published first: 409 `stale_base`.
pub async fn create_version(
    tx: &mut sqlx::PgConnection,
    new: DraftVersion<'_>,
) -> ApiResult<Created> {
    sqlx::query_scalar!(
        "SELECT id FROM databases WHERE id = $1 AND deleted_at IS NULL FOR NO KEY UPDATE",
        new.database_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::not_found("No such database. It may have been deleted."))?;
    let latest = sqlx::query!(
        "SELECT id, number FROM versions WHERE database_id = $1 ORDER BY number DESC LIMIT 1",
        new.database_id,
    )
    .fetch_optional(&mut *tx)
    .await?;
    if latest.as_ref().map(|v| v.id) != new.expected_base {
        return Err(stale_base(latest.map(|v| v.number)));
    }
    let number = latest.map_or(1, |v| v.number + 1);
    let model = serde_json::to_value(new.model).map_err(ApiError::internal)?;
    let id = sqlx::query_scalar!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout,
                               message, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
        new.database_id,
        number,
        model,
        new.source,
        SourceKind::Sql as SourceKind,
        VersionOrigin::Draft as VersionOrigin,
        new.layout,
        new.message,
        new.created_by,
    )
    .fetch_one(&mut *tx)
    .await?;
    Ok(Created { id, number })
}

/// 409 `stale_base`: someone published since the draft started.
pub fn stale_base(latest: Option<i32>) -> ApiError {
    let message = match latest {
        Some(n) => format!(
            "v{n} was published since this draft started. Update the draft to v{n} before publishing."
        ),
        None => "The database's versions changed since this draft started.".to_owned(),
    };
    ApiError::Conflict {
        code: "stale_base",
        message,
    }
}
