//! Creating a version: the number, the layout, and the unchanged check, under
//! a lock on the database row.

use remus_core::Schema;
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::{VersionSummary, layout::VersionLayout, summary};
use crate::{
    ApiError,
    databases::no_such_database,
    domain::{SourceKind, VersionOrigin},
};

/// A version about to be stored.
pub struct NewVersion {
    pub database_id: Uuid,
    /// Canonical, with its `database` set.
    pub model: Schema,
    /// What was uploaded or pushed, verbatim.
    pub source: String,
    pub source_kind: SourceKind,
    pub origin: VersionOrigin,
    pub message: Option<String>,
    pub created_by: Option<Uuid>,
    pub created_by_token: Option<Uuid>,
    /// `None`: the previous version's layout (empty for the first). `Some`:
    /// this one (a draft publishes its layout and groups). Either way it is
    /// stored pruned to `model`.
    pub layout: Option<VersionLayout>,
    /// `Some(base)`: only if the latest version is still `base` (`None`:
    /// still no version), checked under the same lock as the number;
    /// otherwise [`StoreError::StaleBase`]. A draft publishes this way, so
    /// it never silently overwrites a version published meanwhile.
    pub expected_base: Option<Option<Uuid>>,
}

/// Why [`store`] stored nothing.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The latest version is no longer `expected_base`.
    #[error("the base version is no longer the latest")]
    StaleBase {
        /// The latest version now.
        latest: Option<VersionSummary>,
    },
    #[error(transparent)]
    Api(#[from] ApiError),
}

impl From<sqlx::Error> for StoreError {
    fn from(err: sqlx::Error) -> Self {
        Self::Api(err.into())
    }
}

/// 409 `stale_base`, `details.latest` the version that came out meanwhile.
impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::Api(err) => err,
            StoreError::StaleBase { latest } => ApiError::Conflict {
                code: STALE_BASE,
                message: match &latest {
                    Some(latest) => format!(
                        "v{} came out meanwhile. Update the draft, then publish again.",
                        latest.number
                    ),
                    None => "The database's versions changed meanwhile.".to_owned(),
                },
            },
        }
    }
}

/// The 409's code when the base moved.
pub const STALE_BASE: &str = "stale_base";

#[derive(Debug)]
pub enum Stored {
    Created(VersionSummary),
    /// The model equals the latest version's: nothing was stored, here is
    /// that latest version.
    Unchanged(VersionSummary),
}

/// Stores `new` as the database's next version. With `skip_unchanged`
/// (pushes), a model romulus-diff finds no difference in from the latest
/// version's creates nothing.
///
/// Numbers come from the latest version under `FOR NO KEY UPDATE` on the
/// databases row: concurrent creations for one database queue there (and
/// nothing else waits: inserting a token or a version's foreign key only
/// takes `KEY SHARE`), so numbers have no gaps and no duplicates;
/// `UNIQUE (database_id, number)` is the backstop. The unchanged check runs
/// under the same lock, so two identical pushes at once create one version.
/// A new version starts with the previous one's layout unless given one.
pub async fn store(
    db: &PgPool,
    new: NewVersion,
    skip_unchanged: bool,
) -> Result<Stored, StoreError> {
    let mut tx = db.begin().await?;
    sqlx::query_scalar!(
        "SELECT id FROM databases WHERE id = $1 AND deleted_at IS NULL FOR NO KEY UPDATE",
        new.database_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(no_such_database)?;
    let latest = sqlx::query!(
        "SELECT id, number, model, layout FROM versions
         WHERE database_id = $1 ORDER BY number DESC LIMIT 1",
        new.database_id,
    )
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(expected) = new.expected_base
        && latest.as_ref().map(|latest| latest.id) != expected
    {
        tx.rollback().await?;
        let latest = match latest {
            Some(latest) => Some(summary(db, latest.id).await?),
            None => None,
        };
        return Err(StoreError::StaleBase { latest });
    }

    if skip_unchanged && let Some(latest) = &latest {
        // A stored model that no longer parses (an older remus) counts as changed.
        let unchanged = Schema::deserialize(&latest.model)
            .is_ok_and(|previous| romulus_diff::diff(&previous, &new.model).is_empty());
        if unchanged {
            tx.rollback().await?;
            return Ok(Stored::Unchanged(summary(db, latest.id).await?));
        }
    }

    let number = latest.as_ref().map_or(1, |latest| latest.number + 1);
    let layout = match new.layout {
        Some(layout) => layout,
        None => latest
            .map(|latest| VersionLayout::from_value(&latest.layout))
            .unwrap_or_default(),
    }
    .pruned(&new.model);
    let layout = serde_json::to_value(&layout).map_err(ApiError::internal)?;
    let model = serde_json::to_value(&new.model).map_err(ApiError::internal)?;
    let id = sqlx::query_scalar!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout,
                               message, created_by, created_by_token)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
        new.database_id,
        number,
        model,
        new.source,
        new.source_kind as SourceKind,
        new.origin as VersionOrigin,
        layout,
        new.message,
        new.created_by,
        new.created_by_token,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(database = %new.database_id, number, origin = ?new.origin, "version created");
    Ok(Stored::Created(summary(db, id).await?))
}
