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
//! 3. checks the approvals of that text, when the organization requires
//!    some (409 `approvals_required`; an admin may publish without them,
//!    saying why: [`approvals::gate`]);
//! 4. compiles exactly that text (422 when it does not compile);
//! 5. stores it as the next version, under the lock on the database row,
//!    unless someone published since the draft started (409 `stale_base`:
//!    "Update draft" comes first), and marks the draft published.
//!
//! In migration mode the text is the migration: the version stores the
//! schema it derives (its model, and as `source` SQL that compiles back to
//! it: `drafts::source`) and the migration itself
//! (`versions.migration_sql`). What is reviewed is the source *on its
//! base* (`compile::input_hash`): after "Update draft" moved the base, the
//! same migration is another review.
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

use super::{
    approvals::{self, PublishApprovals},
    find, latest_number, not_open, source, versions,
};
use crate::{
    ApiError, ApiResult, AppState,
    auth::{Member, min},
    collab::{
        Hub, HubError,
        compile::input_hash,
        room::{Content, FreezeError},
    },
    databases::{self, DatabaseRef},
    domain::{DraftMode, DraftStatus, Schema},
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
    /// What the draft is edited as: its SQL, or a migration from its base.
    pub mode: DraftMode,
    /// What was previewed (`compile::input_hash`: the SQL; in migration
    /// mode the migration and the base it applies to): send it back to
    /// publish exactly this.
    pub sha256: String,
    /// Migration mode: the migration the version will keep.
    pub migration: Option<String>,
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
    /// The approvals the organization requires, and who approved this SQL.
    pub approvals: PublishApprovals,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PublishDraft {
    /// The `sha256` of the preview the person confirmed.
    pub sha256: String,
    /// What changed, in a line (at most 1000 characters).
    pub message: Option<String>,
    /// Publish without the approvals the organization requires, saying why
    /// (admins; the audit log keeps it).
    pub override_reason: Option<String>,
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
    let sha256 = reviewed(&content);
    let approvals = approvals::status(&state.db, member.org.id, member.role, &content).await?;
    let number = crate::versions::next_number(&state.db, database.id).await?;

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
        number,
        base_version: row.base_version,
        latest_version: latest,
        stale: row.base_version != latest,
        mode: content.mode,
        sha256,
        migration: (content.mode == DraftMode::Migration).then(|| content.migration.clone()),
        diagnostics,
        diff,
        base_model,
        model: model
            .map(|m| serde_json::to_value(m).map_err(ApiError::internal))
            .transpose()?,
        approvals,
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
        (status = 422, description = "`compile_failed`: the SQL does not compile (`details.diagnostics`); `unrepresentable`: in migration mode, no SQL romulus can keep compiles back to the schema (`details.objects`)", body = ErrorBody),
        (status = 429, description = "`rate_limited`: the checks publishing compiles (migration mode) count against the compile limits", body = ErrorBody),
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

    let content = freeze(&state, id, &member.user.name).await?;
    let frozen = Frozen {
        hub: &state.collab,
        draft: id,
        done: false,
    };

    if reviewed(&content) != body.sha256 {
        return Err(ApiError::Conflict {
            code: "draft_changed",
            message: "The draft changed while you were reviewing it. Review the changes again."
                .into(),
        });
    }
    let approvals = approvals::gate(&state, &member, id, &content, &body).await?;
    let mut model = frozen_model(&state.collab, &content).await?;
    let checks = source::Checks {
        hub: &state.collab,
        limits: &state.limits,
        user: member.user.id,
        database: database.id,
        major: content.major,
    };
    let source = version_source(&checks, &content, &model).await?;
    database.slug.clone_into(&mut model.database);
    let created = match store(
        &state,
        &database,
        id,
        draft.base_version_id,
        &content,
        (model, source),
        message,
        member.user.id,
        approvals,
    )
    .await
    {
        Ok(created) => created,
        Err(err) if err.code() == "draft_not_open" => {
            frozen.deleted(&member.user.name);
            return Err(err);
        }
        Err(err) => return Err(err),
    };
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

/// Freezes the draft for publishing (409 `draft_busy` while someone else
/// publishes or updates it): its content.
async fn freeze(state: &AppState, id: Uuid, by: &str) -> ApiResult<Content> {
    match state.collab.freeze(id, by).await.map_err(hub_error)? {
        Ok(content) => Ok(content),
        Err(FreezeError::NotOpen(_)) => Err(ApiError::Conflict {
            code: "draft_busy",
            message: "Someone else is publishing this draft right now.".into(),
        }),
    }
}

/// The model of the frozen content, or 422 `compile_failed`.
async fn frozen_model(hub: &Hub, content: &Content) -> ApiResult<Schema> {
    match compiled(hub, content).await? {
        (Some(model), _) => Ok(model),
        (None, diagnostics) => Err(ApiError::Unprocessable {
            code: "compile_failed",
            message: "The SQL has errors. Fix them before publishing.".into(),
            details: Some(json!({ "diagnostics": diagnostics })),
        }),
    }
}

/// Stores the frozen content as the next version and marks the draft
/// published, in one transaction. If an error comes back although that
/// transaction committed (reading the new version back failed), the draft is
/// published all the same: what the database says wins.
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
    (model, source): (Schema, String),
    message: Option<&str>,
    by: Uuid,
    approvals: Option<approvals::Freeze>,
) -> ApiResult<versions::Created> {
    let migration_sql = (content.mode == DraftMode::Migration).then(|| content.migration.clone());
    let stored = versions::create_version(
        &state.db,
        versions::DraftVersion {
            database_id: database.id,
            draft,
            expected_base: base,
            layout: content.layout.clone(),
            model,
            source,
            migration_sql,
            message: message.map(str::to_owned),
            created_by: by,
            approvals,
            compiled_on: content.major,
            discussion: content.discussion.clone(),
        },
    )
    .await;
    match stored {
        Ok(created) => Ok(created),
        Err(err) => match published_as(&state.db, draft).await {
            Ok(Some(created)) => {
                tracing::warn!(%draft, error = %err, "publish: committed, then failed; published anyway");
                Ok(created)
            }
            _ => Err(err),
        },
    }
}

/// The version a draft became, once it is published.
async fn published_as(db: &sqlx::PgPool, draft: Uuid) -> ApiResult<Option<versions::Created>> {
    Ok(sqlx::query_as!(
        versions::Created,
        r#"SELECT v.id, v.number FROM drafts d JOIN versions v ON v.id = d.published_version_id
           WHERE d.id = $1 AND d.status = 'published'"#,
        draft,
    )
    .fetch_optional(db)
    .await?)
}

/// The model of the content's source (the last compile's when it was of
/// that very text): its SQL, or its base version then its migration; or the
/// diagnostics that prevent one.
pub(super) async fn compiled(
    hub: &Hub,
    content: &Content,
) -> ApiResult<(Option<Schema>, Vec<Diagnostic>)> {
    // A `---` line typed and never posted: the SQL never keeps one.
    let unposted =
        crate::collab::comments::comment_line_diagnostics(content.source(), &content.base_sql);
    if !unposted.is_empty() {
        return Ok((None, unposted));
    }
    if let Some(model) = &content.model {
        let model = Schema::deserialize(&**model).map_err(ApiError::internal)?;
        return Ok((Some(model), Vec::new()));
    }
    let compiled = match content.mode {
        DraftMode::Schema => {
            hub.compiler()
                .compile_draft(content.major, &content.sql)
                .await?
        }
        DraftMode::Migration => {
            hub.compiler()
                .compile_migration(content.major, &content.base_sql, &content.migration)
                .await?
        }
    };
    Ok((compiled.model, compiled.diagnostics))
}

/// The SQL the version keeps: the draft's text, or (migration mode) SQL
/// that compiles back to the schema the migration made (`drafts::source`).
async fn version_source(
    checks: &source::Checks<'_>,
    content: &Content,
    model: &Schema,
) -> ApiResult<String> {
    match content.mode {
        DraftMode::Schema => Ok(content.sql.clone()),
        DraftMode::Migration => {
            checks
                .schema_source(model, &content.base_sql, &content.migration)
                .await
        }
    }
}

/// What a person reviews of a draft, as the preview names it: its source,
/// on its base in migration mode (`compile::input_hash`).
pub(super) fn reviewed(content: &Content) -> String {
    input_hash(content.mode, &content.base_sql, content.source())
}

/// A frozen draft: unfrozen when dropped, unless it was published.
pub(super) struct Frozen<'a> {
    pub(super) hub: &'a Hub,
    pub(super) draft: Uuid,
    pub(super) done: bool,
}

impl Frozen<'_> {
    fn published(mut self, number: i32, by: &str) {
        self.done = true;
        self.hub.published(self.draft, number, by);
    }

    /// Deleted meanwhile (its room was not loaded when the deletion asked):
    /// the room closes rather than open again.
    fn deleted(mut self, by: &str) {
        self.done = true;
        self.hub.deleted(self.draft, by);
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
pub(super) fn empty_like(model: &Schema) -> Schema {
    let mut empty = model.clone();
    empty.entities.clear();
    empty.enums.clear();
    empty.domains.clear();
    empty.composites.clear();
    empty.partitions.clear();
    empty.extensions.clear();
    empty
}

pub(super) fn hub_error(err: HubError) -> ApiError {
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
