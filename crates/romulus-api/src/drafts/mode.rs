//! A draft's two modes, one source of truth at a time (docs/MVP.md "Review
//! round 1", item 2): `POST …/drafts/{id}/mode` [developer], with
//! `{"mode": "migration"}` or `{"mode": "schema"}`.
//!
//! - **To migration mode** ("Edit migration"): the draft's SQL must compile
//!   (422 `compile_failed`); the migration romulus generates from the base
//!   version to that schema (`romulus_migration::editable`: its statements,
//!   headings and warnings, no transaction) becomes the doc's `migration`,
//!   the source from then on. The schema and the graph are what it makes of
//!   the base version.
//! - **Back to schema mode** ("Discard migration"): the migration must
//!   apply to the current base (422 `compile_failed` otherwise: after an
//!   update, an older model would revert what the new base brought). The
//!   schema SQL becomes that schema, written as the smallest edit of the
//!   SQL the draft had before: canonical, and `drafts.base_sql` the base in
//!   the same form (an update merges like with like), when both compile
//!   back to their models; otherwise the base's SQL then the migration
//!   (`drafts::source`), relative to the base's own text.
//!
//! Either way the room switches in one transaction everyone sees
//! (`Hub::switch_mode`: the texts and `meta.mode`, then `drafts.mode`), if
//! the source and the base it was computed from are still the room's;
//! computed again a few times while someone types (409 `draft_changed`
//! after that). Being published or updated: 409 `draft_busy`. Reviewers:
//! 403. Already in that mode: the draft as it is.

use axum::extract::State;
use chrono::Utc;
use romulus_migration::{Meta, Migration, Options, editable, migration};
use serde::Deserialize;
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    Draft, find, not_open, one,
    publish::{compiled, empty_like, hub_error},
    source,
    versions::{self, Version},
};
use crate::{
    ApiError, ApiResult, AppState,
    auth::{Member, min},
    collab::{
        compile::sha256_hex,
        room::{Content, RewriteError},
        store::BaseSql,
    },
    databases,
    domain::{DraftMode, DraftStatus, Schema},
    error::ErrorBody,
    extract::{Json, Path},
};

/// Attempts before giving up on a draft that keeps changing.
const ATTEMPTS: usize = 3;

#[derive(Debug, Deserialize, ToSchema)]
pub struct SwitchMode {
    /// The mode to switch to.
    pub mode: DraftMode,
}

/// The text a migration draft starts from: what to write, nothing written.
pub fn blank_migration(database: &str, from: Option<i32>) -> String {
    editable(&Migration::default(), &meta(database, from, ""))
}

fn meta(database: &str, from: Option<i32>, to: &str) -> Meta {
    Meta {
        database: database.to_owned(),
        from: from.map_or_else(|| "an empty database".to_owned(), |n| format!("v{n}")),
        to: to.to_owned(),
        date: Utc::now().format("%Y-%m-%d").to_string(),
        version: String::new(),
        name: String::new(),
    }
}

/// Switches the draft between schema mode and migration mode.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/drafts/{id}/mode",
    operation_id = "switch_draft_mode",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    request_body = SwitchMode,
    responses(
        (status = 200, body = Draft),
        (status = 403, description = "Reviewers cannot switch modes", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`draft_changed` (it kept changing: try again), `draft_busy` (being published or updated), `draft_published`", body = ErrorBody),
        (status = 422, description = "`compile_failed` (`details.diagnostics`): the SQL has errors, so there is no migration to start from; or the migration does not apply to the current base, so there is no schema to go back to. `unrepresentable` (`details.objects`): no SQL romulus can keep compiles back to the schema the migration made", body = ErrorBody),
        (status = 429, body = ErrorBody),
        (status = 503, description = "The compiler is unavailable", body = ErrorBody),
    )
)]
pub(super) async fn switch_mode(
    member: Member<min::Developer>,
    Path((org, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
    Json(body): Json<SwitchMode>,
) -> ApiResult<Json<Draft>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    if row.status != DraftStatus::Open {
        return Err(not_open(row.status));
    }
    // Either way the draft is compiled (the SQL, or base + migration).
    state.limits.compile.check(member.user.id)?;
    state.limits.database_compiles.check(database.id)?;
    let database_name = format!("{org}/{db_slug}");

    for _ in 0..ATTEMPTS {
        let content = state.collab.content(id).await.map_err(hub_error)?;
        if content.mode == body.mode {
            break;
        }
        // The base the room holds now, with the text: the seed is computed
        // from both, and the room refuses it if either moved meanwhile.
        let base = match content.base_version {
            Some(base) => versions::by_id(&state.db, database.id, base).await?,
            None => None,
        };
        let base_model = base.as_ref().map(Version::schema).transpose()?;
        let base_number = base.as_ref().map(|b| b.number);
        let model = current_model(&state, &content, body.mode, base_number).await?;
        let (seed, base_sql) = match body.mode {
            DraftMode::Migration => {
                let old = base_model.unwrap_or_else(|| empty_like(&model));
                let meta = meta(&database_name, base_number, &row.name);
                let seed = tokio::task::spawn_blocking(move || {
                    editable(&migration(&old, &model, &Options::default()), &meta)
                })
                .await
                .map_err(ApiError::internal)?;
                (seed, BaseSql::Keep)
            }
            DraftMode::Schema => {
                let checks = source::Checks {
                    hub: &state.collab,
                    limits: &state.limits,
                    user: member.user.id,
                    database: database.id,
                };
                let (seed, base_sql) = checks
                    .back_to_schema(
                        &model,
                        base_model.as_ref(),
                        &content.base_sql,
                        &content.migration,
                    )
                    .await?;
                (seed, base_sql.map_or(BaseSql::Version, BaseSql::Text))
            }
        };
        match state
            .collab
            .switch_mode(
                id,
                body.mode,
                sha256_hex(content.source()),
                content.base_version,
                seed,
                base_sql,
                &member.user.name,
            )
            .await
            .map_err(hub_error)?
        {
            Ok(_) => break,
            Err(RewriteError::Changed) => {}
            Err(RewriteError::NotOpen(_)) => {
                return Err(ApiError::Conflict {
                    code: "draft_busy",
                    message: "This draft is being published or updated. Try again in a moment."
                        .into(),
                });
            }
            Err(err @ RewriteError::Mode(_)) => return Err(ApiError::internal(err)),
        }
    }
    let row = find(&state.db, &database, id).await?;
    if row.mode != body.mode {
        return Err(ApiError::Conflict {
            code: "draft_changed",
            message: "The draft kept changing while switching. Try again when typing pauses."
                .into(),
        });
    }
    Ok(Json(one(&state, member.org.id, &database, row).await?))
}

/// The schema the draft's source makes now, on its base as the room holds
/// it (the SQL, or base + migration), to switch from: 422 `compile_failed`
/// otherwise. Going back to schema mode never takes an older model than the
/// migration on the current base: after "Update draft" that would revert
/// what the new base brought.
async fn current_model(
    state: &AppState,
    content: &Content,
    to: DraftMode,
    base: Option<i32>,
) -> ApiResult<Schema> {
    match compiled(&state.collab, content).await? {
        (Some(model), _) => Ok(model),
        (None, diagnostics) => Err(ApiError::Unprocessable {
            code: "compile_failed",
            message: match (to, base) {
                (DraftMode::Migration, _) => {
                    "The SQL has errors: fix them, then edit the migration.".to_owned()
                }
                (DraftMode::Schema, Some(n)) => format!(
                    "The migration does not apply to v{n}: fix it (or empty it, to start from v{n}) before going back to the SQL."
                ),
                (DraftMode::Schema, None) => {
                    "The migration has errors: fix them (or empty it) before going back to the SQL."
                        .to_owned()
                }
            },
            details: Some(json!({ "diagnostics": diagnostics })),
        }),
    }
}
