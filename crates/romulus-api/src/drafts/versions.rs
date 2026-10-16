//! What drafts read of versions, and how a draft becomes one (through the
//! versions module's [`store`], which numbers versions under the lock on the
//! database row and checks the expected base there).

use serde::Deserialize as _;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    ApiError, ApiResult,
    domain::{Schema, SourceKind, VersionOrigin},
    versions::{
        layout::VersionLayout,
        store::{NewVersion, StoreError, Stored, store},
    },
};

/// A version, as a draft starts from it.
pub struct Version {
    pub id: Uuid,
    pub number: i32,
    pub model: Value,
    pub source: String,
    pub source_kind: SourceKind,
    pub layout: Value,
}

impl Version {
    pub fn schema(&self) -> ApiResult<Schema> {
        Schema::deserialize(&self.model).map_err(ApiError::internal)
    }

    /// Its SQL as a draft has it: the source, or `render_sql` of a pushed
    /// model (or of an imported one: a migration is a change, not the
    /// schema).
    pub fn sql(&self) -> ApiResult<String> {
        Ok(match self.source_kind {
            SourceKind::Sql => self.source.clone(),
            SourceKind::RemusJson | SourceKind::Migration => {
                romulus_sandbox::render_sql(&self.schema()?)
            }
        })
    }
}

/// One version of the database, removed or not (a draft's base may be).
pub async fn by_id(db: &PgPool, database_id: Uuid, id: Uuid) -> ApiResult<Option<Version>> {
    Ok(sqlx::query_as!(
        Version,
        r#"SELECT id, number, model, source, source_kind AS "source_kind: SourceKind", layout
           FROM versions WHERE database_id = $1 AND id = $2"#,
        database_id,
        id,
    )
    .fetch_optional(db)
    .await?)
}

/// The database's latest live version, if it has one.
pub async fn latest(db: &PgPool, database_id: Uuid) -> ApiResult<Option<Version>> {
    Ok(sqlx::query_as!(
        Version,
        r#"SELECT id, number, model, source, source_kind AS "source_kind: SourceKind", layout
           FROM versions WHERE database_id = $1 AND removed_at IS NULL ORDER BY number DESC LIMIT 1"#,
        database_id,
    )
    .fetch_optional(db)
    .await?)
}

/// A version a draft becomes.
pub struct DraftVersion {
    pub database_id: Uuid,
    /// Marked published in the same transaction as the version is stored.
    pub draft: Uuid,
    /// The version the draft is based on: creating fails unless it is
    /// still the latest (`None`: the database has none).
    pub expected_base: Option<Uuid>,
    /// Canonical, its `database` set.
    pub model: Schema,
    /// SQL that compiles to `model`: the draft's text, or (migration mode)
    /// the derived schema rendered.
    pub source: String,
    /// Migration mode: the migration, stored with the version.
    pub migration_sql: Option<String>,
    pub layout: VersionLayout,
    pub message: Option<String>,
    pub created_by: Uuid,
    /// The approvals to check again and keep with the version (when the
    /// organization requires some).
    pub approvals: Option<super::approvals::Freeze>,
}

pub struct Created {
    pub id: Uuid,
    pub number: i32,
}

/// Stores the next version of the database and marks the draft published,
/// in one transaction: unless someone published since the draft started
/// (409 `stale_base`), or the draft was deleted meanwhile (409
/// `draft_not_open`).
pub async fn create_version(db: &PgPool, new: DraftVersion) -> ApiResult<Created> {
    let stored = store(
        db,
        NewVersion {
            database_id: new.database_id,
            model: new.model,
            source: new.source,
            source_kind: SourceKind::Sql,
            origin: VersionOrigin::Draft,
            message: new.message,
            created_by: Some(new.created_by),
            created_by_token: None,
            layout: Some(new.layout),
            expected_base: Some(new.expected_base),
            publishes_draft: Some(new.draft),
            approvals: new.approvals,
            migration_sql: new.migration_sql,
        },
        false,
    )
    .await;
    match stored {
        Ok(Stored::Created(version) | Stored::Unchanged(version)) => Ok(Created {
            id: version.id,
            number: version.number,
        }),
        Err(StoreError::StaleBase { latest }) => Err(stale_base(latest.map(|v| v.number))),
        Err(err @ (StoreError::DraftNotOpen | StoreError::Api(_))) => Err(err.into()),
    }
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
