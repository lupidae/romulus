//! A draft's migration: `GET …/drafts/{id}/migration?format=` [member]. From
//! the draft's base version (nothing, for a database without versions) to
//! the model its SQL compiles to now, laid out like a version's migration
//! ([`crate::versions::migration`]). A draft whose SQL does not compile has
//! none: 422 `compile_failed`, with the diagnostics.

use axum::extract::State;
use remus_core::Schema;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    find, latest_number, not_open,
    publish::{compiled, hub_error},
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
    ),
    responses(
        (status = 200, body = SchemaMigration),
        (status = 400, description = "`bad_request`: an unknown format", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`draft_published`: its version has the migration now", body = ErrorBody),
        (status = 422, description = "`compile_failed`: the SQL has errors (`details.diagnostics`)", body = ErrorBody),
        (status = 503, description = "The compiler is unavailable", body = ErrorBody),
    )
)]
pub(super) async fn draft_migration(
    member: Member,
    Path((org, db_slug, id)): Path<(String, String, Uuid)>,
    Query(options): Query<MigrationOptions>,
    State(state): State<AppState>,
) -> ApiResult<Json<SchemaMigration>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    if row.status != DraftStatus::Open {
        return Err(not_open(row.status));
    }
    let latest = latest_number(&state.db, database.id).await?;
    let content = state.collab.content(id).await.map_err(hub_error)?;
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
    };
    Ok(Json(build(sides, options).await?))
}
