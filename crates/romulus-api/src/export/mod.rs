//! Data export (the hosted Terms, 17.1): everything a database holds, as
//! files, in a ZIP streamed as it is written (one version in memory at a
//! time, so an organization of any size fits).
//!
//! Endpoints (who may call them in brackets; see `auth::member`):
//! - `GET /orgs/{org}/databases/{db}/export` [member]: one database.
//! - `GET /orgs/{org}/export` [admin]: every live database of the
//!   organization, a folder each.
//!
//! On every plan and on self-hosted installs alike, never capped: versions
//! past the plan's history window are in it (export is how people get
//! everything). Removed versions are not (they are on their way out, and
//! restorable for 30 days; the manifest lists their numbers).
//!
//! A database's folder:
//!
//! ```text
//! manifest.json                         the database, its versions, what each file is
//! versions/v0007.sql                    the schema as SQL DDL (the SQL uploaded or
//!                                       published; rendered from the model otherwise)
//! versions/v0007.model.json             the remus model (lossless)
//! versions/v0007.layout.json            table positions and groups
//! versions/v0007.renames.json           the renames chosen when it was published
//! versions/v0007.source.json|.sql       what was pushed or imported, when it is not the DDL
//! versions/v0007.written-migration.sql  the migration it was written as (migration mode)
//! migrations/v0006-v0007.up.sql         the migration between consecutive versions,
//! migrations/v0006-v0007.down.sql       planned with the renames chosen at the time
//! comments.json                         every version's comment threads
//! drafts/01-<name>/schema.sql           each open draft: its SQL (and migration.sql
//!                  layout.json, …       in migration mode), layout, renames chosen,
//!                  comments.json        its discussion
//! ```
//!
//! Limited per person and per organization (`rate::Limits::exports`,
//! `org_exports`), and a few streaming at once (`export_slots`): each reads
//! every version and plans every migration.

use std::{
    io::Write as _,
    sync::{Arc, Mutex, PoisonError},
};

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use futures_util::StreamExt as _;
use remus_core::Schema;
use romulus_migration::{Format, Meta, Migration, Options, files};
use serde::{Deserialize as _, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::mpsc;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    ApiError, ApiResult, AppState,
    auth::{Member, min},
    compile, databases,
    domain::{PgMajor, SourceKind, VersionOrigin},
    error::ErrorBody,
    extract::Path,
    versions::migration::stored_renames,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(database_export))
        .routes(routes!(org_export))
}

/// What the manifest says of the archive's format.
pub const FORMAT: &str = "romulus-export/1";

/// One database to put in the archive, under `folder`.
#[derive(Debug, Clone)]
struct Exported {
    id: Uuid,
    slug: String,
    name: String,
    description: String,
    pg_major: PgMajor,
    created_at: DateTime<Utc>,
    folder: String,
}

/// A ZIP writer whose output is handed to the response as it grows.
type Zip = zip::ZipWriter<zip::write::StreamWriter<Sink>>;

/// What the writer wrote and the body has not sent yet.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Sink {
    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The archive being written: files go in, bytes go out on `tx`.
struct Archive {
    zip: Zip,
    sink: Sink,
    tx: mpsc::Sender<Result<Bytes, std::io::Error>>,
}

#[derive(Debug, thiserror::Error)]
enum ExportError {
    #[error("the download was closed")]
    Closed,
    #[error("writing the archive")]
    Zip(#[from] zip::result::ZipError),
    #[error("writing the archive")]
    Io(#[from] std::io::Error),
    #[error("reading what to export")]
    Database(#[from] sqlx::Error),
    #[error("{0}")]
    Api(#[from] ApiError),
}

impl Archive {
    fn new(tx: mpsc::Sender<Result<Bytes, std::io::Error>>) -> Self {
        let sink = Sink::default();
        let zip = zip::ZipWriter::new_stream(sink.clone()).set_auto_large_file();
        Self { zip, sink, tx }
    }

    /// Adds a file, then sends what is written.
    async fn add(&mut self, path: &str, content: &[u8]) -> Result<(), ExportError> {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        self.zip.start_file(path, options)?;
        self.zip.write_all(content)?;
        self.flush().await
    }

    async fn flush(&mut self) -> Result<(), ExportError> {
        let bytes = self.sink.take();
        if bytes.is_empty() {
            return Ok(());
        }
        self.tx
            .send(Ok(Bytes::from(bytes)))
            .await
            .map_err(|_| ExportError::Closed)
    }

    async fn finish(self) -> Result<(), ExportError> {
        let Self { zip, sink, tx } = self;
        zip.finish()?;
        let rest = sink.take();
        if !rest.is_empty() {
            tx.send(Ok(Bytes::from(rest)))
                .await
                .map_err(|_| ExportError::Closed)?;
        }
        Ok(())
    }
}

/// A version as the export reads it, one at a time.
struct VersionRow {
    id: Uuid,
    number: i32,
    origin: VersionOrigin,
    source_kind: SourceKind,
    message: Option<String>,
    created_at: DateTime<Utc>,
    model: Value,
    source: String,
    migration_sql: Option<String>,
    author: Option<String>,
    layout: Value,
    renames: Value,
}

/// What the manifest says of a version.
#[derive(Debug, Serialize)]
struct VersionEntry {
    number: i32,
    id: Uuid,
    created_at: DateTime<Utc>,
    origin: VersionOrigin,
    message: Option<String>,
    author: Option<String>,
    files: Vec<String>,
}

#[derive(Debug, Serialize)]
struct MigrationEntry {
    from: i32,
    to: i32,
    up: String,
    down: String,
    /// The renames the up migration made (`ALTER … RENAME`), as chosen.
    renames: Value,
    /// Statements that delete data or can fail on existing rows.
    destructive: usize,
}

fn padded(number: i32) -> String {
    format!("v{number:04}")
}

/// The schema as SQL DDL: what was uploaded or published when it was SQL,
/// rendered from the model otherwise (as the version's page shows it).
fn ddl(row: &VersionRow, model: &Schema) -> String {
    match row.source_kind {
        SourceKind::Sql => row.source.clone(),
        SourceKind::RemusJson | SourceKind::Migration => compile::render_sql(model),
    }
}

/// The migration between two consecutive versions, up and down, as plain
/// SQL files.
fn migrations_between(
    database: &str,
    old: &Schema,
    new: &Schema,
    from: i32,
    to: i32,
    major: PgMajor,
    renames: romulus_migration::RenameChoices,
) -> (String, String, Migration) {
    let up = romulus_migration::migration(
        old,
        new,
        &Options {
            concurrently: false,
            major: Some(major.number()),
            renames,
        },
    );
    let down = romulus_migration::down(old, new, &up);
    let meta = |from: i32, to: i32| Meta {
        database: database.to_owned(),
        from: format!("v{from}"),
        to: format!("v{to}"),
        date: Utc::now().format("%Y-%m-%d").to_string(),
        version: format!("{to:04}"),
        name: format!("v{from}_to_v{to}"),
    };
    let text = |m: &Migration, from, to| {
        files(m, &Migration::default(), Format::Plain, &meta(from, to))
            .into_iter()
            .map(|f| f.content)
            .collect::<String>()
    };
    let up_sql = text(&up, from, to);
    let down_sql = text(&down, to, from);
    (up_sql, down_sql, up)
}

/// Writes one database's folder.
#[allow(clippy::too_many_lines, reason = "the folder, file after file")]
async fn write_database(
    archive: &mut Archive,
    db: &PgPool,
    hub: &crate::collab::Hub,
    org_slug: &str,
    database: &Exported,
) -> Result<(), ExportError> {
    let folder = &database.folder;
    let numbers = sqlx::query!(
        r#"SELECT number, removed_at IS NOT NULL AS "removed!" FROM versions
           WHERE database_id = $1 ORDER BY number"#,
        database.id,
    )
    .fetch_all(db)
    .await?;
    let removed: Vec<i32> = numbers
        .iter()
        .filter(|n| n.removed)
        .map(|n| n.number)
        .collect();
    let live: Vec<i32> = numbers
        .iter()
        .filter(|n| !n.removed)
        .map(|n| n.number)
        .collect();

    let mut entries = Vec::with_capacity(live.len());
    let mut migrations = Vec::new();
    let mut previous: Option<(i32, Schema)> = None;
    let label = format!("{org_slug}/{}", database.slug);
    for number in live {
        let row = sqlx::query_as!(
            VersionRow,
            r#"SELECT v.id, v.number, v.origin AS "origin: VersionOrigin",
                      v.source_kind AS "source_kind: SourceKind", v.message, v.created_at,
                      v.model, v.source, v.migration_sql,
                      coalesce(u.name, t.name) AS author, v.layout, v.renames
               FROM versions v
               LEFT JOIN users u ON u.id = v.created_by
               LEFT JOIN api_tokens t ON t.id = v.created_by_token
               WHERE v.database_id = $1 AND v.number = $2 AND v.removed_at IS NULL"#,
            database.id,
            number,
        )
        .fetch_optional(db)
        .await?;
        // Removed while the export ran: left out.
        let Some(row) = row else { continue };
        let renames = match &previous {
            Some((from, _)) => Some(stored_renames(db, database.id, *from, number).await?),
            None => None,
        };
        let prior = previous.take();
        let major = database.pg_major;
        let label = label.clone();
        // Rendering and planning a few hundred tables: off the async workers.
        let (row, model, written, migration) = tokio::task::spawn_blocking(move || {
            let model = Schema::deserialize(&row.model).map_err(ApiError::internal)?;
            let sql = ddl(&row, &model);
            let json = serde_json::to_vec_pretty(&row.model).map_err(ApiError::internal)?;
            let layout = serde_json::to_vec_pretty(
                &crate::versions::layout::VersionLayout::from_value(&row.layout),
            )
            .map_err(ApiError::internal)?;
            let mut written: Vec<(String, Vec<u8>)> = vec![
                (format!("{}.sql", padded(row.number)), sql.into_bytes()),
                (format!("{}.model.json", padded(row.number)), json),
                (format!("{}.layout.json", padded(row.number)), layout),
            ];
            if row
                .renames
                .as_object()
                .is_some_and(|renames| !renames.is_empty())
            {
                written.push((
                    format!("{}.renames.json", padded(row.number)),
                    serde_json::to_vec_pretty(&row.renames).map_err(ApiError::internal)?,
                ));
            }
            match row.source_kind {
                SourceKind::RemusJson => written.push((
                    format!("{}.source.json", padded(row.number)),
                    row.source.clone().into_bytes(),
                )),
                SourceKind::Migration => written.push((
                    format!("{}.source.sql", padded(row.number)),
                    row.source.clone().into_bytes(),
                )),
                SourceKind::Sql => {}
            }
            if let Some(sql) = &row.migration_sql {
                written.push((
                    format!("{}.written-migration.sql", padded(row.number)),
                    sql.clone().into_bytes(),
                ));
            }
            let migration = match (&prior, renames) {
                (Some((from, old)), Some(renames)) => {
                    let (up, down, planned) =
                        migrations_between(&label, old, &model, *from, row.number, major, renames);
                    Some((*from, up, down, planned))
                }
                _ => None,
            };
            Ok::<_, ApiError>((row, model, written, migration))
        })
        .await
        .map_err(ApiError::internal)??;

        let mut paths = Vec::new();
        for (name, content) in written {
            let path = format!("{folder}/versions/{name}");
            archive.add(&path, content.as_slice()).await?;
            paths.push(format!("versions/{name}"));
        }
        if let Some((from, up, down, planned)) = migration {
            let base = format!("migrations/{}-{}", padded(from), padded(row.number));
            archive
                .add(&format!("{folder}/{base}.up.sql"), up.as_bytes())
                .await?;
            archive
                .add(&format!("{folder}/{base}.down.sql"), down.as_bytes())
                .await?;
            migrations.push(MigrationEntry {
                from,
                to: row.number,
                up: format!("{base}.up.sql"),
                down: format!("{base}.down.sql"),
                renames: serde_json::to_value(&planned.applied).unwrap_or_default(),
                destructive: planned.destructive(),
            });
        }
        entries.push(VersionEntry {
            number: row.number,
            id: row.id,
            created_at: row.created_at,
            origin: row.origin,
            message: row.message,
            author: row.author,
            files: paths,
        });
        previous = Some((row.number, model));
    }

    let comments = comments(db, database.id).await?;
    archive
        .add(
            &format!("{folder}/comments.json"),
            &serde_json::to_vec_pretty(&comments).map_err(ApiError::internal)?,
        )
        .await?;
    let drafts = write_drafts(archive, db, hub, database).await?;
    let manifest = json!({
        "format": FORMAT,
        "exported_at": Utc::now(),
        "database": {
            "id": database.id,
            "slug": database.slug,
            "name": database.name,
            "description": database.description,
            "postgres": database.pg_major.number(),
            "created_at": database.created_at,
        },
        "versions": entries,
        "removed_versions": removed,
        "migrations": migrations,
        "comments": "comments.json",
        "drafts": drafts,
        "notes": [
            "versions/vNNNN.sql is the schema as SQL DDL; vNNNN.model.json is romulus's model of it (remus JSON), lossless.",
            "migrations/ hold the migration between consecutive versions as romulus plans it, renames as they were chosen; down migrations are best effort.",
            "Versions hidden by a plan's history window are included. Removed versions are not.",
            "drafts/ hold each open draft as it is now: its source text, layout, renames chosen and discussion."
        ],
    });
    archive
        .add(
            &format!("{folder}/manifest.json"),
            &serde_json::to_vec_pretty(&manifest).map_err(ApiError::internal)?,
        )
        .await?;
    Ok(())
}

/// Each open draft of `database` as it is now, read from its room (loaded if
/// needed): what the manifest says of them.
async fn write_drafts(
    archive: &mut Archive,
    db: &PgPool,
    hub: &crate::collab::Hub,
    database: &Exported,
) -> Result<Vec<Value>, ExportError> {
    let drafts = sqlx::query!(
        r#"SELECT d.id, d.name, d.mode AS "mode: crate::domain::DraftMode", d.created_at,
                  u.name AS "created_by?", b.number AS "base?"
           FROM drafts d
           LEFT JOIN users u ON u.id = d.created_by
           LEFT JOIN versions b ON b.id = d.base_version_id
           WHERE d.database_id = $1 AND d.status = 'open'
           ORDER BY d.created_at, d.id"#,
        database.id,
    )
    .fetch_all(db)
    .await?;
    let mut listed = Vec::with_capacity(drafts.len());
    for (n, draft) in drafts.into_iter().enumerate() {
        let dir = format!(
            "drafts/{:02}-{}",
            n + 1,
            romulus_migration::slug(&draft.name)
        );
        let mut entry = json!({
            "id": draft.id,
            "name": draft.name,
            "mode": draft.mode,
            "base_version": draft.base,
            "created_by": draft.created_by,
            "created_at": draft.created_at,
            "folder": dir,
        });
        let content = match hub.content(draft.id).await {
            Ok(content) => content,
            Err(err) => {
                tracing::warn!(draft = %draft.id, error = %err, "export: a draft could not be read");
                entry["error"] = json!("This draft could not be read: export again in a moment.");
                listed.push(entry);
                continue;
            }
        };
        let folder = &database.folder;
        let mut files = vec![("schema.sql".to_owned(), content.sql.clone().into_bytes())];
        if content.mode == crate::domain::DraftMode::Migration {
            files.push((
                "migration.sql".to_owned(),
                content.migration.clone().into_bytes(),
            ));
        }
        files.push((
            "layout.json".to_owned(),
            serde_json::to_vec_pretty(&content.layout).map_err(ApiError::internal)?,
        ));
        if !content.renames.is_empty() {
            files.push((
                "renames.json".to_owned(),
                serde_json::to_vec_pretty(&content.renames).map_err(ApiError::internal)?,
            ));
        }
        let discussion: Vec<Value> = content
            .discussion
            .iter()
            .map(|thread| {
                json!({
                    "thread": thread.id,
                    "anchor": thread.thread,
                    "comments": thread.comments.iter().map(|(id, comment)| {
                        let mut value = serde_json::to_value(comment).unwrap_or_default();
                        value["id"] = json!(id);
                        value
                    }).collect::<Vec<_>>(),
                })
            })
            .collect();
        files.push((
            "comments.json".to_owned(),
            serde_json::to_vec_pretty(&discussion).map_err(ApiError::internal)?,
        ));
        let mut names = Vec::with_capacity(files.len());
        for (name, bytes) in files {
            archive
                .add(&format!("{folder}/{dir}/{name}"), bytes.as_slice())
                .await?;
            names.push(format!("{dir}/{name}"));
        }
        entry["files"] = json!(names);
        listed.push(entry);
    }
    Ok(listed)
}

/// Every version's threads with their comments, as JSON.
async fn comments(db: &PgPool, database: Uuid) -> Result<Vec<Value>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT v.number, t.id, t.anchor, t.frozen, t.created_at, t.resolved_at, t.resolved_by_name,
                  coalesce(
                      (SELECT jsonb_agg(jsonb_build_object(
                                  'id', c.id, 'author', c.author_name, 'body', c.body,
                                  'created_at', c.created_at, 'edited_at', c.edited_at)
                              ORDER BY c.created_at)
                       FROM version_comments c WHERE c.thread_id = t.id),
                      '[]'::jsonb
                  ) AS "comments!"
           FROM version_threads t JOIN versions v ON v.id = t.version_id
           WHERE v.database_id = $1 AND v.removed_at IS NULL
           ORDER BY v.number, t.created_at"#,
        database,
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            json!({
                "version": r.number,
                "thread": r.id,
                "anchor": r.anchor,
                "from_review": r.frozen,
                "created_at": r.created_at,
                "resolved_at": r.resolved_at,
                "resolved_by": r.resolved_by_name,
                "comments": r.comments,
            })
        })
        .collect())
}

/// A file name for `Content-Disposition`: letters, digits, `-`, `_`, `.`.
fn file_name(parts: &[&str]) -> String {
    let date = Utc::now().format("%Y%m%d");
    let name: String = parts
        .join("-")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{name}-export-{date}.zip")
}

/// The ZIP of `databases`, streamed; written by a task of its own (the
/// body ends early, and the task stops, if the download is closed).
fn stream(
    state: &AppState,
    org_slug: String,
    org: Option<Value>,
    databases: Vec<Exported>,
    name: &str,
    slot: crate::rate::Slot,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let db = state.db.clone();
    let hub = state.collab.clone();
    tokio::spawn(async move {
        // Held while the archive is written.
        let _slot = slot;
        let errors = tx.clone();
        let mut archive = Archive::new(tx);
        let written = async {
            if let Some(org) = &org {
                let manifest = json!({
                    "format": FORMAT,
                    "exported_at": Utc::now(),
                    "organization": org,
                    "databases": databases.iter().map(|d| json!({
                        "slug": d.slug, "name": d.name, "folder": d.folder,
                    })).collect::<Vec<_>>(),
                });
                archive
                    .add(
                        "manifest.json",
                        &serde_json::to_vec_pretty(&manifest).map_err(ApiError::internal)?,
                    )
                    .await?;
            }
            for database in &databases {
                write_database(&mut archive, &db, &hub, &org_slug, database).await?;
            }
            archive.finish().await
        }
        .await;
        match written {
            Ok(()) | Err(ExportError::Closed) => {}
            Err(err) => {
                tracing::warn!(error = %crate::error::Chain(&err), "export: failed mid-way");
                // The client sees a broken download, not a ZIP missing files.
                let _ = errors
                    .send(Err(std::io::Error::other("the export failed")))
                    .await;
            }
        }
    });
    let body = Body::from_stream(tokio_stream(rx));
    (
        [
            (header::CONTENT_TYPE, "application/zip".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        body,
    )
        .into_response()
}

fn tokio_stream(
    rx: mpsc::Receiver<Result<Bytes, std::io::Error>>,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    futures_util::stream::unfold(rx, |mut rx| async move {
        let item = rx.recv().await?;
        Some((item, rx))
    })
    .fuse()
}

/// Within the export limits: per person, per organization, and a slot
/// among those streaming (429 `rate_limited` otherwise).
fn admit(state: &AppState, user: Uuid, org: Uuid) -> ApiResult<crate::rate::Slot> {
    state.limits.exports.check(user)?;
    state.limits.org_exports.check(org)?;
    state.limits.export_slots.take(org)
}

/// The organization's live databases, folder names unique.
async fn databases_of(db: &PgPool, org: Uuid, only: Option<Uuid>) -> ApiResult<Vec<Exported>> {
    let rows = sqlx::query!(
        r#"SELECT id, slug, name, description, pg_major AS "pg_major: PgMajor", created_at
           FROM databases WHERE org_id = $1 AND deleted_at IS NULL AND ($2::uuid IS NULL OR id = $2)
           ORDER BY slug"#,
        org,
        only,
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Exported {
            folder: r.slug.clone(),
            id: r.id,
            slug: r.slug,
            name: r.name,
            description: r.description,
            pg_major: r.pg_major,
            created_at: r.created_at,
        })
        .collect())
}

/// Every version of the database as SQL, the migrations between them,
/// their comments and a manifest, in a ZIP streamed as it is written. Any
/// member; on every plan, versions hidden by the plan's history included.
#[utoipa::path(
    get,
    path = "/orgs/{org}/databases/{db}/export",
    operation_id = "export_database",
    tag = "export",
    params(
        ("org" = String, Path, description = "The organization's slug"),
        ("db" = String, Path, description = "The database's slug"),
    ),
    responses(
        (status = 200, description = "A ZIP: `<db>/manifest.json`, `versions/`, `migrations/`, `comments.json`, `drafts/`", content_type = "application/zip", body = Vec<u8>),
        (status = 404, body = ErrorBody),
        (status = 429, description = "`rate_limited`: too many exports for now", body = ErrorBody),
    )
)]
async fn database_export(
    member: Member,
    State(state): State<AppState>,
    Path((_, slug)): Path<(String, String)>,
) -> ApiResult<Response> {
    let database = databases::find(&state.db, member.org.id, &slug).await?;
    let slot = admit(&state, member.user.id, member.org.id)?;
    let list = databases_of(&state.db, member.org.id, Some(database.id)).await?;
    tracing::info!(org = %member.org.slug, database = %slug, "export: a database");
    Ok(stream(
        &state,
        member.org.slug.clone(),
        None,
        list,
        &file_name(&[&member.org.slug, &slug]),
        slot,
    ))
}

/// Every live database of the organization, a folder each (as the database
/// export lays it out), and a manifest. Admins only.
#[utoipa::path(
    get,
    path = "/orgs/{org}/export",
    operation_id = "export_org",
    tag = "export",
    params(("org" = String, Path, description = "The organization's slug")),
    responses(
        (status = 200, description = "A ZIP: `manifest.json`, then a folder per database", content_type = "application/zip", body = Vec<u8>),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 429, description = "`rate_limited`: too many exports for now", body = ErrorBody),
    )
)]
async fn org_export(
    member: Member<min::Admin>,
    State(state): State<AppState>,
) -> ApiResult<Response> {
    let slot = admit(&state, member.user.id, member.org.id)?;
    let list = databases_of(&state.db, member.org.id, None).await?;
    let org = json!({
        "id": member.org.id,
        "slug": member.org.slug,
        "name": member.org.name,
    });
    tracing::info!(org = %member.org.slug, databases = list.len(), "export: an organization");
    Ok(stream(
        &state,
        member.org.slug.clone(),
        Some(org),
        list,
        &file_name(&[&member.org.slug]),
        slot,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_safe_for_a_header() {
        let name = file_name(&["acme", "billing \"db\""]);
        assert!(name.starts_with("acme-billing--db--export-"), "{name}");
        assert!(
            std::path::Path::new(&name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
        );
        assert!(!name.contains('"'));
    }

    #[test]
    fn versions_sort_as_files() {
        assert_eq!(padded(7), "v0007");
        assert_eq!(padded(12_345), "v12345");
    }
}
