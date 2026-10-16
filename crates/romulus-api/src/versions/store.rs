//! Creating a version: the number, the layout, and the unchanged check, under
//! a lock on the database row. "The latest version" is the latest live one
//! (removed versions are compared with nothing and give no layout); the
//! number follows the highest ever given (`databases.last_version_number`).

use std::sync::Arc;

use remus_core::Schema;
use serde::Deserialize;
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use super::{VersionSummary, layout::VersionLayout, summary};
use crate::{
    ApiError, audit,
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
    /// Recorded in the same transaction, with the new version's number (a
    /// draft published without its approvals).
    pub audit: Option<audit::Entry>,
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
/// Numbers come from the highest given (`last_version_number`, which a
/// trigger moves on insert) under `FOR NO KEY UPDATE` on the databases row:
/// concurrent creations for one database queue there (and nothing else
/// waits: inserting a token or a version's foreign key only takes `KEY
/// SHARE`), so numbers are never given twice, a removed version's included;
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
    let last_number = sqlx::query_scalar!(
        "SELECT last_version_number FROM databases WHERE id = $1 AND deleted_at IS NULL FOR NO KEY UPDATE",
        database_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(no_such_database)?;
    let current = sqlx::query_scalar!(
        "SELECT id FROM versions WHERE database_id = $1 AND removed_at IS NULL
         ORDER BY number DESC LIMIT 1",
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

    let number = last_number + 1;
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
    if let Some(entry) = &new.audit {
        let entry = audit::Entry {
            version_number: Some(number),
            ..entry.clone()
        };
        audit::record(&mut *tx, &entry).await?;
    }
    tx.commit().await?;
    tracing::info!(database = %database_id, number, origin = ?new.origin, "version created");
    Ok(Stored::Created(summary(db, id).await?))
}

/// The latest live version of a database, whole.
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
         WHERE database_id = $1 AND removed_at IS NULL ORDER BY number DESC LIMIT 1",
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

/// One version of an import, about to be stored.
pub struct ImportedVersion {
    /// The migration's up SQL.
    pub source: String,
    /// The file (or folder) name.
    pub message: String,
    pub migration_id: String,
    pub path: String,
    /// sha256 of `source`.
    pub checksum: Vec<u8>,
    /// What was not replayed (`imports::SkippedStatement[]`).
    pub skipped: serde_json::Value,
}

/// An import's versions, stored together or not at all. Their models and
/// layouts are in `import_staging` under `staging`, one row per version in
/// order (`imports::run` writes them as the replay reads them).
pub struct NewImport {
    pub database_id: Uuid,
    pub format: crate::domain::MigrationFormat,
    pub created_by: Option<Uuid>,
    pub created_by_token: Option<Uuid>,
    /// Versions that were not imported may precede these.
    pub append: bool,
    /// How many migrations the database had imported (live ones) when the
    /// import was planned: another import storing meanwhile makes this one
    /// stale.
    pub imported_before: usize,
    /// The highest number given when the import was planned
    /// (`last_version_number`): another version created meanwhile makes it
    /// stale (the first version was compared with the latest then, and
    /// inherits its layout).
    pub last_number: i32,
    pub staging: Uuid,
    pub versions: Vec<ImportedVersion>,
}

/// Stores an import's versions in one transaction, numbered after the
/// latest version under the same lock as [`store`], their staged models
/// and layouts moved in (and the staging rows deleted) without passing
/// through the API. Refuses with 409 `concurrent_import` when another
/// import stored migrations, or another version was created, since this
/// one was planned, and `has_versions` when versions that were not
/// imported exist and `append` is false. Returns each version's (id,
/// number).
#[allow(clippy::too_many_lines)] // one transaction, query after query
pub async fn store_import(db: &PgPool, new: NewImport) -> Result<Vec<(Uuid, i32)>, StoreError> {
    let NewImport {
        database_id,
        format,
        created_by,
        created_by_token,
        append,
        imported_before,
        last_number: planned_last,
        staging,
        versions,
    } = new;

    let mut tx = db.begin().await?;
    let last_number = sqlx::query_scalar!(
        "SELECT last_version_number FROM databases WHERE id = $1 AND deleted_at IS NULL FOR NO KEY UPDATE",
        database_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(no_such_database)?;
    let imported = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM version_migrations WHERE database_id = $1 AND NOT removed"#,
        database_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    if usize::try_from(imported).unwrap_or(usize::MAX) != imported_before {
        return Err(StoreError::Api(ApiError::Conflict {
            code: "concurrent_import",
            message: "Another import of this database finished meanwhile. Import again: what it added is skipped."
                .into(),
        }));
    }
    if !append {
        let others = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM versions WHERE database_id = $1 AND origin <> 'import' AND removed_at IS NULL
               ) AS "exists!""#,
            database_id,
        )
        .fetch_one(&mut *tx)
        .await?;
        if others {
            return Err(StoreError::Api(ApiError::Conflict {
                code: "has_versions",
                message: "This database has versions that were not imported. Import with append=true to add the migrations after them."
                    .into(),
            }));
        }
    }
    if last_number != planned_last {
        return Err(StoreError::Api(ApiError::Conflict {
            code: "concurrent_import",
            message: "A version of this database was created during the import. Import again: nothing was stored."
                .into(),
        }));
    }
    let mut number = last_number;

    let mut stored = Vec::with_capacity(versions.len());
    for (k, version) in versions.into_iter().enumerate() {
        number += 1;
        let message: String = version.message.chars().take(1000).collect();
        let id = sqlx::query_scalar!(
            "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout,
                                   message, created_by, created_by_token)
             SELECT $1, $2, s.model, $3, 'migration', 'import', s.layout, $4, $5, $6
             FROM import_staging s
             WHERE s.import_id = $7 AND s.ordinal = $8
             RETURNING id",
            database_id,
            number,
            version.source,
            message.trim(),
            created_by,
            created_by_token,
            staging,
            i32::try_from(k).map_err(ApiError::internal)?,
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::Internal(format!("no staged model for version {k}").into()))?;
        let inserted = sqlx::query!(
            "INSERT INTO version_migrations (version_id, database_id, migration_id, path, format, checksum, skipped)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            id,
            database_id,
            version.migration_id,
            version.path,
            format as crate::domain::MigrationFormat,
            version.checksum,
            version.skipped,
        )
        .execute(&mut *tx)
        .await;
        if let Err(err) = inserted {
            if crate::db::is_unique_violation(&err) {
                return Err(StoreError::Api(ApiError::Conflict {
                    code: "concurrent_import",
                    message: "Another import of this database stored these migrations meanwhile."
                        .into(),
                }));
            }
            return Err(err.into());
        }
        stored.push((id, number));
    }
    sqlx::query!("DELETE FROM import_staging WHERE import_id = $1", staging)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(database = %database_id, versions = stored.len(), last = number, "migrations imported");
    Ok(stored)
}
