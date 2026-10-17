//! Moving a database to another Postgres major (docs/MVP.md, "Several
//! Postgres majors").
//!
//! Endpoints [developer]:
//! - `POST /orgs/{org}/databases/{db}/major/check` `{pg_major}`: the dry
//!   run. The latest version's SQL (its source, or its model rendered for a
//!   push or an import) compiled on that major: whether it compiles, its
//!   errors located in that text, and how its model reads there.
//! - `PUT /orgs/{org}/databases/{db}/major` `{pg_major}`: the switch. The
//!   same compile (the sandbox serves it from its cache after a check);
//!   refused with 422 `incompatible_major` and the errors when it does not
//!   compile, 409 `latest_changed` when a version arrived meanwhile. Then
//!   `databases.pg_major` changes, an audit event says from what to what, and
//!   the database's open drafts compile on the new major.
//!
//! Versions keep the major they were compiled on (`versions.pg_major`):
//! nothing is recompiled but the dry run. A database with no version moves
//! freely.

use axum::extract::State;
use remus_core::Schema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{Database, DatabaseRef, find, load_one_by_id};
use crate::{
    ApiError, ApiResult, AppState,
    audit::{self, AuditDetails, Entry},
    auth::{Member, min},
    compile::{self, Diagnostic},
    domain::{AuditAction, PgMajor, SourceKind},
    error::ErrorBody,
    extract::{Json, Path},
    versions::Changes,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(check))
        .routes(routes!(switch))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ChangeMajor {
    pub pg_major: PgMajor,
}

/// Whether the database's latest version compiles on another major.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MajorCheck {
    /// The major checked.
    pub pg_major: PgMajor,
    /// The version compiled: the latest. `null` when the database has none
    /// (nothing to check: it may move).
    pub against: Option<i32>,
    /// The major that version was compiled (or pushed) on; `null` when its
    /// model does not say.
    pub against_major: Option<i32>,
    /// It compiles on `pg_major`, or there is no version: the database may
    /// move there.
    pub compiles: bool,
    /// Errors (when it does not compile), then notes, located in `sql`.
    pub diagnostics: Vec<Diagnostic>,
    /// The SQL compiled, when it does not compile (the errors' lines are
    /// its): the version's source, or its model as SQL.
    pub sql: Option<String>,
    /// When it compiles: how its model reads on `pg_major` against the
    /// version's (`no changes`, most often).
    pub changes: Option<Changes>,
}

/// The latest live version, as the check needs it.
struct Latest {
    id: Uuid,
    number: i32,
    pg_major: Option<i16>,
    sql: String,
    model: Schema,
}

async fn latest(state: &AppState, database: &DatabaseRef) -> ApiResult<Option<Latest>> {
    let row = sqlx::query!(
        r#"SELECT id, number, pg_major, source, source_kind AS "source_kind: SourceKind", model
           FROM versions WHERE database_id = $1 AND removed_at IS NULL
           ORDER BY number DESC LIMIT 1"#,
        database.id,
    )
    .fetch_optional(&state.db)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    // Parsing and rendering a large model: off the async workers.
    let latest = tokio::task::spawn_blocking(move || {
        let model = Schema::deserialize(&row.model).map_err(ApiError::internal)?;
        let sql = match row.source_kind {
            SourceKind::Sql => row.source,
            SourceKind::RemusJson | SourceKind::Migration => compile::render_sql(&model),
        };
        Ok::<_, ApiError>(Latest {
            id: row.id,
            number: row.number,
            pg_major: row.pg_major,
            sql,
            model,
        })
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(Some(latest))
}

/// The dry run: `latest` compiled on `major`.
async fn dry_run(
    state: &AppState,
    member: &Member<min::Developer>,
    database: &DatabaseRef,
    latest: Option<Latest>,
    major: PgMajor,
) -> ApiResult<MajorCheck> {
    let Some(latest) = latest else {
        return Ok(MajorCheck {
            pg_major: major,
            against: None,
            against_major: None,
            compiles: true,
            diagnostics: Vec::new(),
            sql: None,
            changes: None,
        });
    };
    state.limits.compile.check(member.user.id)?;
    state.limits.database_compiles.check(database.id)?;
    let compiled = state.compiler.compile(major, &latest.sql).await?;
    let (number, against_major) = (latest.number, latest.pg_major.map(i32::from));
    let Some(model) = compiled.model else {
        return Ok(MajorCheck {
            pg_major: major,
            against: Some(number),
            against_major,
            compiles: false,
            diagnostics: compiled.diagnostics,
            sql: Some(latest.sql),
            changes: None,
        });
    };
    let changes = tokio::task::spawn_blocking(move || {
        let diff = romulus_diff::diff(&latest.model, &model);
        Changes {
            against: Some(number),
            unchanged: diff.is_empty(),
            summary: diff.summary.text,
        }
    })
    .await
    .map_err(ApiError::internal)?;
    Ok(MajorCheck {
        pg_major: major,
        against: Some(number),
        against_major,
        compiles: true,
        diagnostics: compiled.diagnostics,
        sql: None,
        changes: Some(changes),
    })
}

/// Compiles the latest version on another major, without changing
/// anything: what moving the database there would meet. Developers and
/// admins.
#[utoipa::path(
    post,
    path = "/orgs/{org}/databases/{db}/major/check",
    operation_id = "check_database_major",
    tag = "databases",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
    ),
    request_body = ChangeMajor,
    responses(
        (status = 200, body = MajorCheck),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 422, description = "`validation`: not a supported major; `major_unavailable`: this server has no sandbox for it", body = ErrorBody),
        (status = 429, description = "`rate_limited`", body = ErrorBody),
        (status = 503, description = "`unavailable`: that major's compiler is down", body = ErrorBody),
    )
)]
async fn check(
    member: Member<min::Developer>,
    State(state): State<AppState>,
    Path((_, slug)): Path<(String, String)>,
    Json(body): Json<ChangeMajor>,
) -> ApiResult<Json<MajorCheck>> {
    let database = find(&state.db, member.org.id, &slug).await?;
    state.compiler.require_served(body.pg_major)?;
    let latest = latest(&state, &database).await?;
    Ok(Json(
        dry_run(&state, &member, &database, latest, body.pg_major).await?,
    ))
}

/// Moves the database to another Postgres major, once its latest version
/// compiles there. Developers and admins.
#[utoipa::path(
    put,
    path = "/orgs/{org}/databases/{db}/major",
    operation_id = "change_database_major",
    tag = "databases",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
    ),
    request_body = ChangeMajor,
    responses(
        (status = 200, body = Database),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`latest_changed`: a version arrived meanwhile; check again", body = ErrorBody),
        (status = 422, description = "`incompatible_major` (`details`: `MajorCheck`): the latest version does not compile there; `major_unavailable`: this server has no sandbox for it; or `validation`", body = ErrorBody),
        (status = 429, description = "`rate_limited`", body = ErrorBody),
        (status = 503, description = "`unavailable`: that major's compiler is down", body = ErrorBody),
    )
)]
async fn switch(
    member: Member<min::Developer>,
    State(state): State<AppState>,
    Path((_, slug)): Path<(String, String)>,
    Json(body): Json<ChangeMajor>,
) -> ApiResult<Json<Database>> {
    let database = find(&state.db, member.org.id, &slug).await?;
    let to = body.pg_major;
    let from = database.pg_major;
    state.compiler.require_served(to)?;
    if to == from {
        return Ok(Json(
            load_one_by_id(&state.db, member.org.id, database.id).await?,
        ));
    }
    let latest = latest(&state, &database).await?;
    let checked = latest.as_ref().map(|l| l.id);
    let outcome = dry_run(&state, &member, &database, latest, to).await?;
    if !outcome.compiles {
        let errors = outcome.diagnostics.iter().filter(|d| d.is_error()).count();
        let version = outcome
            .against
            .map_or_else(|| "The latest version".to_owned(), |n| format!("v{n}"));
        return Err(ApiError::Unprocessable {
            code: "incompatible_major",
            message: match errors {
                0 | 1 => format!(
                    "{version} does not compile on Postgres {to}. Fix the error first, or stay on {from}."
                ),
                n => format!(
                    "{version} does not compile on Postgres {to}: {n} errors. Fix them first, or stay on {from}."
                ),
            },
            details: Some(json!(outcome)),
        });
    }

    let mut tx = state.db.begin().await?;
    // The version checked must still be the latest: one that arrived since
    // was not compiled on the new major.
    sqlx::query!(
        "SELECT id FROM databases WHERE id = $1 FOR NO KEY UPDATE",
        database.id
    )
    .fetch_one(&mut *tx)
    .await?;
    let now_latest = sqlx::query_scalar!(
        "SELECT id FROM versions WHERE database_id = $1 AND removed_at IS NULL
         ORDER BY number DESC LIMIT 1",
        database.id,
    )
    .fetch_optional(&mut *tx)
    .await?;
    if now_latest != checked {
        return Err(ApiError::Conflict {
            code: "latest_changed",
            message: "A new version arrived while checking. Check again.".to_owned(),
        });
    }
    let moved = sqlx::query!(
        "UPDATE databases SET pg_major = $2 WHERE id = $1 AND deleted_at IS NULL AND pg_major = $3",
        database.id,
        to as PgMajor,
        from as PgMajor,
    )
    .execute(&mut *tx)
    .await?;
    if moved.rows_affected() != 1 {
        return Err(ApiError::Conflict {
            code: "latest_changed",
            message: "Someone changed the database meanwhile. Check again.".to_owned(),
        });
    }
    audit::record(
        &mut *tx,
        &Entry {
            org_id: member.org.id,
            database_id: Some(database.id),
            action: AuditAction::MajorChanged,
            actor_id: Some(member.user.id),
            actor_name: Some(member.user.name.clone()),
            version_number: outcome.against,
            reason: None,
            details: AuditDetails {
                from: Some(i32::from(from.get())),
                to: Some(i32::from(to.get())),
                ..AuditDetails::default()
            },
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(org = %member.org.slug, database = %database.slug, %from, %to, "database moved to another Postgres major");
    // Its open drafts compile on the new major from now on.
    state
        .collab
        .major_changed(database.id, to, &member.user.name);
    Ok(Json(
        load_one_by_id(&state.db, member.org.id, database.id).await?,
    ))
}
