//! Publishing a draft as the database's next version, in one step
//! (docs/MVP.md "Drafts and versions: the UX").
//!
//! The editor first asks for a [`PublishPreview`]: the diff against the
//! base, and the SHA-256 of the text it is about. Publishing then:
//!
//! 1. freezes the draft (everyone sees "Publishing…"; the server drops
//!    every update from then on);
//! 2. checks that the frozen text is the one previewed (409
//!    `draft_changed` otherwise: someone typed meanwhile, review again);
//! 3. compiles exactly that text (422 when it does not compile);
//! 4. stores it as the next version, under the lock on the database row,
//!    unless someone published since the draft started (409 `stale_base`:
//!    "Update draft" comes first), and marks the draft published.
//!
//! Any failure unfreezes the draft, and the clients send back what they
//! typed while it was frozen.

use axum::{extract::State, http::StatusCode};
use romulus_diff::SchemaDiff;
use romulus_sandbox::Diagnostic;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{find, latest_number, not_open, versions};
use crate::{
    ApiError, ApiResult, AppState,
    auth::{Member, min},
    collab::{
        Hub, HubError,
        compile::sha256_hex,
        room::{Content, FreezeError},
    },
    databases::{self, DatabaseRef},
    domain::{DraftStatus, Schema},
    error::ErrorBody,
    extract::{Json, Path},
    orgs::invalid,
};

/// What publishing the draft now would create.
#[derive(Debug, Serialize, ToSchema)]
pub struct PublishPreview {
    /// The number the new version would get.
    pub number: i32,
    pub base_version: Option<i32>,
    pub latest_version: Option<i32>,
    /// Someone published since the draft started: "Update draft" first.
    pub stale: bool,
    /// SHA-256 of the SQL previewed: send it back to publish exactly this.
    pub sha256: String,
    /// Errors that prevent publishing (then no diff), then notes.
    pub diagnostics: Vec<Diagnostic>,
    /// What changes against the base version (against nothing when there is
    /// none); `null` when the SQL does not compile.
    pub diff: Option<SchemaDiff>,
    /// The base version's model, for drawing the diff.
    #[schema(value_type = Option<Object>)]
    pub base_model: Option<Value>,
    /// The model the version would have.
    #[schema(value_type = Option<Object>)]
    pub model: Option<Value>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PublishDraft {
    /// The `sha256` of the preview the person confirmed.
    pub sha256: String,
    /// What changed, in a line (at most 1000 characters).
    pub message: Option<String>,
}

/// The version a draft became.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct Published {
    pub id: Uuid,
    pub number: i32,
}

/// The diff publishing would create, for the confirm dialog.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/drafts/{id}/publish",
    operation_id = "preview_publish",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    responses(
        (status = 200, body = PublishPreview),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "Already published", body = ErrorBody),
        (status = 503, description = "The compiler is unavailable", body = ErrorBody),
    )
)]
pub(super) async fn preview(
    member: Member<min::Developer>,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
) -> ApiResult<Json<PublishPreview>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    if row.status != DraftStatus::Open {
        return Err(not_open(row.status));
    }
    let latest = latest_number(&state.db, database.id).await?;
    let content = state.collab.content(id).await.map_err(hub_error)?;
    let sha256 = sha256_hex(&content.sql);

    let (model, diagnostics) = compiled(&state.collab, &content).await?;
    let base_model = match row.base_version {
        Some(_) => versions::latest(&state.db, database.id)
            .await?
            .filter(|v| Some(v.number) == row.base_version)
            .map(|v| v.model),
        None => None,
    };
    let base_schema = base_model
        .as_ref()
        .map(Schema::deserialize)
        .transpose()
        .map_err(ApiError::internal)?;
    let diff = model.as_ref().map(|model| {
        let empty = empty_like(model);
        romulus_diff::diff(base_schema.as_ref().unwrap_or(&empty), model)
    });

    Ok(Json(PublishPreview {
        number: latest.map_or(1, |n| n + 1),
        base_version: row.base_version,
        latest_version: latest,
        stale: row.base_version != latest,
        sha256,
        diagnostics,
        diff,
        base_model,
        model: model
            .map(|m| serde_json::to_value(m).map_err(ApiError::internal))
            .transpose()?,
    }))
}

/// Publishes the draft as the next version.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/drafts/{id}/publish",
    operation_id = "publish_draft",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    request_body = PublishDraft,
    responses(
        (status = 201, body = Published),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`draft_changed` (review again), `stale_base` (update the draft first), `draft_published`, `draft_busy` (someone else is publishing it)", body = ErrorBody),
        (status = 422, description = "The SQL does not compile (`details.diagnostics`)", body = ErrorBody),
        (status = 503, description = "The compiler is unavailable", body = ErrorBody),
    )
)]
pub(super) async fn publish(
    member: Member<min::Developer>,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
    Json(body): Json<PublishDraft>,
) -> ApiResult<(StatusCode, Json<Published>)> {
    let message = body
        .message
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    if message.is_some_and(|m| m.chars().count() > 1000) {
        return Err(invalid(
            "message",
            "Keep the message to 1000 characters or fewer.",
        ));
    }
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let draft = sqlx::query!(
        r#"SELECT status AS "status: DraftStatus", base_version_id
           FROM drafts WHERE id = $1 AND database_id = $2 AND status <> 'deleted'"#,
        id,
        database.id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("No such draft. It may have been deleted."))?;
    if draft.status != DraftStatus::Open {
        return Err(not_open(draft.status));
    }
    // Checked again under the lock; this spares freezing for nothing.
    let latest = versions::latest(&state.db, database.id).await?;
    if latest.as_ref().map(|v| v.id) != draft.base_version_id {
        return Err(versions::stale_base(latest.map(|v| v.number)));
    }

    let content = match state
        .collab
        .freeze(id, &member.user.name)
        .await
        .map_err(hub_error)?
    {
        Ok(content) => content,
        Err(FreezeError::NotOpen(_)) => {
            return Err(ApiError::Conflict {
                code: "draft_busy",
                message: "Someone else is publishing this draft right now.".into(),
            });
        }
    };
    let frozen = Frozen {
        hub: &state.collab,
        draft: id,
        done: false,
    };

    if sha256_hex(&content.sql) != body.sha256 {
        return Err(ApiError::Conflict {
            code: "draft_changed",
            message: "The draft changed while you were reviewing it. Review the changes again."
                .into(),
        });
    }
    let mut model = match compiled(&state.collab, &content).await? {
        (Some(model), _) => model,
        (None, diagnostics) => {
            return Err(ApiError::Unprocessable {
                code: "compile_failed",
                message: "The SQL has errors. Fix them before publishing.".into(),
                details: Some(json!({ "diagnostics": diagnostics })),
            });
        }
    };
    database.slug.clone_into(&mut model.database);
    let created = store(
        &state,
        &database,
        id,
        draft.base_version_id,
        &content,
        model,
        message,
        member.user.id,
    )
    .await?;
    frozen.published(created.number, &member.user.name);
    state.collab.version_created(database.id, created.number);
    tracing::info!(draft = %id, number = created.number, "draft published");
    Ok((
        StatusCode::CREATED,
        Json(Published {
            id: created.id,
            number: created.number,
        }),
    ))
}

/// Stores the frozen content as the next version, then marks the draft
/// published. Two steps: if the second fails, the version exists and the
/// draft, still open, is now stale (it cannot be published twice).
#[expect(
    clippy::too_many_arguments,
    reason = "one call site, all of it the version"
)]
async fn store(
    state: &AppState,
    database: &DatabaseRef,
    draft: Uuid,
    base: Option<Uuid>,
    content: &Content,
    model: Schema,
    message: Option<&str>,
    by: Uuid,
) -> ApiResult<versions::Created> {
    let created = versions::create_version(
        &state.db,
        versions::DraftVersion {
            database_id: database.id,
            expected_base: base,
            layout: content.layout.clone(),
            model,
            source: content.sql.clone(),
            message: message.map(str::to_owned),
            created_by: by,
        },
    )
    .await?;
    let marked = sqlx::query!(
        "UPDATE drafts SET status = 'published', published_version_id = $2
         WHERE id = $1 AND status = 'open'",
        draft,
        created.id,
    )
    .execute(&state.db)
    .await?;
    if marked.rows_affected() != 1 {
        tracing::warn!(%draft, number = created.number, "publish: the draft was deleted meanwhile");
    }
    Ok(created)
}

/// The model of the content's SQL (the last compile's when it was of that
/// very text), or the diagnostics that prevent one.
async fn compiled(hub: &Hub, content: &Content) -> ApiResult<(Option<Schema>, Vec<Diagnostic>)> {
    if let Some(model) = &content.model {
        let model = Schema::deserialize(&**model).map_err(ApiError::internal)?;
        return Ok((Some(model), Vec::new()));
    }
    let compiled = hub.compiler().compile(&content.sql).await?;
    Ok((compiled.model, compiled.diagnostics))
}

/// A frozen draft: unfrozen when dropped, unless it was published.
struct Frozen<'a> {
    hub: &'a Hub,
    draft: Uuid,
    done: bool,
}

impl Frozen<'_> {
    fn published(mut self, number: i32, by: &str) {
        self.done = true;
        self.hub.published(self.draft, number, by);
    }
}

impl Drop for Frozen<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.hub.unfreeze(self.draft);
        }
    }
}

/// A schema with nothing in it, for a first version's diff.
fn empty_like(model: &Schema) -> Schema {
    let mut empty = model.clone();
    empty.entities.clear();
    empty.enums.clear();
    empty.domains.clear();
    empty.composites.clear();
    empty.partitions.clear();
    empty.extensions.clear();
    empty
}

fn hub_error(err: HubError) -> ApiError {
    ApiError::Unavailable(match err {
        HubError::Closing => "The server is restarting. Try again in a moment.".into(),
        HubError::Unavailable => "The draft could not be loaded. Try again in a moment.".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_version_diffs_against_nothing() {
        let model = Schema::from_json(remus_core::fixtures::SHOWCASE).unwrap();
        let empty = empty_like(&model);
        let diff = romulus_diff::diff(&empty, &model);
        assert!(!diff.is_empty());
        assert_eq!(empty.entities, []);
    }
}
