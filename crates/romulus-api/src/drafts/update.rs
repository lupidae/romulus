//! "Update draft": bringing a version published since the draft started into
//! it (docs/MVP.md "Drafts and versions: the UX"). The SQL merge itself is
//! `romulus_sandbox::merge`, statement by statement.
//!
//! 1. Freeze the draft: everyone sees "Updating…", edits are dropped (and
//!    sent again once it is over, Yjs merging them with the update's).
//! 2. Merge its SQL with the latest version's, relative to `drafts.base_sql`
//!    (or, before any update, its base version's text), and its layout and
//!    groups key by key ([`merge_layout`]).
//! 3. Rewrite the SQL as the smallest edit in one Yjs transaction
//!    (`Hub::rewrite`, as Tidy does), so everyone's cursors and selections
//!    stay where they were; then the layout and groups, and wait until it is
//!    all written.
//! 4. Move `base_version_id` to the latest version and keep the new
//!    `base_sql`; tell everyone; unfreeze.
//!
//! Conflicts become blocks in the text that the compiler refuses, so the
//! draft cannot be published until they are resolved. Updating again while
//! blocks remain (a newer version came out meanwhile) rewrites the blocks it
//! concerns. A draft whose SQL does not parse is not merged (422
//! `draft_has_errors`): the merge works on statements.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use axum::extract::State;
use romulus_sandbox::{
    merge::{Input, MergeError, Merged, conflicts, merge},
    render_sql,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    no_such_draft, not_open,
    publish::{Frozen, hub_error},
    versions::{self, Version},
};
use crate::{
    ApiError, ApiResult, AppState,
    auth::{Member, min},
    collab::{
        compile::sha256_hex,
        patch,
        room::{Content, FreezeError, RewriteError, UpdateTicket},
    },
    databases,
    domain::{DraftStatus, Schema},
    error::ErrorBody,
    extract::{Json, Path},
    versions::layout::{TableGroup, TablePosition, VersionLayout},
};

/// What "Update draft" did.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdatedDraft {
    /// The version the draft is based on now (`null`: the database has
    /// none).
    pub base_version: Option<i32>,
    /// Conflict blocks in its SQL now: publishing waits until none is left.
    pub conflicts: u32,
    /// Whether anything changed (already on the latest version: nothing).
    pub changed: bool,
}

/// Brings the latest version into the draft: see the module documentation.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/drafts/{id}/update",
    operation_id = "update_draft",
    tag = "drafts",
    params(("org" = String, Path), ("db" = String, Path), ("id" = Uuid, Path)),
    responses(
        (status = 200, body = UpdatedDraft),
        (status = 403, description = "Reviewers cannot update drafts", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`draft_published`, `draft_busy` (being published or updated), `draft_changed` (rewritten meanwhile, try again)", body = ErrorBody),
        (status = 422, description = "`draft_has_errors`: the SQL does not parse (`details.diagnostics`)", body = ErrorBody),
        (status = 503, description = "The draft could not be loaded", body = ErrorBody),
    )
)]
pub(super) async fn update(
    member: Member<min::Developer>,
    Path((_, db_slug, id)): Path<(String, String, Uuid)>,
    State(state): State<AppState>,
) -> ApiResult<Json<UpdatedDraft>> {
    let database = databases::find(&state.db, member.org.id, &db_slug).await?;
    // A first look: nothing newer, nothing to freeze for.
    let draft = read(&state, database.id, id).await?;
    let latest = versions::latest(&state.db, database.id).await?;
    if latest.as_ref().map(|v| v.id) == draft.base_version_id {
        return up_to_date(&state, database.id, id, draft.base_version_id).await;
    }

    let (content, ticket) = match state
        .collab
        .freeze_to_update(id, &member.user.name)
        .await
        .map_err(hub_error)?
    {
        Ok(frozen) => frozen,
        Err(FreezeError::NotOpen(_)) => {
            return Err(ApiError::Conflict {
                code: "draft_busy",
                message: "Someone is publishing or updating this draft right now.".into(),
            });
        }
    };
    let frozen = Frozen {
        hub: &state.collab,
        draft: id,
        done: false,
    };
    // Frozen, read again: an update that finished meanwhile moved the base
    // and left its text, and this one starts from both.
    let draft = read(&state, database.id, id).await?;
    let latest = versions::latest(&state.db, database.id).await?;
    let Some(latest) = latest.filter(|v| Some(v.id) != draft.base_version_id) else {
        drop(frozen);
        return up_to_date(&state, database.id, id, draft.base_version_id).await;
    };
    let base_of = |version: Option<Uuid>| version_of(&state, database.id, version);
    // Where the text stands: an update that rewrote it but failed before
    // moving the base left that base pending, and its rewrite noted in the
    // doc the version it merged. Then the text is relative to that base,
    // whatever was typed since (the layout still merges from the old one).
    let pending = base_of(draft.pending_base_version_id)
        .await?
        .filter(|pending| content.merged == Some(pending.number));
    let (base, ancestor) = match pending {
        Some(pending) => (Some(pending), draft.pending_base_sql.clone()),
        None => (
            base_of(draft.base_version_id).await?,
            draft.base_sql.clone(),
        ),
    };
    let layout_base = base_of(draft.base_version_id).await?;

    // From here, a failure leaves the draft with its merged text, its old
    // base and the new one pending: the next update finishes this one.
    let (plan, rewritten, latest) = rewrite(
        &state,
        id,
        ticket,
        Bases {
            sql: base,
            layout: layout_base,
            ancestor,
        },
        latest,
        content,
    )
    .await?;
    let changed = rewritten || !(plan.layout.is_empty() && plan.groups.is_empty());
    let (conflicts, ancestor) = (plan.conflicts, plan.merged.ancestor);
    // Then the layout, and everything is written before the base moves.
    state
        .collab
        .merge_layout(id, ticket, plan.layout, plan.groups)
        .await
        .map_err(hub_error)?
        .map_err(ApiError::internal)?;

    // The text is written: the base moves, nothing is pending any more.
    let moved = sqlx::query!(
        "UPDATE drafts SET base_version_id = $2, base_sql = $3, updated_at = now(),
                           pending_base_version_id = NULL, pending_base_sql = NULL
         WHERE id = $1 AND status = 'open' AND base_version_id IS NOT DISTINCT FROM $4",
        id,
        latest.id,
        ancestor,
        draft.base_version_id,
    )
    .execute(&state.db)
    .await?;
    if moved.rows_affected() != 1 {
        return Err(ApiError::Conflict {
            code: "draft_not_open",
            message: "This draft was deleted or published meanwhile.".into(),
        });
    }
    state
        .collab
        .updated(id, latest.number, &member.user.name, conflicts);
    drop(frozen);
    tracing::info!(draft = %id, version = latest.number, conflicts, "draft updated");
    Ok(Json(UpdatedDraft {
        base_version: Some(latest.number),
        conflicts,
        changed,
    }))
}

/// What a merge starts from: the version and text the SQL is relative to,
/// and the version the layout is.
struct Bases {
    sql: Option<Version>,
    layout: Option<Version>,
    ancestor: Option<String>,
}

/// Merges and rewrites the draft's SQL, the new base pending on the draft
/// first (see the pending columns' comment). Frozen, nobody types; another
/// server rewrite can still land first: then merge again from what is there
/// now. Answers the plan, whether the text changed, and the latest version
/// back.
async fn rewrite(
    state: &AppState,
    id: Uuid,
    ticket: UpdateTicket,
    bases: Bases,
    latest: Version,
    mut content: Content,
) -> ApiResult<(Plan, bool, Arc<Version>)> {
    let bases = Arc::new(bases);
    let latest = Arc::new(latest);
    for attempt in 1..=ATTEMPTS {
        let plan = {
            let (bases, latest) = (Arc::clone(&bases), Arc::clone(&latest));
            tokio::task::spawn_blocking(move || Plan::new(&bases, &latest, content))
                .await
                .map_err(ApiError::internal)??
        };
        if !plan.changes.is_empty() {
            sqlx::query!(
                "UPDATE drafts SET pending_base_version_id = $2, pending_base_sql = $3
                 WHERE id = $1 AND status = 'open'",
                id,
                latest.id,
                plan.merged.ancestor,
            )
            .execute(&state.db)
            .await?;
        }
        match state
            .collab
            .rewrite_update(
                id,
                ticket,
                latest.number,
                sha256_hex(&plan.sql),
                plan.changes.clone(),
            )
            .await
            .map_err(hub_error)?
        {
            Ok(rewritten) => return Ok((plan, rewritten, latest)),
            Err(RewriteError::Changed) if attempt < ATTEMPTS => {
                content = state.collab.content(id).await.map_err(hub_error)?;
            }
            Err(RewriteError::Changed) => break,
            Err(err @ RewriteError::NotOpen(_)) => return Err(ApiError::internal(err)),
        }
    }
    Err(ApiError::Conflict {
        code: "draft_changed",
        message: "The SQL kept being rewritten while it was being updated. Try again.".into(),
    })
}

/// One version of the database, when there is one.
async fn version_of(
    state: &AppState,
    database_id: Uuid,
    version: Option<Uuid>,
) -> ApiResult<Option<Version>> {
    match version {
        Some(version) => versions::by_id(&state.db, database_id, version).await,
        None => Ok(None),
    }
}

struct Row {
    base_version_id: Option<Uuid>,
    base_sql: Option<String>,
    pending_base_version_id: Option<Uuid>,
    pending_base_sql: Option<String>,
}

/// The draft's base, if it is open (404 or 409 otherwise).
async fn read(state: &AppState, database_id: Uuid, id: Uuid) -> ApiResult<Row> {
    let draft = sqlx::query!(
        r#"SELECT status AS "status: DraftStatus", base_version_id, base_sql,
                  pending_base_version_id, pending_base_sql
           FROM drafts WHERE id = $1 AND database_id = $2 AND status <> 'deleted'"#,
        id,
        database_id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(no_such_draft)?;
    if draft.status != DraftStatus::Open {
        return Err(not_open(draft.status));
    }
    Ok(Row {
        base_version_id: draft.base_version_id,
        base_sql: draft.base_sql,
        pending_base_version_id: draft.pending_base_version_id,
        pending_base_sql: draft.pending_base_sql,
    })
}

/// Nothing newer than the draft's base: what it is now.
async fn up_to_date(
    state: &AppState,
    database_id: Uuid,
    id: Uuid,
    base: Option<Uuid>,
) -> ApiResult<Json<UpdatedDraft>> {
    let content = state.collab.content(id).await.map_err(hub_error)?;
    let base_version = match base {
        Some(base) => versions::by_id(&state.db, database_id, base)
            .await?
            .map(|v| v.number),
        None => None,
    };
    Ok(Json(UpdatedDraft {
        base_version,
        conflicts: count(&content.sql),
        changed: false,
    }))
}

/// What an update changes, computed off the async workers (parsing and
/// rendering a few hundred tables takes a moment).
struct Plan {
    merged: Merged,
    /// The text merged (the frozen draft's).
    sql: String,
    /// From `sql` to the merged text, as the smallest edit.
    changes: Vec<patch::Change>,
    layout: Vec<(String, Option<TablePosition>)>,
    groups: Vec<(String, Option<TableGroup>)>,
    conflicts: u32,
}

impl Plan {
    fn new(bases: &Bases, latest: &Version, content: Content) -> ApiResult<Self> {
        let label = format!("v{}", latest.number);
        let merged = merge_sql(
            bases.sql.as_ref(),
            latest,
            bases.ancestor.clone(),
            &content,
            &label,
        )?;
        let base_layout = bases
            .layout
            .as_ref()
            .map(|b| VersionLayout::from_value(&b.layout))
            .unwrap_or_default();
        let theirs = VersionLayout::from_value(&latest.layout);
        let (layout, groups) = merge_layout(&base_layout, &theirs, &content.layout);
        Ok(Self {
            changes: patch::changes(&content.sql, &merged.text),
            conflicts: u32::try_from(merged.conflicts).unwrap_or(u32::MAX),
            merged,
            sql: content.sql,
            layout,
            groups,
        })
    }
}

/// An update tries this many times when a server rewrite slips in.
const ATTEMPTS: usize = 3;

fn count(sql: &str) -> u32 {
    u32::try_from(conflicts(sql).0.len()).unwrap_or(u32::MAX)
}

/// The SQL merge, with its inputs: the text the draft is relative to, the
/// latest version's text, the three models as `render_sql` writes them.
fn merge_sql(
    base: Option<&Version>,
    latest: &Version,
    base_sql: Option<String>,
    content: &Content,
    label: &str,
) -> ApiResult<Merged> {
    let ancestor = match (base_sql, base) {
        (Some(sql), _) => sql,
        (None, Some(base)) => base.sql()?,
        (None, None) => String::new(),
    };
    let base_model_sql = match base {
        Some(base) => render_sql(&base.schema()?),
        None => String::new(),
    };
    let theirs_model_sql = render_sql(&latest.schema()?);
    let theirs = latest.sql()?;
    let ours_model_sql = match &content.model {
        Some(model) => Some(render_sql(
            &Schema::deserialize(&**model).map_err(ApiError::internal)?,
        )),
        None => None,
    };
    merge(&Input {
        ancestor: &ancestor,
        ours: &content.sql,
        theirs: &theirs,
        base_model_sql: &base_model_sql,
        theirs_model_sql: &theirs_model_sql,
        ours_model_sql: ours_model_sql.as_deref(),
        label,
    })
    .map_err(|err| match err {
        MergeError::Unparsable(diagnostics) => ApiError::Unprocessable {
            code: "draft_has_errors",
            message: "The draft's SQL has syntax errors. Fix them, then update it.".into(),
            details: Some(json!({ "diagnostics": diagnostics })),
        },
        // A bug of the merge's: logged, nothing applied.
        MergeError::Overlap => ApiError::internal(err),
    })
}

type LayoutOps = Vec<(String, Option<TablePosition>)>;
type GroupOps = Vec<(String, Option<TableGroup>)>;

/// Layout and groups, key by key, as changes to ours. What only one side
/// changed comes from that side. When both did:
///
/// - **a position stays ours**: the people in the draft are arranging it
///   right now, and moving tables under them would undo their work for a
///   cosmetic difference; the version's arrangement is still in its history;
/// - **a group merges field by field**: a name or color only one side
///   changed comes from it (theirs wins when both did); a member either
///   side removed stays removed, a member either side added comes in. A
///   group one side deleted and the other changed stays, changed. A table
///   never ends up in two groups: it stays where ours had it.
fn merge_layout(
    base: &VersionLayout,
    theirs: &VersionLayout,
    ours: &VersionLayout,
) -> (LayoutOps, GroupOps) {
    let positions = merge_maps(&base.layout, &theirs.layout, &ours.layout, |_, o, _| {
        Some(*o)
    });
    let mut groups = merge_maps(&base.groups, &theirs.groups, &ours.groups, |b, o, t| {
        let field = |get: fn(&TableGroup) -> &String| {
            match b {
                Some(b) if get(t) == get(b) => get(o),
                _ => get(t),
            }
            .clone()
        };
        let before: &[String] = b.map_or(&[], |b| &b.members);
        let removed_by_theirs = |m: &String| before.contains(m) && !t.members.contains(m);
        let mut members: Vec<String> = o
            .members
            .iter()
            .filter(|m| !removed_by_theirs(m))
            .cloned()
            .collect();
        members.extend(
            t.members
                .iter()
                .filter(|m| !o.members.contains(m) && !before.contains(m))
                .cloned(),
        );
        Some(TableGroup {
            name: field(|g| &g.name),
            color: field(|g| &g.color),
            members,
        })
    });
    // One group per table: where ours had it, else the first that has it.
    let mut first: BTreeMap<String, String> = BTreeMap::new();
    for (id, group) in &groups {
        for member in &group.members {
            first.entry(member.clone()).or_insert_with(|| id.clone());
        }
    }
    let home = |member: &String| -> String {
        ours.groups
            .iter()
            .find(|(id, g)| g.members.contains(member) && groups_has(&groups, id, member))
            .map_or_else(|| first[member].clone(), |(id, _)| id.clone())
    };
    let homes: BTreeMap<String, String> = first.keys().map(|m| (m.clone(), home(m))).collect();
    for (id, group) in &mut groups {
        group.members.retain(|m| homes[m] == *id);
    }
    (diff(&ours.layout, &positions), diff(&ours.groups, &groups))
}

fn groups_has(groups: &BTreeMap<String, TableGroup>, id: &str, member: &String) -> bool {
    groups.get(id).is_some_and(|g| g.members.contains(member))
}

/// Three-way per key; `both` decides keys both sides changed (from base,
/// ours, theirs; `None`: remove). A key one side removed and the other changed stays, changed.
fn merge_maps<T: Clone + PartialEq>(
    base: &BTreeMap<String, T>,
    theirs: &BTreeMap<String, T>,
    ours: &BTreeMap<String, T>,
    both: impl Fn(Option<&T>, &T, &T) -> Option<T>,
) -> BTreeMap<String, T> {
    let keys: BTreeSet<&String> = base
        .keys()
        .chain(theirs.keys())
        .chain(ours.keys())
        .collect();
    let mut out = BTreeMap::new();
    for key in keys {
        let (b, t, o) = (base.get(key), theirs.get(key), ours.get(key));
        let value = if t == b {
            o.cloned()
        } else if o == b || o == t {
            t.cloned()
        } else {
            match (o, t) {
                (Some(o), Some(t)) => both(b, o, t),
                (Some(kept), None) | (None, Some(kept)) => Some(kept.clone()),
                (None, None) => None,
            }
        };
        if let Some(value) = value {
            out.insert(key.clone(), value);
        }
    }
    out
}

/// What turns `ours` into `merged`, key by key.
fn diff<T: Clone + PartialEq>(
    ours: &BTreeMap<String, T>,
    merged: &BTreeMap<String, T>,
) -> Vec<(String, Option<T>)> {
    let mut ops: Vec<(String, Option<T>)> = merged
        .iter()
        .filter(|(key, value)| ours.get(*key) != Some(value))
        .map(|(key, value)| (key.clone(), Some(value.clone())))
        .collect();
    ops.extend(
        ours.keys()
            .filter(|key| !merged.contains_key(*key))
            .map(|key| (key.clone(), None)),
    );
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f64) -> TablePosition {
        TablePosition { x, y: 0.0 }
    }

    fn layout(positions: &[(&str, f64)], groups: &[(&str, &str, &[&str])]) -> VersionLayout {
        VersionLayout {
            layout: positions
                .iter()
                .map(|(k, x)| ((*k).to_owned(), at(*x)))
                .collect(),
            groups: groups
                .iter()
                .map(|(id, name, members)| {
                    (
                        (*id).to_owned(),
                        TableGroup {
                            name: (*name).to_owned(),
                            color: "blue".to_owned(),
                            members: members.iter().map(|m| (*m).to_owned()).collect(),
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn positions_come_from_whoever_moved_them_and_stay_ours_when_both_did() {
        let base = layout(&[("a", 0.0), ("b", 0.0), ("c", 0.0), ("gone", 0.0)], &[]);
        let theirs = layout(&[("a", 1.0), ("b", 0.0), ("c", 5.0), ("new", 9.0)], &[]);
        let ours = layout(&[("a", 0.0), ("b", 2.0), ("c", 7.0), ("gone", 0.0)], &[]);
        let (positions, groups) = merge_layout(&base, &theirs, &ours);
        assert_eq!(
            positions,
            [
                ("a".to_owned(), Some(at(1.0))),
                ("new".to_owned(), Some(at(9.0))),
                ("gone".to_owned(), None),
            ]
        );
        assert_eq!(groups, []);
    }

    #[test]
    fn group_members_either_side_removed_stay_removed() {
        let base = layout(&[], &[("g", "Core", &["a", "b"])]);
        // Theirs took b out; ours renamed the group and added c.
        let theirs = layout(&[], &[("g", "Core", &["a"])]);
        let ours = layout(&[], &[("g", "Billing", &["a", "b", "c"])]);
        let (_, groups) = merge_layout(&base, &theirs, &ours);
        assert_eq!(
            groups,
            [(
                "g".to_owned(),
                Some(TableGroup {
                    name: "Billing".to_owned(),
                    color: "blue".to_owned(),
                    members: vec!["a".to_owned(), "c".to_owned()],
                })
            )]
        );
    }

    #[test]
    fn groups_take_theirs_names_and_both_sides_members() {
        let base = layout(&[], &[("g", "Core", &["a"]), ("h", "Old", &["c"])]);
        let theirs = layout(&[], &[("g", "Billing", &["a", "b"]), ("h", "Old", &["c"])]);
        let ours = layout(
            &[],
            &[("g", "Core", &["a", "d"]), ("h", "Old", &["c", "b"])],
        );
        let (_, groups) = merge_layout(&base, &theirs, &ours);
        // b stays in h, where ours put it.
        assert_eq!(
            groups,
            [(
                "g".to_owned(),
                Some(TableGroup {
                    name: "Billing".to_owned(),
                    color: "blue".to_owned(),
                    members: vec!["a".to_owned(), "d".to_owned()],
                })
            )]
        );
    }
}
