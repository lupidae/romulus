//! A draft's migration: `GET …/drafts/{id}/migration?format=&sha256=`
//! [member]. From the draft's base version (nothing, for a database without
//! versions) to the model its SQL compiles to, laid out like a version's
//! migration ([`crate::versions::migration`]).
//!
//! - `sha256` (required; the publish preview's, or the live compile event's):
//!   the text the person reviewed, so a migration is never of a text nobody
//!   saw. When the draft moved on since, 409 `draft_changed`: review again.
//! - A stale draft (someone published since it started) has no migration a
//!   database at the latest version could run: 409 `stale_base`, update the
//!   draft first.
//! - A draft whose SQL does not compile has none: 422 `compile_failed`, with
//!   the diagnostics.
//! - Renames: the ones its text runs beyond its base (`ALTER … RENAME`,
//!   graph edits included) are known; detected ones are on or off as the
//!   draft's doc says (`renames`, what its editors chose), and `renames` in
//!   the request (what the person has on screen) over that.

use axum::extract::State;
use remus_core::Schema;
use serde::Deserialize;
use serde_json::json;
use utoipa::IntoParams;
use uuid::Uuid;

use super::{
    find, latest_number, not_open,
    publish::{self, compiled, hub_error},
    versions::stale_base,
};
use crate::{
    ApiError, ApiResult, AppState,
    auth::Member,
    databases,
    domain::DraftStatus,
    error::ErrorBody,
    extract::{Json, Path, Query},
    versions::{
        detail,
        migration::{MigrationOptions, SchemaMigration, Sides, build},
    },
};

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct Reviewed {
    /// What the person reviewed (the publish preview's `sha256`, or a live
    /// compile's SHA-256 of the SQL; in migration mode it covers the base
    /// too: `compile::input_hash`): the migration of exactly that, or 409
    /// `draft_changed`. Required: a migration is never of a text unseen.
    pub sha256: String,
}

/// The migration publishing the draft now would call for.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/drafts/{id}/migration",
    operation_id = "draft_migration",
    tag = "drafts",
    params(
        ("org" = String, Path),
        ("db" = String, Path),
        ("id" = Uuid, Path),
        MigrationOptions,
        Reviewed,
    ),
    responses(
        (status = 200, body = SchemaMigration),
        (status = 400, description = "`bad_request`: an unknown format, no `sha256`, `renames` that is not a JSON object of booleans", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`draft_published`: its version has the migration now; `draft_changed`: not the text reviewed (`sha256`); `stale_base`: update the draft first", body = ErrorBody),
        (status = 422, description = "`compile_failed`: the SQL has errors (`details.diagnostics`)", body = ErrorBody),
        (status = 503, description = "The compiler is unavailable", body = ErrorBody),
    )
)]
pub(super) async fn draft_migration(
    member: Member,
    Path((org, db_slug, id)): Path<(String, String, Uuid)>,
    Query(options): Query<MigrationOptions>,
    Query(reviewed): Query<Reviewed>,
    State(state): State<AppState>,
) -> ApiResult<Json<SchemaMigration>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    if row.status != DraftStatus::Open {
        return Err(not_open(row.status));
    }
    let latest = latest_number(&state.db, database.id).await?;
    // A database at the latest version cannot run a migration planned from
    // an older base: the draft lacks what was published since.
    if row.base_version != latest {
        return Err(stale_base(latest));
    }
    let content = state.collab.content(id).await.map_err(hub_error)?;
    if !publish::is_current(&content, &reviewed.sha256) {
        return Err(ApiError::Conflict {
            code: "draft_changed",
            message: "The draft changed since you reviewed it. Review the changes again.".into(),
        });
    }
    let (model, diagnostics) = compiled(&state.collab, &content).await?;
    let Some(new) = model else {
        return Err(ApiError::Unprocessable {
            code: "compile_failed",
            message: "The draft's SQL has errors: fix them to see its migration.".into(),
            details: Some(json!({ "diagnostics": diagnostics })),
        });
    };
    let old = if let Some(number) = row.base_version {
        let base = detail(&state.db, database.id, Some(number)).await?;
        Schema::deserialize(&base.model).map_err(ApiError::internal)?
    } else {
        // Nothing yet: everything is created.
        let mut empty = new.clone();
        empty.entities.clear();
        empty.enums.clear();
        empty.domains.clear();
        empty.composites.clear();
        empty.partitions.clear();
        empty.extensions.clear();
        empty
    };
    let mut decisions = content.renames.clone();
    if let Some(on_screen) = options.rename_decisions()? {
        decisions.extend(on_screen);
    }
    let sides = Sides {
        old: &old,
        new: &new,
        from: row.base_version,
        to: latest.map_or(1, |n| n + 1),
        from_label: row
            .base_version
            .map_or_else(|| "nothing".to_owned(), |n| format!("v{n}")),
        to_label: format!("draft \"{}\"", row.name),
        name: row.name.clone(),
        database: format!("{org}/{db_slug}"),
        major: content.major,
        renames: content.rename_choices(Some(&decisions)),
    };
    Ok(Json(build(sides, options).await?))
}
