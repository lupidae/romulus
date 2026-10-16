//! Running an import: the replay in the sandbox, its progress, the
//! versions stored, the report.
//!
//! The job runs in a task of its own: a client that goes away (a closed
//! tab, a CI runner cancelled) does not leave it half done, it finishes and
//! stores (importing again then creates nothing). Its progress goes to the
//! client as NDJSON when asked for (`Accept: application/x-ndjson`).

use std::{
    convert::Infallible,
    sync::{Arc, Mutex, PoisonError},
    time::Instant,
};

use axum::{
    body::{Body, Bytes},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use remus_core::Schema;
use romulus_sandbox::{ReplayEvent, import::Migration};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    IgnoredFile, ImportOptions, SkippedStatement,
    plan::{self, ImportPreview, MigrationStatus, Planned},
};
use crate::{
    ApiError, ApiResult, AppState,
    domain::MigrationFormat,
    error::ErrorBody,
    extract::Json,
    versions::store::{ImportedVersion, NewImport, store_import},
};

/// Who imports.
#[derive(Debug, Clone, Copy)]
pub enum Author {
    User(Uuid),
    Token(Uuid),
}

/// How an import ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportStatus {
    /// Versions were created (201).
    Created,
    /// Every migration was imported already: nothing to do (200).
    Unchanged,
    /// A dry run: everything replayed, nothing stored (200).
    Checked,
}

/// One migration in the report.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportedMigration {
    pub id: String,
    pub name: String,
    pub path: String,
    /// `new` (created by this import, or would be in a dry run) or
    /// `imported` (already, untouched).
    pub status: MigrationStatus,
    /// Its version number (`null` in a dry run).
    pub version: Option<i32>,
    /// What it changed, against the version before it: `"2 tables added"`.
    pub changes: Option<String>,
    /// What was not replayed.
    pub skipped: Vec<SkippedStatement>,
}

/// What an import did.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportReport {
    pub status: ImportStatus,
    pub format: MigrationFormat,
    /// Every migration of the files, in order.
    pub migrations: Vec<ImportedMigration>,
    pub ignored: Vec<IgnoredFile>,
    /// Versions created.
    pub created: usize,
    pub duration_ms: u64,
}

/// One line of an import's progress (`Accept: application/x-ndjson`).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ImportEvent {
    /// The files, planned: what will be replayed.
    Planned { preview: ImportPreview },
    /// Every migration ran once in the sandbox: they replay; versions come
    /// next, one per migration.
    Checked,
    /// A migration's model was read.
    Replayed {
        /// Its position in `preview.migrations`.
        index: usize,
        /// The version number it will get.
        number: i32,
        /// What it changes: `"2 tables added"`.
        changes: String,
    },
    /// The import finished (the last line).
    Done { report: ImportReport },
    /// The import stopped, nothing stored (the last line): 422
    /// `import_failed` (`details`: `ImportFailure`), 409, 503…
    Failed { error: ErrorBody },
}

/// Plan, refuse what cannot be imported, then run (see the module
/// documentation).
pub async fn start(
    state: AppState,
    database_id: Uuid,
    slug: String,
    author: Author,
    files: Vec<romulus_sandbox::import::SourceFile>,
    options: ImportOptions,
    stream: bool,
) -> ApiResult<Response> {
    let started = Instant::now();
    let planned = plan::plan(&state.db, database_id, files, &options).await?;
    if let Some(refusal) = plan::refusal(&planned, &options) {
        return Err(refusal);
    }
    let job = Job {
        state,
        database_id,
        slug,
        author,
        options,
        planned,
        started,
    };
    if stream {
        let (events, received) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let _ = job.run(Some(events)).await;
        });
        let body = futures_util::stream::unfold(received, |mut received| async move {
            let event = received.recv().await?;
            let mut line = serde_json::to_vec(&event).unwrap_or_default();
            line.push(b'\n');
            Some((Ok::<_, Infallible>(Bytes::from(line)), received))
        });
        return Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/x-ndjson"),
                (header::CACHE_CONTROL, "no-cache"),
                // Proxies must pass lines on as they come.
                (header::HeaderName::from_static("x-accel-buffering"), "no"),
            ],
            Body::from_stream(body),
        )
            .into_response());
    }
    let report = tokio::spawn(job.run(None))
        .await
        .map_err(ApiError::internal)??;
    let status = if report.status == ImportStatus::Created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(report)).into_response())
}

struct Job {
    state: AppState,
    database_id: Uuid,
    slug: String,
    author: Author,
    options: ImportOptions,
    planned: Planned,
    started: Instant,
}

type Events = mpsc::UnboundedSender<ImportEvent>;

impl Job {
    /// The job, start to end. Its outcome also ends the stream.
    async fn run(self, events: Option<Events>) -> ApiResult<ImportReport> {
        if let Some(events) = &events {
            let _ = events.send(ImportEvent::Planned {
                preview: self.planned.preview.clone(),
            });
        }
        let outcome = self.import(events.as_ref()).await;
        if let Some(events) = &events {
            let _ = events.send(match &outcome {
                Ok(report) => ImportEvent::Done {
                    report: report.clone(),
                },
                Err(err) => ImportEvent::Failed { error: err.body() },
            });
        }
        if let Err(err) = &outcome {
            tracing::info!(database = %self.database_id, code = err.code(), "import refused");
        }
        outcome
    }

    async fn import(&self, events: Option<&Events>) -> ApiResult<ImportReport> {
        let planned = &self.planned;
        if planned.new.is_empty() {
            return Ok(self.report(ImportStatus::Unchanged, &[], &[], &[]));
        }
        let new: Vec<&Migration> = planned
            .new
            .iter()
            .map(|&k| &planned.migrations[k])
            .collect();
        let mut texts: Vec<String> = planned.history.iter().map(|e| e.source.clone()).collect();
        let from = texts.len();
        texts.extend(new.iter().map(|m| m.sql.clone()));

        // Models arrive as they are read; each is compared with the one
        // before it (the latest version for the first).
        let first_number = planned.preview.first_number.unwrap_or(1);
        let previous = planned
            .latest
            .as_ref()
            .and_then(|(_, model)| Schema::deserialize(model).ok());
        let progress = Arc::new(Mutex::new(Progress {
            previous,
            models: Vec::with_capacity(new.len()),
            changes: Vec::with_capacity(new.len()),
        }));
        let sink = Arc::clone(&progress);
        let sender = events.cloned();
        let indexes = planned.new.clone();
        let slug = self.slug.clone();
        let replay = self
            .state
            .compiler
            .replay(texts, from, move |event| {
                let mut progress = sink.lock().unwrap_or_else(PoisonError::into_inner);
                match event {
                    ReplayEvent::Checked => {
                        if let Some(sender) = &sender {
                            let _ = sender.send(ImportEvent::Checked);
                        }
                    }
                    ReplayEvent::Replayed { index, model } => {
                        let mut model = *model;
                        slug.clone_into(&mut model.database);
                        let k = index - from;
                        let changes = changes(progress.previous.as_ref(), &model);
                        if let Some(sender) = &sender {
                            let _ = sender.send(ImportEvent::Replayed {
                                index: indexes[k],
                                number: first_number + i32::try_from(k).unwrap_or(i32::MAX),
                                changes: changes.clone(),
                            });
                        }
                        progress.previous = Some(model.clone());
                        progress.models.push(model);
                        progress.changes.push(changes);
                    }
                }
            })
            .await?;

        let skipped: Vec<Vec<SkippedStatement>> = new
            .iter()
            .enumerate()
            .map(|(k, m)| {
                replay.skipped[from + k]
                    .iter()
                    .map(|s| plan::skipped_in_file(s, m))
                    .collect()
            })
            .collect();
        if let Some(failure) = &replay.failure {
            // A failure in a migration imported before cannot happen (it
            // replayed then); name the first new one if it does.
            let k = failure.index.saturating_sub(from).min(new.len() - 1);
            let migration = new[k];
            let errors = failure
                .diagnostics
                .iter()
                .filter(|d| d.is_error())
                .map(|d| plan::located(d, migration))
                .collect();
            let index = planned.new[k];
            return Err(plan::failed(
                index,
                &planned.preview.migrations[index],
                errors,
            ));
        }
        let Progress {
            models, changes, ..
        } = std::mem::take(&mut *progress.lock().unwrap_or_else(PoisonError::into_inner));
        if models.len() != new.len() {
            return Err(ApiError::Internal(
                format!(
                    "the replay read {} models for {} migrations",
                    models.len(),
                    new.len()
                )
                .into(),
            ));
        }
        if self.options.dry_run {
            return Ok(self.report(ImportStatus::Checked, &[], &changes, &skipped));
        }

        let (created_by, created_by_token) = match self.author {
            Author::User(id) => (Some(id), None),
            Author::Token(id) => (None, Some(id)),
        };
        let versions = new
            .iter()
            .zip(models)
            .zip(&skipped)
            .map(|((migration, model), skipped)| ImportedVersion {
                model,
                source: migration.sql.clone(),
                message: migration.name.clone(),
                migration_id: migration.id.clone(),
                path: migration.path.clone(),
                checksum: plan::checksum(&migration.sql),
                skipped: serde_json::to_value(skipped).unwrap_or_default(),
            })
            .collect();
        let stored = store_import(
            &self.state.db,
            NewImport {
                database_id: self.database_id,
                format: planned.format,
                created_by,
                created_by_token,
                append: self.options.append,
                imported_before: planned.history.len(),
                versions,
            },
        )
        .await?;
        if let Some((_, last)) = stored.last() {
            // Open drafts of the database learn their base moved.
            self.state.collab.version_created(self.database_id, *last);
        }
        let numbers: Vec<i32> = stored.iter().map(|(_, n)| *n).collect();
        Ok(self.report(ImportStatus::Created, &numbers, &changes, &skipped))
    }

    /// The report: every migration of the files, the new ones with what
    /// this import made of them.
    fn report(
        &self,
        status: ImportStatus,
        numbers: &[i32],
        changes: &[String],
        skipped: &[Vec<SkippedStatement>],
    ) -> ImportReport {
        let preview = &self.planned.preview;
        let migrations = preview
            .migrations
            .iter()
            .enumerate()
            .map(|(index, m)| {
                let k = self.planned.new.iter().position(|&n| n == index);
                ImportedMigration {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    path: m.path.clone(),
                    status: m.status,
                    version: match k {
                        Some(k) => numbers.get(k).copied(),
                        None => m.version,
                    },
                    changes: k.and_then(|k| changes.get(k).cloned()),
                    skipped: k
                        .and_then(|k| skipped.get(k).cloned())
                        .unwrap_or_else(|| m.skipped.clone()),
                }
            })
            .collect();
        ImportReport {
            status,
            format: preview.format,
            migrations,
            ignored: preview.ignored.clone(),
            created: numbers.len(),
            duration_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }
}

#[derive(Default)]
struct Progress {
    previous: Option<Schema>,
    models: Vec<Schema>,
    changes: Vec<String>,
}

/// `"2 tables added, 1 changed"`, against the model before (nothing for
/// the first version).
fn changes(previous: Option<&Schema>, model: &Schema) -> String {
    let empty;
    let previous = match previous {
        Some(previous) => previous,
        None => {
            empty = crate::versions::empty(model);
            &empty
        }
    };
    romulus_diff::diff(previous, model).summary.text
}
