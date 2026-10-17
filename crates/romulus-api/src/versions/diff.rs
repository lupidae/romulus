//! Two versions of a database compared: `GET …/databases/{db}/diff?from=&to=`
//! [member]. What the diff page draws, in one response: romulus-diff's
//! semantic diff (change list and status map), both models (the graph draws
//! their union), both models as SQL (the side-by-side text diff) and the
//! layout of the union.
//!
//! Versions never change, so neither does a diff between two of them: its
//! serialised content (everything but the two summaries, whose authors can
//! be renamed or deleted) is kept in [`DiffCache`], keyed by the two version
//! ids (not their numbers: a database deleted and created again under the
//! same slug numbers its versions from 1 again). A request resolves the
//! numbers to summaries first, and loads the full versions only on a miss.

use std::{
    collections::{HashSet, VecDeque},
    sync::Mutex,
};

use axum::{
    body::Bytes,
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};
use remus_core::Schema;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{VersionDetail, VersionSummary, detail, layout::VersionLayout, missing, page};
use crate::{
    ApiError, ApiResult, AppState,
    auth::Member,
    compile, databases,
    error::ErrorBody,
    extract::{Path, Query},
    orgs::invalid,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(diff))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DiffVersions {
    /// The version compared from: the left side, `before` in the diff.
    pub from: i32,
    /// The version compared to: the right side, `after`. May equal `from`
    /// (nothing changed), or be older (the diff then reads backwards).
    pub to: i32,
}

/// `from` compared with `to`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VersionDiff {
    pub from: VersionSummary,
    pub to: VersionSummary,
    /// From `from`'s model to `to`'s: the change list, the summary and the
    /// status of every object for the graph.
    pub diff: romulus_diff::SchemaDiff,
    /// Both models: the graph draws their union, styled by `diff.status`.
    pub models: DiffModels,
    /// Both models as SQL, rendered the same way whatever their source (an
    /// upload's text or a push's JSON): canonical order, and the newer
    /// version's header on both, so a line diff of the two shows only what
    /// the schema changed.
    pub sql: DiffSql,
    /// Where the union's tables sit: the newer version's positions and
    /// groups; the older one's positions for tables the newer did not place
    /// (the ones it removed, say), each in its old group if the newer one
    /// still has it, or in its old group alone if not.
    pub layout: VersionLayout,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DiffModels {
    /// `from`'s remus model.
    #[schema(value_type = Object)]
    pub before: serde_json::Value,
    /// `to`'s remus model.
    #[schema(value_type = Object)]
    pub after: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DiffSql {
    pub before: String,
    pub after: String,
}

/// Compares two versions: what changed from `from` to `to`, three ways.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/diff",
    operation_id = "diff_versions",
    tag = "versions",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
        DiffVersions,
    ),
    responses(
        (status = 200, body = VersionDiff),
        (status = 400, description = "`bad_request`: a number that is not one", body = ErrorBody),
        (status = 404, description = "`not_found`: no such database, or no such version", body = ErrorBody),
        (status = 422, description = "`validation`: a number below 1", body = ErrorBody),
    )
)]
async fn diff(
    member: Member,
    State(state): State<AppState>,
    Path((_, slug)): Path<(String, String)>,
    Query(query): Query<DiffVersions>,
) -> ApiResult<Response> {
    for (field, number) in [("from", query.from), ("to", query.to)] {
        if number < 1 {
            return Err(invalid(field, "Version numbers start at 1."));
        }
    }
    let database = databases::find(&state.db, member.org.id, &slug).await?;
    let (from, to) = summaries(&state.db, database.id, query.from, query.to).await?;
    let key = (from.id, to.id);
    let content = if let Some(content) = state.diffs.get(key) {
        content
    } else {
        // Only now the full versions (models, layouts): a hit needs neither.
        let before = detail(&state.db, database.id, Some(query.from)).await?;
        let after = if query.to == query.from {
            before.clone()
        } else {
            detail(&state.db, database.id, Some(query.to)).await?
        };
        // Two renders and a diff of a few hundred tables: milliseconds, off
        // the async workers all the same.
        let content = tokio::task::spawn_blocking(move || compare(before, after))
            .await
            .map_err(ApiError::internal)??;
        state.diffs.insert(key, content.clone());
        content
    };
    let body = with_versions(&from, &to, &content)?;
    Ok(([(header::CONTENT_TYPE, "application/json")], body).into_response())
}

/// Both versions' summaries, authors as they are now; 404 for a number
/// the database does not have.
async fn summaries(
    db: &sqlx::PgPool,
    database_id: Uuid,
    from: i32,
    to: i32,
) -> ApiResult<(VersionSummary, VersionSummary)> {
    let one = async |number: i32| -> ApiResult<VersionSummary> {
        // The history's query: the newest version below number + 1.
        let rows = match number.checked_add(1) {
            Some(below) => page(db, database_id, Some(below), 1).await?,
            None => Vec::new(),
        };
        match rows.into_iter().next().filter(|row| row.number == number) {
            Some(row) => Ok(row.into()),
            // "v3 was removed.", or that there is none.
            None => Err(missing(db, database_id, Some(number)).await),
        }
    };
    let from_summary = one(from).await?;
    let to_summary = if to == from {
        from_summary.clone()
    } else {
        one(to).await?
    };
    Ok((from_summary, to_summary))
}

/// What a diff holds besides the two summaries: what is cached.
#[derive(Serialize)]
struct DiffContent {
    diff: romulus_diff::SchemaDiff,
    models: DiffModels,
    sql: DiffSql,
    layout: VersionLayout,
}

/// The cached content, serialised (a JSON object).
fn compare(from: VersionDetail, to: VersionDetail) -> ApiResult<Bytes> {
    let before = Schema::deserialize(&from.model).map_err(ApiError::internal)?;
    let after = Schema::deserialize(&to.model).map_err(ApiError::internal)?;
    let diff = romulus_diff::diff(&before, &after);
    // The newer version decides what is not the schema: the SQL's header,
    // the layout. Either way round, a pair reads the same.
    let from_is_newer = from.number > to.number;
    let newer = if from_is_newer { &before } else { &after };
    let sql = DiffSql {
        before: sql_like(&before, newer),
        after: sql_like(&after, newer),
    };
    let layout = if from_is_newer {
        union(from.layout.clone(), to.layout.clone(), &before, &after)
    } else {
        union(to.layout.clone(), from.layout.clone(), &after, &before)
    };
    let content = DiffContent {
        diff,
        models: DiffModels {
            before: from.model,
            after: to.model,
        },
        sql,
        layout,
    };
    serde_json::to_vec(&content)
        .map(Bytes::from)
        .map_err(ApiError::internal)
}

/// The response, a [`VersionDiff`]: `{"from": …, "to": …}` joined with the
/// cached content's object, without parsing it again.
fn with_versions(from: &VersionSummary, to: &VersionSummary, content: &[u8]) -> ApiResult<Bytes> {
    #[derive(Serialize)]
    struct Versions<'a> {
        from: &'a VersionSummary,
        to: &'a VersionSummary,
    }
    let head = serde_json::to_vec(&Versions { from, to }).map_err(ApiError::internal)?;
    let (Some((b'}', head)), Some((b'{', rest))) = (head.split_last(), content.split_first())
    else {
        return Err(ApiError::internal(std::io::Error::other(
            "a diff's parts are not JSON objects",
        )));
    };
    let mut body = Vec::with_capacity(head.len() + rest.len() + 1);
    body.extend_from_slice(head);
    // `{}` has nothing to add after the comma.
    if rest != b"}" {
        body.push(b',');
    }
    body.extend_from_slice(rest);
    Ok(Bytes::from(body))
}

/// `schema` as SQL in canonical order (`romulus_diff::normalize`), with the
/// metadata of `like`: the header names the database and server version,
/// which differ between an upload (compiled in the sandbox) and a push (from
/// wherever remus ran) without the schema differing. Rendered as a JSON
/// version's SQL tab is (`compile::render_sql`). A draft's publish preview
/// renders its two sides with it too, `like` being the draft's model.
pub(crate) fn sql_like(schema: &Schema, like: &Schema) -> String {
    let mut schema = romulus_diff::normalize(schema);
    like.remus_version.clone_into(&mut schema.remus_version);
    like.database.clone_into(&mut schema.database);
    like.server_version.clone_into(&mut schema.server_version);
    schema.generated_at = None;
    compile::render_sql(&schema)
}

/// The layout of the union of two models: `newer`'s, then `older`'s
/// positions for tables `newer` has none for. A table only the older model
/// has goes back into the group it was in: `newer`'s group of the same id, or
/// the old group itself (holding only such tables) when `newer` dropped it.
fn union(
    mut newer: VersionLayout,
    older: VersionLayout,
    newer_model: &Schema,
    older_model: &Schema,
) -> VersionLayout {
    for (name, position) in older.layout {
        newer.layout.entry(name).or_insert(position);
    }
    let kept: HashSet<String> = newer_model
        .entities
        .iter()
        .map(remus_core::model::Entity::qualified_name)
        .collect();
    let gone: HashSet<String> = older_model
        .entities
        .iter()
        .map(remus_core::model::Entity::qualified_name)
        .filter(|name| !kept.contains(name))
        .collect();
    for (id, mut group) in older.groups {
        group.members.retain(|name| gone.contains(name));
        if group.members.is_empty() {
            continue;
        }
        match newer.groups.get_mut(&id) {
            Some(current) => {
                for name in group.members {
                    if !current.members.contains(&name) {
                        current.members.push(name);
                    }
                }
            }
            None => {
                newer.groups.insert(id, group);
            }
        }
    }
    newer
}

/// Version ids compared from and to.
type Key = (Uuid, Uuid);

/// Serialised diffs, most recently used first, within a byte budget: a diff
/// of two 300-table versions is a few hundred kilobytes (two models, two
/// SQL texts), so the default keeps the last few dozen comparisons.
pub struct DiffCache {
    entries: Mutex<VecDeque<(Key, Bytes)>>,
    budget: usize,
}

impl Default for DiffCache {
    fn default() -> Self {
        Self::with_budget(32 * 1024 * 1024)
    }
}

impl DiffCache {
    /// Keeps at most `budget` bytes of responses (and always the last one).
    pub fn with_budget(budget: usize) -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            budget,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<(Key, Bytes)>> {
        // Plain data, valid at every step: a panic elsewhere leaves it usable.
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn get(&self, key: Key) -> Option<Bytes> {
        let mut entries = self.lock();
        let at = entries.iter().position(|(k, _)| *k == key)?;
        let entry = entries.remove(at)?;
        let body = entry.1.clone();
        entries.push_front(entry);
        Some(body)
    }

    pub fn insert(&self, key: Key, body: Bytes) {
        let mut entries = self.lock();
        entries.retain(|(k, _)| *k != key);
        entries.push_front((key, body));
        let mut total: usize = entries.iter().map(|(_, b)| b.len()).sum();
        while total > self.budget && entries.len() > 1 {
            if let Some((_, evicted)) = entries.pop_back() {
                total -= evicted.len();
            }
        }
    }

    /// How many diffs are kept.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::domain::{SourceKind, VersionOrigin};

    fn model(tables: &[&str]) -> Schema {
        let entities: Vec<_> = tables
            .iter()
            .map(|name| {
                json!({ "schema": "public", "name": name, "kind": "r", "comment": null,
                        "partition_key": null, "view_definition": null, "columns": [] })
            })
            .collect();
        Schema::from_json(
            &json!({ "remus_version": "0.2", "database": "app", "server_version": "17",
                     "entities": entities })
            .to_string(),
        )
        .unwrap()
    }

    fn key(n: u128) -> Key {
        (Uuid::from_u128(n), Uuid::from_u128(n + 1))
    }

    #[test]
    fn the_cache_forgets_the_least_recently_used_diff_past_its_budget() {
        let cache = DiffCache::with_budget(10);
        cache.insert(key(1), Bytes::from_static(b"aaaa"));
        cache.insert(key(2), Bytes::from_static(b"bbbb"));
        // Reading the first makes the second the oldest.
        assert_eq!(cache.get(key(1)).as_deref(), Some(&b"aaaa"[..]));
        cache.insert(key(3), Bytes::from_static(b"cccc"));
        assert_eq!(cache.len(), 2);
        assert!(cache.get(key(2)).is_none());
        assert!(cache.get(key(1)).is_some());
        assert!(cache.get(key(3)).is_some());
    }

    #[test]
    fn the_cache_keeps_the_last_diff_even_past_its_budget() {
        let cache = DiffCache::with_budget(2);
        cache.insert(key(1), Bytes::from_static(b"aaaa"));
        assert!(cache.get(key(1)).is_some());
        cache.insert(key(1), Bytes::from_static(b"bbbb"));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(key(1)).as_deref(), Some(&b"bbbb"[..]));
    }

    #[test]
    fn the_union_layout_is_the_newer_one_with_the_older_ones_positions_for_the_rest() {
        let newer = VersionLayout::from_value(&json!({
            "layout": { "public.users": { "x": 10, "y": 10 } },
            "groups": { "g1": { "name": "Core", "color": "blue", "members": ["public.users"] } }
        }));
        let older = VersionLayout::from_value(&json!({
            "layout": {
                "public.users": { "x": 500, "y": 500 },
                "public.orders": { "x": 300, "y": 0 },
                "public.legacy": { "x": 0, "y": 300 }
            },
            "groups": {
                "g1": { "name": "Old name", "color": "red", "members": ["public.users", "public.orders"] },
                "g2": { "name": "Legacy", "color": "stone", "members": ["public.legacy", "public.users"] }
            }
        }));
        // `orders` and `legacy` were removed; `audit` is new and unplaced.
        let union = union(
            newer,
            older,
            &model(&["users", "audit"]),
            &model(&["users", "orders", "legacy"]),
        );
        assert_eq!(
            serde_json::to_value(&union).unwrap(),
            json!({
                "layout": {
                    "public.legacy": { "x": 0.0, "y": 300.0 },
                    "public.orders": { "x": 300.0, "y": 0.0 },
                    "public.users": { "x": 10.0, "y": 10.0 }
                },
                "groups": {
                    // The newer group, joined by the table it lost.
                    "g1": { "name": "Core", "color": "blue",
                            "members": ["public.users", "public.orders"] },
                    // A dropped group comes back for its removed tables only.
                    "g2": { "name": "Legacy", "color": "stone", "members": ["public.legacy"] }
                }
            })
        );
    }

    #[test]
    fn the_response_is_the_summaries_joined_with_the_cached_content() {
        let summary = |number| VersionSummary {
            id: Uuid::from_u128(number),
            number: i32::try_from(number).unwrap(),
            origin: VersionOrigin::Push,
            source_kind: SourceKind::RemusJson,
            message: Some("From CI".into()),
            created_at: chrono::DateTime::UNIX_EPOCH,
            author: None,
            written_migration: false,
            postgres: crate::versions::VersionPostgres::new(
                VersionOrigin::Push,
                Some("17.11".into()),
                Some(17),
                17,
            ),
        };
        let content = DiffContent {
            diff: romulus_diff::diff(&model(&["a"]), &model(&["a", "b"])),
            models: DiffModels {
                before: json!({ "a": 1 }),
                after: json!({ "b": 2 }),
            },
            sql: DiffSql {
                before: "x".into(),
                after: "y".into(),
            },
            layout: VersionLayout::default(),
        };
        let bytes = serde_json::to_vec(&content).unwrap();
        let body = with_versions(&summary(1), &summary(2), &bytes).unwrap();
        let whole = VersionDiff {
            from: summary(1),
            to: summary(2),
            diff: content.diff.clone(),
            models: content.models.clone(),
            sql: content.sql.clone(),
            layout: content.layout.clone(),
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::to_value(&whole).unwrap()
        );
    }

    #[test]
    fn the_sql_of_both_sides_has_the_same_header_and_order() {
        let mut uploaded = model(&["plans", "invoices"]);
        uploaded.database = "billing".into();
        let mut pushed = model(&["invoices", "plans"]);
        "16.4".clone_into(&mut pushed.server_version);
        let before = sql_like(&uploaded, &pushed);
        assert_eq!(before, sql_like(&pushed, &pushed));
        let invoices = before.find("CREATE TABLE public.invoices").unwrap();
        let plans = before.find("CREATE TABLE public.plans").unwrap();
        assert!(invoices < plans, "{before}");
        assert!(before.contains("from app (PostgreSQL 16.4)"), "{before}");
    }
}
