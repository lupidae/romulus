//! A draft's source over REST: the text people type in the editor (the SQL
//! in schema mode, the migration in migration mode), read with where its
//! compile stands, and written as a person's edit. The MCP endpoint's draft
//! tools go through here; the editor itself speaks to the collab server.
//!
//! Endpoints (who may call them in brackets; see `auth::member`):
//! - `GET …/drafts/{id}/text?wait=` [member]: the source, its SHA-256, the
//!   hash a migration, a test or publishing names it by, and the last
//!   compile (with `wait`, a few seconds for the compile of this text).
//! - `PUT …/drafts/{id}/text` [developer]: the whole new source, or edits
//!   (an exact piece of text replaced, or text appended), optionally only if
//!   the source is still the one read (`sha256`). Applied by the room as
//!   the smallest edit, live for everyone in the draft, as a rewrite (undo
//!   histories go, as with Tidy), and the caller is an editor from then on:
//!   approvals of the old text stop counting, and theirs never counts.
//! - `PUT …/drafts/{id}/renames` [developer]: renames of the draft's
//!   migration turned on or off, or back to romulus's own choice, as the
//!   migration view's switches do (part of what is reviewed: approvals of
//!   other decisions stop counting, and the caller is an editor).

use std::{collections::BTreeMap, time::Duration};

use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::{find, not_open, publish::hub_error};
use crate::{
    ApiError, ApiResult, AppState,
    auth::{Member, min},
    collab::{
        compile::{CompileState, sha256_hex},
        doc::MAX_RENAME_DECISIONS,
        patch,
        protocol::LiveStatus,
        room::{Content, RewriteError},
    },
    databases,
    domain::{DraftMode, DraftStatus},
    error::ErrorBody,
    extract::{Json, Path, Query},
};

/// A draft's source is at most this (what the compiler takes).
pub const MAX_TEXT_BYTES: usize = 1 << 20;

/// Edits at most per request.
const MAX_EDITS: usize = 100;

/// Attempts before giving up on a draft that keeps changing (edits only:
/// a whole text is applied once, or refused).
const ATTEMPTS: usize = 3;

/// How long `wait` waits for the compile of the text.
const COMPILE_WAIT: Duration = Duration::from_secs(10);

/// A draft's source, and where its compile stands.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DraftText {
    /// Which text is the source: the schema SQL, or a migration from the
    /// base version.
    pub mode: DraftMode,
    /// The source as it is now.
    pub text: String,
    /// Hex SHA-256 of `text`: send it back with an edit so that one made by
    /// someone else meanwhile is never overwritten.
    pub sha256: String,
    /// What is reviewed (`publish::reviewed`): the `sha256` the draft's
    /// migration, a migration test, an approval and publishing take.
    pub reviewed_sha256: String,
    /// The version the draft is based on; `null` when the database had
    /// none.
    pub base_version: Option<i32>,
    /// In migration mode, the schema the migration makes, as SQL (from the
    /// last compile that succeeded); `null` in schema mode.
    pub schema_sql: Option<String>,
    pub compile: DraftCompile,
}

/// Where the source's compile stands.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DraftCompile {
    /// `ok`, `failed` (errors in `diagnostics`), `unavailable` (the
    /// compiler is away), or `pending` (not compiled yet).
    #[schema(value_type = String)]
    pub state: String,
    /// The result is of `text` as it is; otherwise a newer compile is due
    /// and `state` is the previous text's.
    pub current: bool,
    /// Errors (when `failed`), then notes, located in `text`.
    #[schema(value_type = Vec<romulus_sandbox::Diagnostic>)]
    pub diagnostics: Value,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ReadText {
    /// Wait (a few seconds at most) for the compile of the text as it is.
    #[serde(default)]
    pub wait: bool,
}

/// One edit: `old`, exactly as it is in the source and found once,
/// replaced by `new`; or, with `old` empty, `new` appended at the end.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TextEdit {
    #[serde(default)]
    pub old: String,
    pub new: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EditDraftText {
    /// The whole new source. Or:
    #[serde(default)]
    pub text: Option<String>,
    /// edits, applied in order to the source as it is.
    #[serde(default)]
    pub edits: Option<Vec<TextEdit>>,
    /// Hex SHA-256 of the source the caller read (`DraftText.sha256`): 409
    /// `draft_changed` if it is another now. Absent: applied to the source
    /// as it is (edits are, again, if someone typed meanwhile).
    #[serde(default)]
    pub sha256: Option<String>,
    /// The mode the text is for: 409 `draft_mode_changed` if the draft is in
    /// the other one. Absent: whichever it is in.
    #[serde(default)]
    pub mode: Option<DraftMode>,
    /// Wait (a few seconds at most) for the compile of the new text.
    #[serde(default)]
    pub wait: bool,
}

/// What [`DraftText`] says of `content`.
fn text_of(content: &Content, base_version: Option<i32>) -> DraftText {
    let source = content.source();
    let compile = content.compile.as_ref().map_or(
        DraftCompile {
            state: "pending".into(),
            current: false,
            diagnostics: Value::Array(Vec::new()),
        },
        |last| DraftCompile {
            state: state_name(&last.state).into(),
            current: last.current,
            diagnostics: last.diagnostics.clone(),
        },
    );
    DraftText {
        mode: content.mode,
        text: source.to_owned(),
        sha256: sha256_hex(source),
        reviewed_sha256: super::publish::reviewed(content),
        base_version,
        schema_sql: content
            .compile
            .as_ref()
            .and_then(|last| last.schema_sql.as_deref())
            .map(str::to_owned),
        compile,
    }
}

fn state_name(state: &CompileState) -> &'static str {
    match state {
        CompileState::Ok => "ok",
        CompileState::Failed => "failed",
        CompileState::Unavailable => "unavailable",
        CompileState::Pending => "pending",
    }
}

/// The content, once the compile of its text is in (or `COMPILE_WAIT` has
/// passed, or the compiler is away).
async fn compiled_content(state: &AppState, id: Uuid, wait: bool) -> ApiResult<Content> {
    let deadline = tokio::time::Instant::now() + COMPILE_WAIT;
    loop {
        let content = state.collab.content(id).await.map_err(hub_error)?;
        let settled = content
            .compile
            .as_ref()
            .is_some_and(|last| last.current || matches!(last.state, CompileState::Unavailable));
        if !wait || settled || tokio::time::Instant::now() >= deadline {
            return Ok(content);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The draft's source and its compile.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/drafts/{id}/text",
    operation_id = "get_draft_text",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path), ReadText),
    responses(
        (status = 200, body = DraftText),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`draft_published`: its version keeps it", body = ErrorBody),
        (status = 503, body = ErrorBody),
    )
)]
pub(super) async fn read(
    member: Member,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    Query(query): Query<ReadText>,
    State(state): State<AppState>,
) -> ApiResult<Json<DraftText>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    if row.status != DraftStatus::Open {
        return Err(not_open(row.status));
    }
    let content = compiled_content(&state, id, query.wait).await?;
    Ok(Json(text_of(&content, row.base_version)))
}

/// Writes the draft's source: the whole text, or edits of it.
#[utoipa::path(
    put,
    path = "/orgs/{org}/databases/{db}/drafts/{id}/text",
    operation_id = "edit_draft_text",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    request_body = EditDraftText,
    responses(
        (status = 200, description = "The source after the edit", body = DraftText),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`draft_changed` (not the text read: read it again), `draft_mode_changed`, `draft_published`, `draft_busy` (being published or updated)", body = ErrorBody),
        (status = 413, body = ErrorBody),
        (status = 422, description = "`validation`: neither or both of `text` and `edits`; `edit_not_found` / `edit_ambiguous`: an edit's `old` is not in the source, or more than once (`details.edit`, from 0)", body = ErrorBody),
        (status = 503, body = ErrorBody),
    )
)]
pub(super) async fn edit(
    member: Member<min::Developer>,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
    Json(body): Json<EditDraftText>,
) -> ApiResult<Json<DraftText>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    if row.status != DraftStatus::Open {
        return Err(not_open(row.status));
    }
    let change = match (body.text, body.edits) {
        (Some(text), None) => Change::Text(text),
        (None, Some(edits)) if !edits.is_empty() && edits.len() <= MAX_EDITS => {
            Change::Edits(edits)
        }
        (None, Some(_)) => {
            return Err(crate::orgs::invalid(
                "edits",
                "Send 1 to 100 edits, or the whole text.",
            ));
        }
        _ => {
            return Err(crate::orgs::invalid(
                "text",
                "Send either the whole text (`text`) or edits of it (`edits`).",
            ));
        }
    };

    for _ in 0..ATTEMPTS {
        let content = state.collab.content(id).await.map_err(hub_error)?;
        if body.mode.is_some_and(|mode| mode != content.mode) {
            return Err(mode_changed(content.mode));
        }
        let old = content.source().to_owned();
        let from = sha256_hex(&old);
        if body.sha256.as_deref().is_some_and(|read| read != from) {
            return Err(changed());
        }
        let new = change.apply(&old)?;
        if new.len() > MAX_TEXT_BYTES {
            return Err(ApiError::PayloadTooLarge);
        }
        let (old_text, new_text) = (old.clone(), new.clone());
        let changes = tokio::task::spawn_blocking(move || patch::changes(&old_text, &new_text))
            .await
            .map_err(ApiError::internal)?;
        match state
            .collab
            .edit(id, member.user.id, content.mode, from, changes)
            .await
            .map_err(hub_error)?
        {
            Ok(edited) => {
                tracing::info!(draft = %id, edited, via_token = member.user.via.is_some(), "draft text written");
                let content = compiled_content(&state, id, body.wait).await?;
                return Ok(Json(text_of(&content, row.base_version)));
            }
            // A whole text, or a text read, was for the source as it was.
            Err(RewriteError::Changed)
                if body.sha256.is_some() || matches!(change, Change::Text(_)) =>
            {
                return Err(changed());
            }
            Err(RewriteError::Changed) => {}
            Err(RewriteError::Mode(mode)) => return Err(mode_changed(mode)),
            Err(RewriteError::NotOpen(status)) => return Err(closed(status)),
        }
    }
    Err(changed())
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetRenames {
    /// Rename keys (`SchemaMigration.renames[].key`) to `true` (apply it),
    /// `false` (do not), or `null` (romulus's own choice again).
    #[schema(value_type = std::collections::BTreeMap<String, Option<bool>>)]
    pub renames: BTreeMap<String, Option<bool>>,
}

/// A draft's rename decisions, as the doc holds them.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RenameDecisions {
    #[schema(value_type = std::collections::BTreeMap<String, bool>)]
    pub renames: BTreeMap<String, bool>,
}

/// Turns renames of the draft's migration on or off.
#[utoipa::path(
    put,
    path = "/orgs/{org}/databases/{db}/drafts/{id}/renames",
    operation_id = "set_draft_renames",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    request_body = SetRenames,
    responses(
        (status = 200, description = "The draft's decisions after", body = RenameDecisions),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`draft_published`, `draft_busy`", body = ErrorBody),
        (status = 422, body = ErrorBody),
    )
)]
pub(super) async fn set_renames(
    member: Member<min::Developer>,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
    Json(body): Json<SetRenames>,
) -> ApiResult<Json<RenameDecisions>> {
    if body.renames.len() > MAX_RENAME_DECISIONS
        || body
            .renames
            .keys()
            .any(|key| key.is_empty() || key.len() > 2048)
    {
        return Err(crate::orgs::invalid(
            "renames",
            "Send at most 2000 rename keys, as the migration lists them.",
        ));
    }
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    let row = find(&state.db, &database, id).await?;
    if row.status != DraftStatus::Open {
        return Err(not_open(row.status));
    }
    match state
        .collab
        .set_renames(id, member.user.id, body.renames)
        .await
        .map_err(hub_error)?
    {
        Ok(renames) => Ok(Json(RenameDecisions { renames })),
        Err(RewriteError::NotOpen(status)) => Err(closed(status)),
        Err(RewriteError::Changed | RewriteError::Mode(_)) => Err(changed()),
    }
}

enum Change {
    Text(String),
    Edits(Vec<TextEdit>),
}

impl Change {
    fn apply(&self, source: &str) -> ApiResult<String> {
        match self {
            Self::Text(text) => Ok(text.clone()),
            Self::Edits(edits) => apply_edits(source, edits),
        }
    }
}

/// `edits` applied in order to `source`.
pub fn apply_edits(source: &str, edits: &[TextEdit]) -> ApiResult<String> {
    let mut text = source.to_owned();
    for (index, edit) in edits.iter().enumerate() {
        if edit.old.is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&edit.new);
            continue;
        }
        let mut found = text.match_indices(edit.old.as_str());
        let Some((at, _)) = found.next() else {
            return Err(edit_error(
                "edit_not_found",
                index,
                "is not in the draft's text: read the text again and copy it exactly",
            ));
        };
        if found.next().is_some() {
            return Err(edit_error(
                "edit_ambiguous",
                index,
                "is in the draft's text more than once: include more of the text around it",
            ));
        }
        text.replace_range(at..at + edit.old.len(), &edit.new);
    }
    Ok(text)
}

fn edit_error(code: &'static str, index: usize, what: &str) -> ApiError {
    ApiError::Unprocessable {
        code,
        message: format!("Edit {}: its `old` {what}.", index + 1),
        details: Some(serde_json::json!({ "edit": index })),
    }
}

fn changed() -> ApiError {
    ApiError::Conflict {
        code: "draft_changed",
        message: "Someone changed the draft since it was read. Read it again, then edit.".into(),
    }
}

fn mode_changed(mode: DraftMode) -> ApiError {
    ApiError::Conflict {
        code: "draft_mode_changed",
        message: format!(
            "The draft is edited as {} now. Read it again.",
            match mode {
                DraftMode::Schema => "its schema SQL",
                DraftMode::Migration => "a migration",
            }
        ),
    }
}

fn closed(status: LiveStatus) -> ApiError {
    match status {
        LiveStatus::Publishing | LiveStatus::Updating => ApiError::Conflict {
            code: "draft_busy",
            message: "The draft is being published or updated. Try again in a moment.".into(),
        },
        _ => not_open(DraftStatus::Published),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old: &str, new: &str) -> TextEdit {
        TextEdit {
            old: old.into(),
            new: new.into(),
        }
    }

    #[test]
    fn edits_replace_one_exact_piece_or_append() {
        let text = apply_edits(
            "CREATE TABLE a (id int);\n",
            &[
                edit("(id int)", "(id int, name text)"),
                edit("", "CREATE TABLE b ();"),
            ],
        )
        .unwrap();
        assert_eq!(
            text,
            "CREATE TABLE a (id int, name text);\nCREATE TABLE b ();"
        );
        // Appending to a text without a final newline starts a line.
        assert_eq!(apply_edits("x", &[edit("", "y")]).unwrap(), "x\ny");
    }

    #[test]
    fn an_edit_must_find_its_text_exactly_once() {
        let missing = apply_edits("a b", &[edit("c", "d")]).unwrap_err();
        assert_eq!(missing.code(), "edit_not_found");
        let twice = apply_edits("a a", &[edit("a", "b")]).unwrap_err();
        assert_eq!(twice.code(), "edit_ambiguous");
        assert!(twice.to_string().starts_with("Edit 1:"));
    }
}
