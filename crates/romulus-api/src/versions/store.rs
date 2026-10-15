//! Creating a version: the number, the layout, and the unchanged check, under
//! a lock on the database row.

use std::sync::Arc;

use remus_core::Schema;
use serde::Deserialize;
use sqlx::{PgExecutor, PgPool};
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
    /// The draft this version publishes: marked `published` in the same
    /// transaction, and nothing is stored unless it is still open
    /// ([`StoreError::DraftNotOpen`]).
    pub publishes_draft: Option<Uuid>,
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
    /// The draft being published was deleted or published meanwhile.
    #[error("the draft is no longer open")]
    DraftNotOpen,
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
            StoreError::DraftNotOpen => ApiError::Conflict {
                code: "draft_not_open",
                message: "This draft was deleted or published meanwhile.".into(),
            },
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
/// `UNIQUE (database_id, number)` is the backstop.
///
/// The costly part (reading the previous model, the diff, serialising) runs
/// off the async workers and before the lock, against the latest version as
/// read then. Under the lock only that version's id is checked again; when
/// another version was stored meanwhile, the comparison is redone against
/// it, under the lock. So two identical pushes at once still create one
/// version, and the lock is held for milliseconds in the common case.
/// A new version starts with the previous one's layout unless given one.
pub async fn store(
    db: &PgPool,
    new: NewVersion,
    skip_unchanged: bool,
) -> Result<Stored, StoreError> {
    let new = Arc::new(new);
    let database_id = new.database_id;
    let model = blocking({
        let new = Arc::clone(&new);
        move || serde_json::to_value(&new.model).map_err(ApiError::internal)
    })
    .await??;
    let mut compared = compare(&new, latest(db, database_id).await?, skip_unchanged).await?;

    let mut tx = db.begin().await?;
    sqlx::query_scalar!(
        "SELECT id FROM databases WHERE id = $1 AND deleted_at IS NULL FOR NO KEY UPDATE",
        database_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(no_such_database)?;
    let current = sqlx::query_scalar!(
        "SELECT id FROM versions WHERE database_id = $1 ORDER BY number DESC LIMIT 1",
        database_id,
    )
    .fetch_optional(&mut *tx)
    .await?;
    if current != compared.base.map(|(id, _)| id) {
        // Someone stored a version since: compare with that one (rare).
        compared = compare(&new, latest(&mut *tx, database_id).await?, skip_unchanged).await?;
    }
    let base = compared.base;

    if let Some(expected) = new.expected_base
        && base.map(|(id, _)| id) != expected
    {
        tx.rollback().await?;
        let latest = match base {
            Some((id, _)) => Some(summary(db, id).await?),
            None => None,
        };
        return Err(StoreError::StaleBase { latest });
    }
    if let (true, Some((id, _))) = (compared.unchanged, base) {
        tx.rollback().await?;
        return Ok(Stored::Unchanged(summary(db, id).await?));
    }

    let number = base.map_or(1, |(_, number)| number + 1);
    let id = sqlx::query_scalar!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout,
                               message, created_by, created_by_token)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
        database_id,
        number,
        model,
        new.source,
        new.source_kind as SourceKind,
        new.origin as VersionOrigin,
        compared.layout,
        new.message,
        new.created_by,
        new.created_by_token,
    )
    .fetch_one(&mut *tx)
    .await?;
    if let Some(draft) = new.publishes_draft {
        let marked = sqlx::query!(
            "UPDATE drafts SET status = 'published', published_version_id = $2
             WHERE id = $1 AND database_id = $3 AND status = 'open'",
            draft,
            id,
            database_id,
        )
        .execute(&mut *tx)
        .await?;
        if marked.rows_affected() != 1 {
            tx.rollback().await?;
            return Err(StoreError::DraftNotOpen);
        }
    }
    tx.commit().await?;
    tracing::info!(database = %database_id, number, origin = ?new.origin, "version created");
    Ok(Stored::Created(summary(db, id).await?))
}

/// The latest version of a database, whole.
struct Latest {
    id: Uuid,
    number: i32,
    model: serde_json::Value,
    layout: serde_json::Value,
}

async fn latest(db: impl PgExecutor<'_>, database_id: Uuid) -> Result<Option<Latest>, sqlx::Error> {
    sqlx::query_as!(
        Latest,
        "SELECT id, number, model, layout FROM versions
         WHERE database_id = $1 ORDER BY number DESC LIMIT 1",
        database_id,
    )
    .fetch_optional(db)
    .await
}

/// A new version, measured against the latest one.
struct Compared {
    /// The latest version compared with: (id, number).
    base: Option<(Uuid, i32)>,
    /// The model equals the base's (only computed when asked).
    unchanged: bool,
    /// The layout to store, pruned to the new model.
    layout: serde_json::Value,
}

/// Off the async workers: parsing a stored model and diffing two can take a
/// while for a few hundred tables.
async fn compare(
    new: &Arc<NewVersion>,
    latest: Option<Latest>,
    skip_unchanged: bool,
) -> Result<Compared, StoreError> {
    let new = Arc::clone(new);
    let compared = blocking(move || {
        let base = latest.as_ref().map(|latest| (latest.id, latest.number));
        // A stored model that no longer parses (an older remus) counts as changed.
        let unchanged = skip_unchanged
            && latest.as_ref().is_some_and(|latest| {
                Schema::deserialize(&latest.model)
                    .is_ok_and(|previous| romulus_diff::diff(&previous, &new.model).is_empty())
            });
        let layout = match &new.layout {
            Some(layout) => layout.clone(),
            None => latest
                .map(|latest| VersionLayout::from_value(&latest.layout))
                .unwrap_or_default(),
        }
        .pruned(&new.model);
        let layout = serde_json::to_value(&layout).map_err(ApiError::internal)?;
        Ok::<_, ApiError>(Compared {
            base,
            unchanged,
            layout,
        })
    })
    .await??;
    Ok(compared)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(ApiError::internal)
}
