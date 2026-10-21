//! A migration between two versions: `GET …/databases/{db}/migration?from=&to=&format=`
//! [member]. The `ALTER …` statements that turn a database at `from` into
//! one at `to` (`romulus-migration`), destructive ones flagged, and the
//! files a migration tool expects (sqlx, Flyway, Prisma, dbmate,
//! golang-migrate or plain SQL), with a best-effort down migration where the
//! tool has one. A draft's migration ([`crate::drafts::migration`]) shares
//! [`SchemaMigration`] and [`build`].
//!
//! Renames: the versions in the range keep the renames they were
//! published with (`versions.renames`, see [`stored_renames`]); they are
//! known, and the renames detected beyond them are on or off as decided then.
//! `renames` (a JSON object of `romulus_diff::Rename::key` → on or off)
//! turns detected ones on or off over those. The response lists every
//! candidate ([`SchemaMigration::renames`]) for the person to choose.
//!
//! Nothing is cached: two models in, a few milliseconds of planning out.
//! The file names carry the time of the request (UTC), as the tools' own
//! `migrate add` would.

use std::collections::BTreeMap;

use axum::extract::State;
use chrono::Utc;
use remus_core::Schema;
use romulus_migration::{
    Candidate, File, Format, Meta, Options, RenameChoices, Statement, Warning, archive_name, files,
    migration, slug,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::detail;
use crate::{
    ApiError, ApiResult, AppState,
    auth::Member,
    databases,
    domain::PgMajor,
    error::ErrorBody,
    extract::{Json, Path, Query},
    orgs::invalid,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(version_migration))
}

/// How to lay the migration out, for both endpoints.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MigrationOptions {
    /// The migration tool: its file names, transactions and down migration.
    /// `plain` when absent.
    pub format: Option<Format>,
    /// Build indexes on existing tables `CONCURRENTLY`, outside the
    /// transaction (their own files where the tool needs it).
    #[serde(default)]
    pub concurrently: bool,
    /// The description in file names (made file-name safe); by default the
    /// target version's message, or `v4_to_v5`.
    pub name: Option<String>,
    /// The first file's version, where the tool numbers its migrations:
    /// Flyway's `V<version>` (digits and dots; by default the target
    /// version's number), or the number other tools put first (digits: a
    /// golang-migrate sequence like `000042`; by default the time,
    /// `YYYYMMDDHHMMSS`). Files after it take the next ones.
    pub version: Option<String>,
    /// Renames turned on or off, over what is stored (the draft's choices,
    /// the versions'): a JSON object of rename keys
    /// (`SchemaMigration.renames[].key`) to `true` or `false`, like
    /// `{"column/public/users/email/email_address":false}`.
    pub renames: Option<String>,
}

impl MigrationOptions {
    /// The decisions in `renames`, or 400 `bad_request` when it is not a
    /// JSON object of booleans (at most 2000, keys at most 2048 bytes).
    pub(crate) fn rename_decisions(&self) -> ApiResult<Option<BTreeMap<String, bool>>> {
        let Some(text) = self.renames.as_deref().filter(|t| !t.trim().is_empty()) else {
            return Ok(None);
        };
        let decisions: BTreeMap<String, bool> = serde_json::from_str(text).map_err(|_| {
            ApiError::BadRequest("renames: a JSON object of rename keys to true or false".into())
        })?;
        if decisions.len() > crate::collab::doc::MAX_RENAME_DECISIONS
            || decisions.keys().any(|k| k.len() > 2048)
        {
            return Err(ApiError::BadRequest(
                "renames: too many, or keys too long".into(),
            ));
        }
        Ok(Some(decisions))
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MigrationVersions {
    /// The version the database is at.
    pub from: i32,
    /// The version to migrate it to. May be older than `from` (a rollback).
    pub to: i32,
}

/// A migration, as statements and as the files of one tool.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SchemaMigration {
    /// The version migrated from; `null` for nothing (a draft of a database
    /// without versions).
    pub from: Option<i32>,
    /// The version migrated to (for a draft, the number publishing it would
    /// give).
    pub to: i32,
    pub format: Format,
    /// In order; the same whatever the format.
    pub statements: Vec<Statement>,
    /// What to know before running it (a column that ends up elsewhere, a
    /// possible rename, a type recreated…).
    pub warnings: Vec<Warning>,
    /// How many statements delete data or can fail on existing rows.
    pub destructive: usize,
    /// The files to put in the tool's migrations folder, in order.
    pub files: Vec<File>,
    /// What the files download as together: `<version>_<name>.zip`.
    pub archive: String,
    /// The Postgres major it is written for: the database's.
    pub pg_major: PgMajor,
    /// Every rename considered, known or detected, applied or not: what a
    /// person turns on or off (`renames` in the request, by `key`).
    pub renames: Vec<Candidate>,
}

/// The two sides, for [`build`].
pub(crate) struct Sides<'a> {
    pub old: &'a Schema,
    pub new: &'a Schema,
    pub from: Option<i32>,
    pub to: i32,
    /// `v4`, `v12`, `nothing`.
    pub from_label: String,
    /// `v5`, `draft "Billing"`.
    pub to_label: String,
    /// The default description in file names.
    pub name: String,
    /// `org/db`.
    pub database: String,
    /// The major the database targets: what the migration is planned for.
    pub major: PgMajor,
    /// The renames to plan with.
    pub renames: RenameChoices,
}

/// Plans the migration and lays it out (off the async workers: a few
/// hundred tables take milliseconds, but they are not free).
pub(crate) async fn build(
    sides: Sides<'_>,
    options: MigrationOptions,
) -> ApiResult<SchemaMigration> {
    let format = options.format.unwrap_or(Format::Plain);
    let name = options
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map_or_else(|| slug(&sides.name), slug);
    let now = Utc::now();
    let version = match options
        .version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(version) => checked_version(format, version)?,
        None if format == Format::Flyway => sides.to.to_string(),
        None => now.format("%Y%m%d%H%M%S").to_string(),
    };
    let meta = Meta {
        database: sides.database,
        from: sides.from_label,
        to: sides.to_label,
        date: now.format("%Y-%m-%d").to_string(),
        version,
        name,
    };
    let archive = archive_name(format, &meta);
    let (old, new) = (sides.old.clone(), sides.new.clone());
    let concurrently = options.concurrently;
    let major = Some(sides.major.number());
    let renames = sides.renames;
    let (up, files) = tokio::task::spawn_blocking(move || {
        let up = migration(
            &old,
            &new,
            &Options {
                concurrently,
                major,
                renames,
            },
        );
        let down = if format.has_down() {
            romulus_migration::down(&old, &new, &up)
        } else {
            romulus_migration::Migration::default()
        };
        let files = files(&up, &down, format, &meta);
        (up, files)
    })
    .await
    .map_err(ApiError::internal)?;
    Ok(SchemaMigration {
        from: sides.from,
        to: sides.to,
        format,
        destructive: up.destructive(),
        statements: up.statements,
        warnings: up.warnings,
        files,
        archive,
        pg_major: sides.major,
        renames: up.renames,
    })
}

/// The renames the versions from `from` (excluded) to `to` were published
/// with, in order: what a migration between them goes by, composed
/// version by version (`RenameChoices::compose`: each version's renames a
/// batch, swaps included, before the next version's; its decisions as they
/// read after the later versions' renames). Going back (`to` below
/// `from`), the same renames reversed, last first, batch by batch; their
/// decisions are about the way forward, and do not apply. Removed versions
/// count (an exception to filtering them out): their renames happened
/// between the two all the same.
pub(crate) async fn stored_renames(
    db: &PgPool,
    database_id: Uuid,
    from: i32,
    to: i32,
) -> ApiResult<RenameChoices> {
    let (low, high) = (from.min(to), from.max(to));
    let rows = sqlx::query_scalar!(
        "SELECT renames FROM versions
         WHERE database_id = $1 AND number > $2 AND number <= $3
         ORDER BY number",
        database_id,
        low,
        high,
    )
    .fetch_all(db)
    .await?;
    // `{}` (not published from a draft) reads as nothing known.
    let forward = RenameChoices::compose(
        rows.into_iter()
            .map(|value| serde_json::from_value::<RenameChoices>(value).unwrap_or_default()),
    );
    Ok(if from <= to {
        forward
    } else {
        forward.reversed()
    })
}

/// A version the tool reads as one: digits and dots for Flyway, digits
/// for the others (at most 20).
fn checked_version(format: Format, version: &str) -> ApiResult<String> {
    let valid = if format == Format::Flyway {
        version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            && version.len() <= 40
    } else {
        version.len() <= 20 && version.bytes().all(|b| b.is_ascii_digit())
    };
    if valid {
        Ok(version.to_owned())
    } else if format == Format::Flyway {
        Err(invalid(
            "version",
            "Flyway versions are numbers, optionally dotted: 42 or 1.2.3.",
        ))
    } else {
        Err(invalid(
            "version",
            "Use digits only: a timestamp like 20261016093000 or a sequence like 000042.",
        ))
    }
}

/// The migration from one version to another, as statements and files.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/migration",
    operation_id = "version_migration",
    tag = "versions",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        MigrationVersions,
        MigrationOptions,
    ),
    responses(
        (status = 200, body = SchemaMigration),
        (status = 400, description = "`bad_request`: a number that is not one, an unknown format, `renames` that is not a JSON object of booleans", body = ErrorBody),
        (status = 402, description = "`plan_limit`: a version past the plan's history window (hosted billing)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such database, or no such version", body = ErrorBody),
        (status = 422, description = "`validation`: a number below 1", body = ErrorBody),
    )
)]
async fn version_migration(
    member: Member,
    State(state): State<AppState>,
    Path((org, slug)): Path<(String, String)>,
    Query(versions): Query<MigrationVersions>,
    Query(options): Query<MigrationOptions>,
) -> ApiResult<Json<SchemaMigration>> {
    for (field, number) in [("from", versions.from), ("to", versions.to)] {
        if number < 1 {
            return Err(invalid(field, "Version numbers start at 1."));
        }
    }
    let database = databases::find(&state.db, member.org.id, &slug).await?;
    let from = detail(&state.db, database.id, Some(versions.from)).await?;
    let to = if versions.to == versions.from {
        from.clone()
    } else {
        detail(&state.db, database.id, Some(versions.to)).await?
    };
    for version in [&from, &to] {
        state
            .billing
            .shown(
                &state.db,
                member.org.id,
                database.id,
                (version.id, version.number, version.created_at),
            )
            .await?;
    }
    let old = Schema::deserialize(&from.model).map_err(ApiError::internal)?;
    let new = Schema::deserialize(&to.model).map_err(ApiError::internal)?;
    let mut renames = stored_renames(&state.db, database.id, versions.from, versions.to).await?;
    if let Some(decisions) = options.rename_decisions()? {
        renames.decisions.extend(decisions);
    }
    let name = to
        .message
        .as_deref()
        .filter(|m| !m.trim().is_empty() && versions.from < versions.to)
        .and_then(|message| match &to.migration {
            // An imported version's message is its migration's file name:
            // its description only, or file names would carry the old
            // tool's version and extension (`…_v3_1_add_x_sql.sql`).
            Some(migration) => {
                romulus_sandbox::import::description(migration.format.into(), message)
            }
            None => Some(message.to_owned()),
        })
        .unwrap_or_else(|| format!("v{}_to_v{}", versions.from, versions.to));
    let sides = Sides {
        old: &old,
        new: &new,
        from: Some(versions.from),
        to: versions.to,
        from_label: format!("v{}", versions.from),
        to_label: format!("v{}", versions.to),
        name,
        database: format!("{org}/{slug}"),
        major: database.pg_major,
        renames,
    };
    Ok(Json(build(sides, options).await?))
}
