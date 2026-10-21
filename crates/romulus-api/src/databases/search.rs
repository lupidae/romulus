//! Searching an organization's schemas: `GET /orgs/{org}/search?q=&limit=`
//! [member]. Tables (views, materialized views, foreign tables), columns and
//! enums of each live database's latest version whose name or comment holds
//! the query (any case; an enum's labels too). Exact names first, then
//! names starting with it, then names holding it, then comments. One query
//! over the stored models: nothing is compiled or loaded into the API.

use axum::extract::State;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::{IntoParams, ToSchema};

use crate::{
    ApiResult,
    auth::Member,
    error::ErrorBody,
    extract::{Json, Query},
    orgs::invalid,
};

/// The query's length, in characters.
const MAX_QUERY: usize = 100;

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchQuery {
    /// What to look for: part of a name or of a comment.
    pub q: String,
    /// 1 to 200 hits; 50 by default.
    pub limit: Option<i64>,
}

/// What matched, where.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SchemaHit {
    /// `table`, `view`, `materialized_view`, `foreign_table`, `column` or
    /// `enum`.
    pub kind: String,
    /// The database's slug, and its name.
    pub database: String,
    pub database_name: String,
    /// The version searched: the database's latest.
    pub version: i32,
    pub schema: String,
    /// The table (or view…) the hit is or is in; the enum's name.
    pub name: String,
    /// The column, for a column.
    pub column: Option<String>,
    /// A column's type; an enum's labels, comma-separated.
    pub r#type: Option<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SearchResults {
    pub hits: Vec<SchemaHit>,
    /// More matched than `limit`.
    pub truncated: bool,
}

/// Tables, columns and enums of the organization's databases matching `q`.
#[utoipa::path(
    get,
    path = "/orgs/{org}/search",
    operation_id = "search_schemas",
    tag = "databases",
    params(("org" = String, Path, description = "The organization's slug"), SearchQuery),
    responses(
        (status = 200, body = SearchResults),
        (status = 404, body = ErrorBody),
        (status = 422, body = ErrorBody),
    )
)]
pub(super) async fn search(
    member: Member,
    State(db): State<PgPool>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<SearchResults>> {
    let q = query.q.trim();
    if q.is_empty() {
        return Err(invalid("q", "Say what to look for."));
    }
    if q.chars().count() > MAX_QUERY {
        return Err(invalid("q", "Keep the search to 100 characters or fewer."));
    }
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let mut hits = sqlx::query_as!(
        SchemaHit,
        r#"WITH latest AS (
               SELECT DISTINCT ON (v.database_id) d.slug, d.name AS database_name, v.number, v.model
               FROM versions v JOIN databases d ON d.id = v.database_id
               WHERE d.org_id = $1 AND d.deleted_at IS NULL AND v.removed_at IS NULL
               ORDER BY v.database_id, v.number DESC
           ),
           entities AS (
               SELECT l.slug, l.database_name, l.number, e.value AS entity
               FROM latest l, jsonb_array_elements(coalesce(l.model -> 'entities', '[]')) e
           ),
           hits AS (
               SELECT CASE entity ->> 'kind' WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized_view'
                                             WHEN 'f' THEN 'foreign_table' ELSE 'table' END AS kind,
                      slug, database_name, number, entity ->> 'schema' AS schema,
                      entity ->> 'name' AS name, NULL::text AS column_name, NULL::text AS type_name,
                      entity ->> 'comment' AS comment, entity ->> 'name' AS matched
               FROM entities
               UNION ALL
               SELECT 'column', slug, database_name, number, entity ->> 'schema', entity ->> 'name',
                      c ->> 'name', c ->> 'type', c ->> 'comment', c ->> 'name'
               FROM entities, jsonb_array_elements(coalesce(entity -> 'columns', '[]')) c
               UNION ALL
               SELECT 'enum', l.slug, l.database_name, l.number, e ->> 'schema', e ->> 'name', NULL,
                      (SELECT string_agg(label, ', ') FROM jsonb_array_elements_text(coalesce(e -> 'labels', '[]')) label),
                      e ->> 'comment', e ->> 'name'
               FROM latest l, jsonb_array_elements(coalesce(l.model -> 'enums', '[]')) e
           )
           SELECT kind AS "kind!", slug AS "database!", database_name AS "database_name!",
                  number AS "version!", schema AS "schema!", name AS "name!",
                  column_name AS "column", type_name AS "type", comment
           FROM hits
           WHERE strpos(lower(matched), lower($2)) > 0
              OR strpos(lower(coalesce(comment, '')), lower($2)) > 0
              OR (kind = 'enum' AND strpos(lower(coalesce(type_name, '')), lower($2)) > 0)
           ORDER BY CASE WHEN lower(matched) = lower($2) THEN 0
                         WHEN starts_with(lower(matched), lower($2)) THEN 1
                         WHEN strpos(lower(matched), lower($2)) > 0 THEN 2
                         ELSE 3 END,
                    slug, schema, name, column_name NULLS FIRST
           LIMIT $3"#,
        member.org.id,
        q,
        limit + 1,
    )
    .fetch_all(&db)
    .await?;
    let truncated = hits.len() > usize::try_from(limit).unwrap_or(usize::MAX);
    hits.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    Ok(Json(SearchResults { hits, truncated }))
}
